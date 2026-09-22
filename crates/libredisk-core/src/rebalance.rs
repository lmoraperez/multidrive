//! Cambiar la topologia de un volumen sin dejar de poder leerlo.
//!
//! Anadir un proveedor no es una cosa, son dos, y cuestan ordenes de magnitud distintos:
//!
//! - **Mas seguridad.** `rs(k,m)` pasa a `rs(k,m+1)`: el mismo espacio util, una caida mas de
//!   margen. Los fragmentos de datos **no se mueven**, porque dependen solo de `k` y del blob
//!   sellado. Solo hay que calcular la paridad nueva y subirla.
//! - **Mas espacio.** `rs(k,m)` pasa a `rs(k+1,m)`: mas capacidad, el mismo margen. Cambian
//!   las fronteras del reparto, asi que **todos** los fragmentos de **todos** los trozos hay
//!   que recalcularlos y volver a subirlos. A las velocidades medidas contra nubes de
//!   consumo, un volumen de 1 TB son dias.
//!
//! Aqui esta implementada la primera. La segunda necesita ademas un esquema de dos
//! generaciones de claves —para poder escribir lo nuevo sin destruir lo viejo mientras el
//! volumen se sigue leyendo— y va aparte.
//!
//! # Por que no se vuelve a cifrar
//!
//! Los fragmentos son trozos del blob **sellado**, y sellar usa un nonce aleatorio: volver a
//! cifrar el mismo contenido da bytes distintos, y la paridad nueva no casaria con los
//! fragmentos que ya estan. El blob sellado original se recupera de los propios fragmentos,
//! que es lo mismo que hace `repair` y por el mismo motivo.
//!
//! # Por que se puede interrumpir
//!
//! Mientras la cabecera no cambie, el volumen se lee con la topologia vieja y los fragmentos
//! viejos, que no se han tocado. Lo unico que hay a medias son fragmentos de paridad de mas,
//! que nadie lee todavia. Volver a lanzarlo se salta los que ya estan: **la cabecera es el
//! unico punto de compromiso**.

use std::sync::Arc;

use futures::future::join_all;
use libredisk_backend::Backend;
use libredisk_raid::Layout;

use crate::{ChunkId, ChunkRef, ChunkStore, CoreError};

/// Cuantos trozos se procesan a la vez.
///
/// Cada uno son varias peticiones, asi que el paralelismo util lo pone la red y no la CPU.
/// Ocho es lo que uso `scrub` para lo mismo.
const A_LA_VEZ: usize = 8;

/// Que se quiere conseguir al anadir proveedores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cambio {
    /// Mismo espacio util, mas tolerancia a caidas.
    MasSeguridad,
    /// Mas espacio util, la misma tolerancia.
    MasEspacio,
}

/// Que topologia sale de anadir `nuevos` proveedores a `antes`.
///
/// No toca nada: sirve para poder ensenar el resultado y su precio **antes** de empezar algo
/// que puede tardar horas.
pub fn planear(antes: Layout, nuevos: usize, cambio: Cambio) -> Result<Layout, CoreError> {
    if nuevos == 0 {
        return Err(CoreError::Rebalanceo(
            "no se ha anadido ningun proveedor".into(),
        ));
    }
    let destino = match (antes, cambio) {
        // Con espejo la distincion no existe: una copia mas es mas seguridad, y el espacio
        // util no cambia nunca porque siempre es el de un proveedor.
        (Layout::Mirror { n }, _) => Layout::Mirror { n: n + nuevos },

        (Layout::ReedSolomon { k, m }, Cambio::MasSeguridad) => {
            Layout::ReedSolomon { k, m: m + nuevos }
        }
        (Layout::ReedSolomon { k, m }, Cambio::MasEspacio) => {
            Layout::ReedSolomon { k: k + nuevos, m }
        }

        // Sin redundancia no hay nada que ampliar, y convertir un `concat` en otra cosa es
        // reescribir el volumen entero: no es esta operacion.
        (Layout::Concat, _) => {
            return Err(CoreError::Rebalanceo(
                "un volumen `concat` no tiene redundancia que ampliar. Crea uno nuevo con la \
                 topologia que quieras y copia el contenido."
                    .into(),
            ));
        }
    };
    destino
        .validate()
        .map_err(|e| CoreError::Rebalanceo(e.to_string()))?;
    Ok(destino)
}

