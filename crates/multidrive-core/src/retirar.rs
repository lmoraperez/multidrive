//! Retirar un volumen de los proveedores.
//!
//! Vive aqui y no en cada interfaz porque lo usan las dos, y porque **es irreversible**: dos
//! copias de un borrado acaban discrepando en que borran, y la que discrepa se lleva algo por
//! delante.
//!
//! Dos reglas, y las dos existen por la misma razon:
//!
//! 1. **Solo se borra lo que es del volumen**: el prefijo de fragmentos, el puntero de raiz,
//!    la cabecera y la maleta. Si el usuario guarda otras cosas en esa carpeta —cosa que la
//!    documentacion desaconseja pero nadie impide— se quedan donde estan. Borrar todo lo que
//!    hubiera debajo seria mas facil de escribir y mucho peor de explicar.
//!
//!    La maleta entra en la lista porque la puso MultiDrive y nadie mas. Dejarla despues de
//!    «eliminar el volumen» significaba que la cuenta no quedaba como estaba antes: quien
//!    borraba para no dejar rastro se dejaba atras un fichero sellado que ni siquiera sabia
//!    que existia. Se cuenta y se ensena **antes** de confirmar, porque borrar de mas sin
//!    avisar es tan malo como borrar de menos.
//! 2. **Si no se puede mirar entero, no se toca nada.** Un proveedor que no se deja enumerar
//!    no se puede limpiar, y decir «hecho» dejando fragmentos detras seria mentir sobre algo
//!    que no tiene vuelta atras.
//!
//! El orden tambien importa: los fragmentos primero y **la cabecera la ultima**. Si esto se
//! corta a la mitad, un volumen sin fragmentos pero con cabecera se diagnostica —`scrub` dira
//! que falta todo— mientras que fragmentos sin cabecera son basura anonima que ya nadie sabe
//! de quien era.
//!
//! La maleta va **detras de la cabecera**, por lo mismo visto del otro lado: mientras el
//! volumen exista, su maleta es el camino para volver a montarlo en otro equipo. Quitarla
//! primero y morir a mitad dejaria un volumen vivo al que se le ha retirado la salida de
//! emergencia.

use std::sync::Arc;

use futures::future::join_all;
use multidrive_backend_api::Backend;

use crate::{CoreError, SHARD_PREFIX, maleta};

/// A cual de los proveedores hay que nombrar para confirmar el borrado. Indice desde 0.
///
/// Se pide **un** nombre y no la lista entera. La lista cumplia su funcion --obliga a mirar lo
/// que se va a borrar-- pero crece con el numero de cuentas, y a partir de unas pocas lo que
/// consigue es que se copie y se pegue sin leerla, que es exactamente lo que queria evitar.
/// Pedir «el tercero de la lista» obliga a contar, y contar obliga a mirar.
///
/// Sale de la sal de la cabecera y no de un sorteo, por una razon practica: en el terminal la
/// orden se ejecuta **dos veces** --la primera ensena que se borraria y dice que escribir, la
/// segunda lo confirma-- y un numero distinto en cada ejecucion haria imposible acertar.
/// Atado a la sal, es el mismo mientras sea el mismo volumen, y distinto en otro.
///
/// No es una medida de seguridad y no tiene que ser impredecible: quien quiera borrar su
/// volumen puede, esto solo esta para que no se borre sin querer.
pub fn a_quien_nombrar(salt: &[u8], cuantos: usize) -> usize {
    if cuantos == 0 {
        return 0;
    }
    // Varios bytes y no solo el primero: dos volumenes creados a la vez comparten mas de lo
    // que parece, y aqui no cuesta nada mirar la sal entera.
    let suma: u64 = salt.iter().map(|b| *b as u64).sum();
    (suma % cuantos as u64) as usize
}

/// Lo que hay de un volumen en cada proveedor.
#[derive(Debug, Clone, Default)]
pub struct Inventario {
    /// Nombre, cuantos fragmentos y cuantos bytes, por proveedor y en su orden.
    pub por_proveedor: Vec<(String, usize, u64)>,
    /// Proveedores que ademas guardan la configuracion sellada.
    ///
    /// Va aparte de la cuenta de fragmentos porque no es un fragmento: no ocupa nada que
    /// merezca la pena sumar, pero es lo unico de la lista que **sigue sirviendo para algo**
    /// despues de borrar el volumen. Quien confirma tiene que verlo nombrado.
    pub con_maleta: Vec<String>,
}

