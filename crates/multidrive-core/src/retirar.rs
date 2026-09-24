//! Retirar un volumen de los proveedores.
//!
//! Vive aqui y no en cada interfaz porque lo usan las dos, y porque **es irreversible**: dos
//! copias de un borrado acaban discrepando en que borran, y la que discrepa se lleva algo por
//! delante.
//!
//! Dos reglas, y las dos existen por la misma razon:
//!
//! 1. **Solo se borra lo que es del volumen**: el prefijo de fragmentos, el puntero de raiz y
//!    la cabecera. Si el usuario guarda otras cosas en esa carpeta —cosa que la documentacion
//!    desaconseja pero nadie impide— se quedan donde estan. Borrar todo lo que hubiera debajo
//!    seria mas facil de escribir y mucho peor de explicar.
//! 2. **Si no se puede mirar entero, no se toca nada.** Un proveedor que no se deja enumerar
//!    no se puede limpiar, y decir «hecho» dejando fragmentos detras seria mentir sobre algo
//!    que no tiene vuelta atras.
//!
//! El orden tambien importa: los fragmentos primero y **la cabecera la ultima**. Si esto se
//! corta a la mitad, un volumen sin fragmentos pero con cabecera se diagnostica —`scrub` dira
//! que falta todo— mientras que fragmentos sin cabecera son basura anonima que ya nadie sabe
//! de quien era.

use std::sync::Arc;

use futures::future::join_all;
use multidrive_backend::Backend;

use crate::{CoreError, SHARD_PREFIX};

/// Lo que hay de un volumen en cada proveedor.
#[derive(Debug, Clone, Default)]
pub struct Inventario {
    /// Nombre, cuantos fragmentos y cuantos bytes, por proveedor y en su orden.
    pub por_proveedor: Vec<(String, usize, u64)>,
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
    Ok(Inventario {
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
) -> Result<Vec<(String, Vec<multidrive_backend::Objeto>)>, CoreError> {
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

    let mut hecho = Vec::with_capacity(backends.len());
    for (b, (nombre, objetos)) in backends.iter().zip(listados) {
        let bytes = objetos.iter().map(|o| o.bytes).sum();
        for o in &objetos {
            b.delete(&o.clave).await?;
        }
        // La raiz y la cabecera, las ultimas.
        b.delete(crate::tree::ROOT_KEY).await?;
        b.delete(crate::header::HEADER_KEY).await?;
        hecho.push((nombre, objetos.len(), bytes));
    }

    Ok(Inventario {
        por_proveedor: hecho,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{clave, memoria};
    use multidrive_backend::OpendalBackend;

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