/// Si pasar de `antes` a `despues` conserva los fragmentos que ya estan.
///
/// Es lo que separa la operacion barata de la cara, y lo que decide si se puede hacer sin
/// dejar el volumen ilegible un rato.
pub fn conserva_lo_escrito(antes: Layout, despues: Layout) -> bool {
    match (antes, despues) {
        (Layout::Mirror { n }, Layout::Mirror { n: n2 }) => n2 >= n,
        (Layout::ReedSolomon { k, m }, Layout::ReedSolomon { k: k2, m: m2 }) => k2 == k && m2 >= m,
        _ => false,
    }
}

/// Lo que paso al ampliar.
#[derive(Debug, Default, Clone)]
pub struct Informe {
    /// Trozos examinados.
    pub trozos: usize,
    /// Los que ya tenian todos los fragmentos nuevos, de una ejecucion anterior.
    pub ya_estaban: usize,
    /// Fragmentos subidos.
    pub fragmentos: usize,
    pub bytes: u64,
    /// Trozos que no se pudieron ampliar, y por que.
    pub fallidos: Vec<(ChunkId, String)>,
}

impl Informe {
    /// Si todo salio bien y por tanto se puede publicar la cabecera nueva.
    ///
    /// Con un solo fallo no se publica: la cabecera es la que promete la redundancia, y
    /// prometer una que no esta es peor que no ampliar.
    pub fn completo(&self) -> bool {
        self.fallidos.is_empty()
    }
}

impl ChunkStore {
    /// Amplia la redundancia escribiendo **solo** los fragmentos que faltan.
    ///
    /// Lee con la topologia actual del almacen y escribe los fragmentos nuevos en
    /// `nuevos`, en ese orden: el primero se lleva el indice que sigue al ultimo de ahora.
    ///
    /// No toca la cabecera. Publicarla es cosa de quien llama, y solo si el informe sale
    /// completo.
    pub async fn ampliar_redundancia(
        &self,
        destino: Layout,
        nuevos: &[Arc<dyn Backend>],
        trozos: &[ChunkRef],
    ) -> Result<Informe, CoreError> {
        let antes = self.layout();
        if !conserva_lo_escrito(antes, destino) {
            return Err(CoreError::Rebalanceo(format!(
                "pasar de {antes} a {destino} mueve los fragmentos que ya estan, asi que no \
                 puede hacerse asi: haria falta reescribir el volumen entero"
            )));
        }
        let cuantos_antes = antes.shard_count();
        let cuantos_despues = destino.shard_count();
        if cuantos_despues - cuantos_antes != nuevos.len() {
            return Err(CoreError::Rebalanceo(format!(
                "{destino} necesita {} proveedores mas y se han dado {}",
                cuantos_despues - cuantos_antes,
                nuevos.len()
            )));
        }

        let mut informe = Informe {
            trozos: trozos.len(),
            ..Default::default()
        };

        // Un trozo puede aparecer varias veces: la deduplicacion por contenido hace que dos
        // ficheros iguales compartan trozo, y el arbol repite nodos entre versiones.
        let mut vistos = std::collections::BTreeSet::new();
        let unicos: Vec<&ChunkRef> = trozos
            .iter()
            .filter(|c| vistos.insert(c.id.clone()))
            .collect();
        informe.trozos = unicos.len();

        for lote in unicos.chunks(A_LA_VEZ) {
            let trabajos = lote
                .iter()
                .map(|c| self.ampliar_uno(destino, cuantos_antes, nuevos, c));
            for (chunk, resultado) in lote.iter().zip(join_all(trabajos).await) {
                match resultado {
                    Ok(Ampliado::YaEstaba) => informe.ya_estaban += 1,
                    Ok(Ampliado::Escritos { cuantos, bytes }) => {
                        informe.fragmentos += cuantos;
                        informe.bytes += bytes;
                    }
                    Err(e) => informe.fallidos.push((chunk.id.clone(), e.to_string())),
                }
            }
        }
        Ok(informe)
    }