impl Inventario {
    pub fn fragmentos(&self) -> usize {
        self.por_proveedor.iter().map(|(_, n, _)| n).sum()
    }

    pub fn bytes(&self) -> u64 {
        self.por_proveedor.iter().map(|(_, _, b)| b).sum()
    }
}

/// Cuenta lo que hay, sin tocar nada.
///
/// Es lo que convierte «se va a borrar el volumen» en una cifra que se puede mirar antes de
/// decir que si. Cuesta una enumeracion por proveedor.
///
/// Falla si alguno no se deja enumerar, porque entonces el recuento seria falso y no se puede
/// pedir una confirmacion sobre una cifra que no es cierta.
pub async fn inventariar(backends: &[Arc<dyn Backend>]) -> Result<Inventario, CoreError> {
    let listados = enumerar(backends).await?;
    let con_maleta = donde_hay_maleta(backends).await;
    Ok(Inventario {
        con_maleta,
        por_proveedor: listados
            .into_iter()
            .map(|(nombre, objetos)| {
                let bytes = objetos.iter().map(|o| o.bytes).sum();
                (nombre, objetos.len(), bytes)
            })
            .collect(),
    })
}

/// Enumera todos, y **falla si alguno no se deja**.
///
/// Se junta todo antes de devolver nada: quien borra necesita saber que puede mirarlo entero
/// antes de tocar el primero.
async fn enumerar(
    backends: &[Arc<dyn Backend>],
) -> Result<Vec<(String, Vec<multidrive_backend_api::Objeto>)>, CoreError> {
    let listados = join_all(
        backends
            .iter()
            .map(|b| async move { (b.name().to_string(), b.list(SHARD_PREFIX).await) }),
    )
    .await;

    let mut out = Vec::with_capacity(backends.len());
    for (nombre, r) in listados {
        match r {
            Ok(objetos) => out.push((nombre, objetos)),
            Err(e) => {
                return Err(CoreError::NoSeDejaEnumerar {
                    proveedor: nombre,
                    motivo: e.to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// Borra los fragmentos, el puntero de raiz y la cabecera. **No se puede deshacer.**
///
/// Devuelve lo que habia, para poder decir cuanto se retiro.
///
/// Enumera **una sola vez**: se mira todo primero y se borra despues. Enumerar otra vez para
/// borrar duplicaria el paso caro --contra una nube, minutos-- y no aportaria nada, porque el
/// volumen tiene un solo escritor.
pub async fn retirar(backends: &[Arc<dyn Backend>]) -> Result<Inventario, CoreError> {
    let listados = enumerar(backends).await?;

    let con_maleta = donde_hay_maleta(backends).await;

    let mut hecho = Vec::with_capacity(backends.len());
    for (b, (nombre, objetos)) in backends.iter().zip(listados) {
        let bytes = objetos.iter().map(|o| o.bytes).sum();
        for o in &objetos {
            b.delete(&o.clave).await?;
        }
        // La raiz y la cabecera, las ultimas. Y la maleta, detras de la cabecera: mientras el
        // volumen exista, es el camino para volver a montarlo en otro equipo.
        b.delete(crate::tree::ROOT_KEY).await?;
        b.delete(crate::header::HEADER_KEY).await?;
        b.delete(maleta::CLAVE).await?;
        hecho.push((nombre, objetos.len(), bytes));
    }

    Ok(Inventario {
        por_proveedor: hecho,
        con_maleta,
    })
}

/// En cuales de estos proveedores hay configuracion sellada.
///
/// No falla: que no se pueda preguntar por ella no es motivo para no borrar el volumen, y lo
/// que se hace con la respuesta es ensenarla. Un proveedor que no contesta sale como que no
/// la tiene, y el borrado de mas abajo la quita igual si estaba.
async fn donde_hay_maleta(backends: &[Arc<dyn Backend>]) -> Vec<String> {
    let hay = join_all(
        backends
            .iter()
            .map(|b| async move { b.exists(maleta::CLAVE).await.unwrap_or(false) }),
    )
    .await;
    backends
        .iter()
        .zip(hay)
        .filter(|(_, hay)| *hay)
        .map(|(b, _)| b.name().to_string())
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{clave, memoria};
    use multidrive_backend::OpendalBackend;

    /// El que hay que nombrar es **el mismo en cada llamada**.
    ///
    /// Es lo que hace que la orden del terminal funcione: se ejecuta dos veces, la primera
    /// dice que escribir y la segunda lo comprueba. Con un sorteo en cada ejecucion no se
    /// podria acertar nunca.
    #[test]
    fn a_quien_nombrar_no_cambia_entre_llamadas() {
        let sal = b"una sal cualquiera";
        let primero = a_quien_nombrar(sal, 4);
        for _ in 0..100 {
            assert_eq!(a_quien_nombrar(sal, 4), primero);
        }
        assert!(primero < 4, "tiene que caer dentro de la lista");
    }

    /// Y cae dentro de la lista con cualquier numero de proveedores, incluido ninguno.
    ///
    /// Lo de «ninguno» no es teorico: un volumen cuyos proveedores no esten configurados llega
    /// aqui con la lista vacia, y devolver un indice fuera seria un panico al ir a leerlo.
    #[test]
    fn a_quien_nombrar_siempre_cae_dentro() {
        for cuantos in 0..40usize {
            for sal in [&b""[..], &b"x"[..], &[0u8; 32][..], &[255u8; 32][..]] {
                let i = a_quien_nombrar(sal, cuantos);
                assert!(
                    cuantos == 0 || i < cuantos,
                    "con {cuantos} proveedores salio {i}"
                );
            }
        }
    }

    /// Dos volumenes distintos no tienen por que pedir el mismo, que es la gracia.
    #[test]
    fn dos_sales_distintas_suelen_pedir_otro() {
        let distintos = (0u8..60)
            .map(|n| a_quien_nombrar(&[n], 4))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            distintos.len() > 1,
            "si siempre saliera el mismo, se aprenderia de memoria"
        );
    }

    /// La maleta se va con el volumen, y se nombra antes de borrarla.
    ///
    /// Se comprueban las dos mitades a la vez a proposito: que `inventariar` la ensene sin
    /// tocarla --es lo que se lee al confirmar-- y que `retirar` la quite. Que se borre algo
    /// que no aparecio en la confirmacion seria peor que no borrarlo.
    #[tokio::test]
    async fn la_maleta_se_va_con_el_volumen_y_se_dice_antes() {
        let backends = memoria(2);
        let s = crate::ChunkStore::new(
            multidrive_raid::Layout::Mirror { n: 2 },
            backends.clone(),
            clave(),
        )
        .unwrap();
        let (mut v, _) = crate::tree::Volume::create(s).await.unwrap();
        v.write_file("/algo.txt", &b"hola"[..]).await.unwrap();
        let bulto = crate::maleta::empaquetar(
            &[("MULTIDRIVE_FS_ROOT".to_string(), "/uno".to_string())]
                .into_iter()
                .collect(),
            "la-frase",
        )
        .unwrap();
        for (nombre, fallo) in crate::maleta::guardar(&backends, &bulto).await {
            assert!(fallo.is_none(), "{nombre}: {fallo:?}");
        }

        let antes = inventariar(&backends).await.unwrap();
        assert_eq!(
            antes.con_maleta,
            vec!["p0".to_string(), "p1".to_string()],
            "la confirmacion tiene que nombrar donde esta la maleta"
        );
        assert!(
            backends[0].exists(maleta::CLAVE).await.unwrap(),
            "inventariar no puede borrar nada"
        );

        let hecho = retirar(&backends).await.unwrap();
        assert_eq!(hecho.con_maleta.len(), 2, "y decir de donde se quito");
        for b in &backends {
            assert!(
                !b.exists(maleta::CLAVE).await.unwrap(),
                "{}: la maleta sigue ahi despues de retirar el volumen",
                b.name()
            );
        }
    }

    /// Retirar un volumen que nunca tuvo maleta no es un caso especial.
    #[tokio::test]
    async fn sin_maleta_no_pasa_nada() {
        let backends = memoria(2);
        let s = crate::ChunkStore::new(
            multidrive_raid::Layout::Mirror { n: 2 },
            backends.clone(),
            clave(),
        )
        .unwrap();
        let (mut v, _) = crate::tree::Volume::create(s).await.unwrap();
        v.write_file("/algo.txt", &b"hola"[..]).await.unwrap();

        assert!(inventariar(&backends).await.unwrap().con_maleta.is_empty());
        let hecho = retirar(&backends).await.unwrap();
        assert!(hecho.con_maleta.is_empty());
        assert!(hecho.fragmentos() > 0);
    }

    /// Lo que no es del volumen sobrevive.
    ///
    /// Es la regla que separa «retirar un volumen» de «vaciar una carpeta», y la que hace que
    /// esta orden se pueda ejecutar sin miedo sobre una cuenta donde haya algo mas.
    #[tokio::test]
    async fn solo_se_borra_lo_que_es_del_volumen() {
        let backends = memoria(2);
        let s = crate::ChunkStore::new(
            multidrive_raid::Layout::Mirror { n: 2 },
            backends.clone(),
            clave(),
        )
        .unwrap();
        let (mut v, _) = crate::tree::Volume::create(s).await.unwrap();
        v.write_file("/algo.txt", &b"hola"[..]).await.unwrap();

        // Algo que no es del volumen, en el mismo proveedor.
        backends[0]
            .put("mis-notas.txt", b"esto no es del volumen".to_vec())
            .await
            .unwrap();

        let antes = inventariar(&backends).await.unwrap();
        assert!(antes.fragmentos() > 0);

        let hecho = retirar(&backends).await.unwrap();
        assert_eq!(hecho.fragmentos(), antes.fragmentos());
        assert_eq!(inventariar(&backends).await.unwrap().fragmentos(), 0);

        assert_eq!(
            backends[0].get("mis-notas.txt").await.unwrap(),
            b"esto no es del volumen"
        );
    }

    /// Y despues no queda cabecera, que es lo que hace que el volumen deje de existir.
    #[tokio::test]
    async fn despues_no_hay_volumen() {
        let backends = memoria(2);
        let s = crate::ChunkStore::new(
            multidrive_raid::Layout::Mirror { n: 2 },
            backends.clone(),
            clave(),
        )
        .unwrap();
        crate::tree::Volume::create(s).await.unwrap();
        let h = crate::header::Header {
            version: 1,
            layout: "mirror:2".into(),
            reparto: 0,
            salt: vec![1, 2, 3],
            backends: vec!["p0".into(), "p1".into()],
            pesos: Vec::new(),
        };
        crate::header::write(&backends, &h).await.unwrap();
        assert!(crate::header::read(&backends).await.unwrap().is_some());

        retirar(&backends).await.unwrap();
        assert!(crate::header::read(&backends).await.unwrap().is_none());
    }

    /// Si uno no se deja enumerar, no se toca nada.
    ///
    /// Decir «hecho» dejando fragmentos detras seria mentir sobre algo que no tiene vuelta
    /// atras, y ademas esos fragmentos ya no serian identificables: sin cabecera, nadie sabe
    /// de quien eran.
    #[tokio::test]
    async fn si_no_se_puede_mirar_entero_no_se_borra_nada() {
        let backends = memoria(1);
        let s = crate::ChunkStore::new(multidrive_raid::Layout::Concat, backends.clone(), clave())
            .unwrap();
        let (mut v, _) = crate::tree::Volume::create(s).await.unwrap();
        v.write_file("/algo.txt", &b"hola"[..]).await.unwrap();

        let mudo: Vec<Arc<dyn Backend>> = vec![
            backends[0].clone(),
            Arc::new(crate::testutil::NoEnumera(
                OpendalBackend::memory("mudo").unwrap(),
            )),
        ];
        let e = retirar(&mudo).await.unwrap_err().to_string();
        assert!(e.contains("mudo"), "{e}");

        // Y lo del proveedor bueno sigue ahi.
        assert!(inventariar(&backends).await.unwrap().fragmentos() > 0);
    }
}