    async fn ampliar_uno(
        &self,
        destino: Layout,
        cuantos_antes: usize,
        nuevos: &[Arc<dyn Backend>],
        chunk: &ChunkRef,
    ) -> Result<Ampliado, CoreError> {
        // Volver a lanzarlo tras una interrupcion no debe repetir el trabajo hecho.
        let ya = join_all(nuevos.iter().enumerate().map(|(j, b)| {
            let clave = chunk.id.shard_key(cuantos_antes + j);
            async move { b.exists(&clave).await.unwrap_or(false) }
        }))
        .await;
        if ya.iter().all(|x| *x) {
            return Ok(Ampliado::YaEstaba);
        }

        // El blob **sellado**, reconstruido de los fragmentos que hay. Volver a cifrar daria
        // otros bytes y la paridad nueva no casaria con los fragmentos viejos.
        let (sellado, _) = self.recover(chunk).await?;
        let todos = libredisk_raid::split(destino, &sellado)?;

        // Los fragmentos que ya estan tienen que salir identicos de la topologia nueva. Lo
        // garantiza como `reed-solomon-erasure` construye su matriz, y hay un test que lo
        // fija; comprobarlo aqui ademas cuesta un cifrado por trozo y es lo que separa
        // "confiamos en la biblioteca" de "lo hemos mirado".
        let viejos = libredisk_raid::split(self.layout(), &sellado)?;
        if todos[..cuantos_antes] != viejos[..] {
            return Err(CoreError::Rebalanceo(format!(
                "ampliar a {destino} cambiaria los fragmentos que ya estan del trozo {}. \
                 No se ha escrito nada.",
                chunk.id
            )));
        }

        let subidas = nuevos.iter().enumerate().zip(ya).filter_map(|((j, b), y)| {
            if y {
                return None;
            }
            let indice = cuantos_antes + j;
            let clave = chunk.id.shard_key(indice);
            let datos = todos[indice].clone();
            Some(async move {
                let n = datos.len() as u64;
                b.put(&clave, datos).await.map(|()| n)
            })
        });

        let mut cuantos = 0;
        let mut bytes = 0;
        for r in join_all(subidas).await {
            match r {
                Ok(n) => {
                    cuantos += 1;
                    bytes += n;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Ampliado::Escritos { cuantos, bytes })
    }
}

enum Ampliado {
    YaEstaba,
    Escritos { cuantos: usize, bytes: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mas_seguridad_sube_la_paridad_y_mas_espacio_sube_los_datos() {
        let antes = Layout::ReedSolomon { k: 4, m: 2 };
        assert_eq!(
            planear(antes, 1, Cambio::MasSeguridad).unwrap(),
            Layout::ReedSolomon { k: 4, m: 3 }
        );
        assert_eq!(
            planear(antes, 1, Cambio::MasEspacio).unwrap(),
            Layout::ReedSolomon { k: 5, m: 2 }
        );
    }

    /// Con espejo las dos opciones son la misma cosa, y no tiene sentido fingir que no.
    #[test]
    fn en_espejo_las_dos_opciones_dan_lo_mismo() {
        let antes = Layout::Mirror { n: 2 };
        assert_eq!(
            planear(antes, 2, Cambio::MasSeguridad).unwrap(),
            Layout::Mirror { n: 4 }
        );
        assert_eq!(
            planear(antes, 2, Cambio::MasEspacio).unwrap(),
            planear(antes, 2, Cambio::MasSeguridad).unwrap()
        );
    }

    #[test]
    fn concat_no_tiene_redundancia_que_ampliar() {
        let e = planear(Layout::Concat, 1, Cambio::MasSeguridad).unwrap_err();
        assert!(e.to_string().contains("concat"), "{e}");
    }

    #[test]
    fn anadir_cero_proveedores_es_un_error_y_no_un_no_operacion() {
        assert!(planear(Layout::Mirror { n: 2 }, 0, Cambio::MasSeguridad).is_err());
    }

    /// La distincion que sostiene todo el modulo.
    #[test]
    fn solo_conserva_lo_escrito_si_k_no_cambia() {
        let a = Layout::ReedSolomon { k: 4, m: 2 };
        assert!(conserva_lo_escrito(a, Layout::ReedSolomon { k: 4, m: 3 }));
        assert!(!conserva_lo_escrito(a, Layout::ReedSolomon { k: 5, m: 2 }));
        // Reducir la paridad tampoco: quita fragmentos, no los conserva.
        assert!(!conserva_lo_escrito(a, Layout::ReedSolomon { k: 4, m: 1 }));
        assert!(conserva_lo_escrito(
            Layout::Mirror { n: 2 },
            Layout::Mirror { n: 3 }
        ));
        // Cambiar de familia nunca conserva nada.
        assert!(!conserva_lo_escrito(a, Layout::Mirror { n: 6 }));
    }
}

#[cfg(test)]
mod tests_ampliar {
    use std::sync::Arc;

    use libredisk_backend::{Backend, OpendalBackend};
    use libredisk_raid::Layout;

    use super::*;
    use crate::testutil::{backends, clave, store_sobre};
    use crate::tree::Volume;

    /// Un volumen con un fichero dentro, y los proveedores sobre los que vive.
    async fn volumen_con(layout: Layout, datos: &[u8]) -> (Vec<Arc<dyn Backend>>, Volume) {
        let bs = backends(layout.shard_count());
        let store = ChunkStore::new(layout, bs.clone(), clave()).unwrap();
        let (mut v, _) = Volume::create(store).await.unwrap();
        v.write_file("/dato.bin", datos).await.unwrap();
        (bs, v)
    }

    fn proveedor(nombre: &str) -> Arc<dyn Backend> {
        Arc::new(OpendalBackend::memory(nombre).unwrap()) as Arc<dyn Backend>
    }

    /// Leer con `n` proveedores destruidos.
    async fn leer_perdiendo(
        bs: &[Arc<dyn Backend>],
        layout: Layout,
        raiz: &crate::tree::Root,
        perdidos: &[usize],
    ) -> Result<Vec<u8>, CoreError> {
        let mutilados: Vec<Arc<dyn Backend>> = bs
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if perdidos.contains(&i) {
                    // Un proveedor vacio con el mismo nombre: el orden es parte del formato,
                    // asi que no se puede simplemente quitarlo de la lista.
                    proveedor(b.name())
                } else {
                    Arc::clone(b)
                }
            })
            .collect();
        let v = Volume::at(store_sobre(&mutilados, layout), raiz.clone());
        v.read_at("/dato.bin", 0, 100_000).await
    }

    /// La prueba que importa: tras ampliar, el volumen aguanta **una caida mas**.
    ///
    /// Y antes de ampliar no la aguantaba. Sin esa segunda mitad, el test pasaria igual si
    /// `ampliar_redundancia` no hiciera absolutamente nada.
    #[tokio::test]
    async fn ampliar_la_paridad_compra_una_caida_mas() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let datos: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let (bs, v) = volumen_con(antes, &datos).await;
        let raiz = v.root().clone();

        // Con rs(2,1), perder dos proveedores deja el volumen ilegible.
        assert!(
            leer_perdiendo(&bs, antes, &raiz, &[0, 1]).await.is_err(),
            "rs(2,1) no deberia sobrevivir a dos caidas"
        );

        let (trozos, ilegibles) = v.walk().await;
        assert!(ilegibles.is_empty(), "{ilegibles:?}");

        let nuevo = proveedor("proveedor-3");
        let destino = planear(antes, 1, Cambio::MasSeguridad).unwrap();
        let informe = v
            .store()
            .ampliar_redundancia(destino, std::slice::from_ref(&nuevo), &trozos)
            .await
            .unwrap();

        assert!(informe.completo(), "{:?}", informe.fallidos);
        assert!(informe.fragmentos > 0, "no escribio ningun fragmento");

        // Y ahora si: con rs(2,2) sobre cuatro, dos caidas se sobreviven.
        let mut todos = bs.clone();
        todos.push(nuevo);
        let leido = leer_perdiendo(&todos, destino, &raiz, &[0, 1])
            .await
            .unwrap();
        assert_eq!(leido, datos, "el contenido cambio al ampliar");
    }

    /// Los fragmentos viejos siguen exactamente donde estaban, byte a byte.
    ///
    /// Es lo que permite no reescribir el volumen, y lo que hace que se pueda interrumpir sin
    /// consecuencias. Si esto dejara de cumplirse, ampliar seria destructivo.
    #[tokio::test]
    async fn los_fragmentos_que_ya_estaban_no_se_tocan() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let (bs, v) = volumen_con(antes, b"contenido de prueba para repartir").await;
        let (trozos, _) = v.walk().await;

        let claves: Vec<String> = trozos
            .iter()
            .flat_map(|c| (0..3).map(move |i| c.id.shard_key(i)))
            .collect();
        let mut sellados = Vec::new();
        for (i, b) in bs.iter().enumerate() {
            for c in &trozos {
                sellados.push(b.get(&c.id.shard_key(i)).await.unwrap());
            }
        }

        let nuevo = proveedor("proveedor-3");
        v.store()
            .ampliar_redundancia(
                Layout::ReedSolomon { k: 2, m: 2 },
                std::slice::from_ref(&nuevo),
                &trozos,
            )
            .await
            .unwrap();

        let mut despues = Vec::new();
        for (i, b) in bs.iter().enumerate() {
            for c in &trozos {
                despues.push(b.get(&c.id.shard_key(i)).await.unwrap());
            }
        }
        assert_eq!(sellados, despues, "se toco algun fragmento que ya estaba");
        assert!(!claves.is_empty());
    }

    /// Volver a lanzarlo no repite el trabajo, que es lo que hace que se pueda interrumpir.
    #[tokio::test]
    async fn la_segunda_pasada_no_escribe_nada() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let (_, v) = volumen_con(antes, b"algo que repartir entre varios").await;
        let (trozos, _) = v.walk().await;
        let nuevo = proveedor("proveedor-3");
        let destino = Layout::ReedSolomon { k: 2, m: 2 };

        let primera = v
            .store()
            .ampliar_redundancia(destino, std::slice::from_ref(&nuevo), &trozos)
            .await
            .unwrap();
        assert!(primera.fragmentos > 0);
        assert_eq!(primera.ya_estaban, 0);

        let segunda = v
            .store()
            .ampliar_redundancia(destino, std::slice::from_ref(&nuevo), &trozos)
            .await
            .unwrap();
        assert_eq!(segunda.fragmentos, 0, "reescribio lo que ya estaba");
        assert_eq!(segunda.ya_estaban, segunda.trozos);
    }

    /// Pedir mas espacio por esta via tiene que fallar, no hacer algo a medias.
    #[tokio::test]
    async fn ampliar_no_sirve_para_cambiar_k() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let (_, v) = volumen_con(antes, b"x").await;
        let (trozos, _) = v.walk().await;
        let nuevo = proveedor("proveedor-3");

        let e = v
            .store()
            .ampliar_redundancia(
                Layout::ReedSolomon { k: 3, m: 1 },
                std::slice::from_ref(&nuevo),
                &trozos,
            )
            .await
            .unwrap_err();
        assert!(e.to_string().contains("reescribir"), "{e}");
    }

    /// Y dar mal el numero de proveedores tampoco escribe a medias.
    #[tokio::test]
    async fn el_numero_de_proveedores_tiene_que_cuadrar() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let (_, v) = volumen_con(antes, b"x").await;
        let (trozos, _) = v.walk().await;

        let e = v
            .store()
            .ampliar_redundancia(Layout::ReedSolomon { k: 2, m: 3 }, &[], &trozos)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("proveedores mas"), "{e}");
    }
}
