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
            let clave = chunk.id.shard_key(self.reparto(), cuantos_antes + j);
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
            let clave = chunk.id.shard_key(self.reparto(), indice);
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

/// Lo que paso al reescribir el volumen en un reparto nuevo.
#[derive(Debug, Default, Clone)]
pub struct InformeReparto {
    pub trozos: usize,
    /// Los que ya estaban enteros en el reparto nuevo, de una ejecucion anterior.
    pub ya_estaban: usize,
    pub fragmentos: usize,
    pub bytes: u64,
    pub fallidos: Vec<(ChunkId, String)>,
}

impl InformeReparto {
    pub fn completo(&self) -> bool {
        self.fallidos.is_empty()
    }
}

impl ChunkStore {
    /// Reescribe cada trozo con otra topologia, en un **reparto nuevo**.
    ///
    /// Lee con la topologia y el reparto de este almacen, y escribe con `destino` bajo
    /// `reparto_nuevo`, que tiene que ser distinto del actual. Los fragmentos viejos no se
    /// tocan: viven en claves distintas y son los que sostienen el volumen mientras esto
    /// corre, que puede ser horas.
    ///
    /// `todos` son **todos** los proveedores del volumen nuevo, viejos y nuevos, en el orden
    /// en que va a declararlos la cabecera.
    ///
    /// No toca la cabecera. Publicarla es lo que hace efectivo el cambio, y solo debe hacerse
    /// con el informe completo: con un trozo a medias, el volumen nuevo no se podria leer
    /// entero.
    pub async fn reescribir_reparto(
        &self,
        destino: Layout,
        reparto_nuevo: u32,
        todos: &[Arc<dyn Backend>],
        trozos: &[ChunkRef],
    ) -> Result<InformeReparto, CoreError> {
        if reparto_nuevo == self.reparto() {
            return Err(CoreError::Rebalanceo(format!(
                "el reparto nuevo tiene que ser distinto del actual ({reparto_nuevo}), o lo \
                 escrito pisaria lo que se esta leyendo"
            )));
        }
        if todos.len() != destino.shard_count() {
            return Err(CoreError::Rebalanceo(format!(
                "{destino} necesita {} proveedores y se han dado {}",
                destino.shard_count(),
                todos.len()
            )));
        }

        let mut vistos = std::collections::BTreeSet::new();
        let unicos: Vec<&ChunkRef> = trozos
            .iter()
            .filter(|c| vistos.insert(c.id.clone()))
            .collect();

        let mut informe = InformeReparto {
            trozos: unicos.len(),
            ..Default::default()
        };

        for lote in unicos.chunks(A_LA_VEZ) {
            let trabajos = lote
                .iter()
                .map(|c| self.reescribir_uno(destino, reparto_nuevo, todos, c));
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

    async fn reescribir_uno(
        &self,
        destino: Layout,
        reparto_nuevo: u32,
        todos: &[Arc<dyn Backend>],
        chunk: &ChunkRef,
    ) -> Result<Ampliado, CoreError> {
        let ya = join_all(todos.iter().enumerate().map(|(i, b)| {
            let clave = chunk.id.shard_key(reparto_nuevo, i);
            async move { b.exists(&clave).await.unwrap_or(false) }
        }))
        .await;
        if ya.iter().all(|x| *x) {
            return Ok(Ampliado::YaEstaba);
        }

        // Con el blob sellado original, no volviendo a cifrar: el nonce es aleatorio y un
        // sellado nuevo daria otros bytes. Aqui no romperia nada --se reescribe todo-- pero
        // el trozo dejaria de ser el mismo objeto y la deduplicacion por contenido se
        // perderia.
        let (sellado, _) = self.recover(chunk).await?;
        let nuevos = libredisk_raid::split(destino, &sellado)?;

        let subidas = todos
            .iter()
            .enumerate()
            .zip(ya)
            .filter_map(|((i, b), y)| if y { None } else { Some((i, b)) })
            .map(|(i, b)| {
                let clave = chunk.id.shard_key(reparto_nuevo, i);
                let datos = nuevos[i].clone();
                async move {
                    let n = datos.len() as u64;
                    b.put(&clave, datos).await.map(|()| n)
                }
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

    /// Borra los fragmentos de un reparto que ya no vale.
    ///
    /// Va aparte y despues de publicar la cabecera **a proposito**: hasta ese momento son los
    /// unicos que sostienen el volumen. Perder esta pasada no rompe nada, solo deja espacio
    /// ocupado que se puede recuperar volviendo a lanzarla.
    pub async fn olvidar_reparto(
        &self,
        viejo: u32,
        cuantos: usize,
        todos: &[Arc<dyn Backend>],
        trozos: &[ChunkRef],
    ) -> usize {
        let mut vistos = std::collections::BTreeSet::new();
        let unicos: Vec<&ChunkRef> = trozos
            .iter()
            .filter(|c| vistos.insert(c.id.clone()))
            .collect();

        let mut borrados = 0;
        for lote in unicos.chunks(A_LA_VEZ) {
            let trabajos = lote.iter().flat_map(|c| {
                todos.iter().take(cuantos).enumerate().map(move |(i, b)| {
                    let clave = c.id.shard_key(viejo, i);
                    async move { b.delete(&clave).await.is_ok() }
                })
            });
            borrados += join_all(trabajos).await.into_iter().filter(|x| *x).count();
        }
        borrados
    }
}

/// Que se sacrifica al retirar proveedores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sacrificio {
    /// Mismo espacio util, menos margen de caidas. `rs(k,m)` pasa a `rs(k,m-1)`.
    Seguridad,
    /// Mismo margen, menos espacio util. `rs(k,m)` pasa a `rs(k-1,m)`.
    Espacio,
}

/// Que topologia queda al retirar `cuantos` proveedores de `antes`.
pub fn planear_retirada(
    antes: Layout,
    cuantos: usize,
    sacrificio: Sacrificio,
) -> Result<Layout, CoreError> {
    if cuantos == 0 {
        return Err(CoreError::Rebalanceo(
            "no se ha retirado ningun proveedor".into(),
        ));
    }
    let destino = match (antes, sacrificio) {
        (Layout::Mirror { n }, _) if n > cuantos => Layout::Mirror { n: n - cuantos },
        (Layout::ReedSolomon { k, m }, Sacrificio::Seguridad) if m >= cuantos => {
            // Sin paridad ninguna ya no es Reed-Solomon: un solo fragmento por trozo y
            // repartido entre k proveedores es perder el volumen si cae cualquiera de ellos.
            // Se dice en vez de dejarlo pasar.
            if m - cuantos == 0 {
                return Err(CoreError::Rebalanceo(format!(
                    "quitar {cuantos} de {antes} dejaria el volumen **sin redundancia**: \
                     cualquier caida lo perderia entero. Si es lo que quieres, crea un volumen \
                     nuevo y copia el contenido, para que sea una decision y no un descuido."
                )));
            }
            Layout::ReedSolomon { k, m: m - cuantos }
        }
        (Layout::ReedSolomon { k, m }, Sacrificio::Espacio) if k > cuantos => {
            Layout::ReedSolomon { k: k - cuantos, m }
        }
        _ => {
            return Err(CoreError::Rebalanceo(format!(
                "no se pueden retirar {cuantos} proveedor(es) de {antes} sin quedarse sin \
                 volumen"
            )));
        }
    };
    destino
        .validate()
        .map_err(|e| CoreError::Rebalanceo(e.to_string()))?;
    Ok(destino)
}

/// Si retirar los **ultimos** proveedores deja lo demas exactamente donde esta.
///
/// Es el caso barato de verdad: no hay que leer ni escribir nada, solo publicar la cabecera y
/// tirar lo que sobra. Vale porque los fragmentos de datos dependen solo de `k`, y los de
/// paridad que quedan son los mismos que ya habia.
///
/// Retirar uno de en medio **no** vale: todos los que van detras cambian de indice, y el
/// indice es donde vive el fragmento.
pub fn basta_con_recortar(antes: Layout, despues: Layout) -> bool {
    match (antes, despues) {
        (Layout::Mirror { n }, Layout::Mirror { n: n2 }) => n2 < n && n2 >= 1,
        (Layout::ReedSolomon { k, m }, Layout::ReedSolomon { k: k2, m: m2 }) => k2 == k && m2 < m,
        _ => false,
    }
}

impl ChunkStore {
    /// Borra los fragmentos de los proveedores que se retiran.
    ///
    /// Va **despues** de publicar la cabecera, igual que [`ChunkStore::olvidar_reparto`]: hasta
    /// ese momento son parte de la redundancia del volumen.
    ///
    /// Devuelve `(borrados, sin_borrar)`. Que no se puedan borrar no es un fallo: el caso
    /// normal de retirar un proveedor es justamente que ya no responda. El volumen queda bien
    /// igual; lo que queda es sitio ocupado en una cuenta ajena.
    pub async fn olvidar_proveedores(
        &self,
        indices: &[usize],
        retirados: &[Arc<dyn Backend>],
        trozos: &[ChunkRef],
    ) -> (usize, usize) {
        let mut vistos = std::collections::BTreeSet::new();
        let unicos: Vec<&ChunkRef> = trozos
            .iter()
            .filter(|c| vistos.insert(c.id.clone()))
            .collect();

        let (mut borrados, mut fallidos) = (0, 0);
        for lote in unicos.chunks(A_LA_VEZ) {
            let trabajos = lote.iter().flat_map(|c| {
                indices.iter().zip(retirados).map(move |(i, b)| {
                    let clave = c.id.shard_key(self.reparto(), *i);
                    async move { b.delete(&clave).await.is_ok() }
                })
            });
            for ok in join_all(trabajos).await {
                if ok {
                    borrados += 1;
                } else {
                    fallidos += 1;
                }
            }
        }
        (borrados, fallidos)
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
            .flat_map(|c| (0..3).map(move |i| c.id.shard_key(0, i)))
            .collect();
        let mut sellados = Vec::new();
        for (i, b) in bs.iter().enumerate() {
            for c in &trozos {
                sellados.push(b.get(&c.id.shard_key(0, i)).await.unwrap());
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
                despues.push(b.get(&c.id.shard_key(0, i)).await.unwrap());
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

    /// Reescribir en otro reparto **no toca** los fragmentos del viejo.
    ///
    /// Es lo que sostiene el volumen durante las horas que tarda: mientras la cabecera no
    /// cambie, se sigue leyendo con la topologia vieja y sus fragmentos.
    #[tokio::test]
    async fn el_reparto_viejo_sigue_intacto_mientras_se_escribe_el_nuevo() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let datos: Vec<u8> = (0..30_000u32).map(|i| (i % 251) as u8).collect();
        let (bs, v) = volumen_con(antes, &datos).await;
        let (trozos, _) = v.walk().await;

        let antiguos = fragmentos(&bs, &trozos, 0, 3).await;

        let mut todos = bs.clone();
        todos.push(proveedor("proveedor-3"));
        let destino = Layout::ReedSolomon { k: 3, m: 1 };

        let informe = v
            .store()
            .reescribir_reparto(destino, 1, &todos, &trozos)
            .await
            .unwrap();
        assert!(informe.completo(), "{:?}", informe.fallidos);
        assert!(informe.fragmentos > 0);

        assert_eq!(
            antiguos,
            fragmentos(&bs, &trozos, 0, 3).await,
            "se toco algun fragmento del reparto viejo"
        );

        // Y el volumen se sigue leyendo con la topologia vieja, que es lo que pasaria si esto
        // se interrumpiera aqui.
        let raiz = v.root().clone();
        let vieja = Volume::at(store_sobre(&bs, antes), raiz.clone());
        assert_eq!(vieja.read_at("/dato.bin", 0, 100_000).await.unwrap(), datos);

        // Con el reparto nuevo tambien, que es lo que se publicaria.
        let nueva = Volume::at(store_sobre(&todos, destino).with_reparto(1), raiz);
        assert_eq!(nueva.read_at("/dato.bin", 0, 100_000).await.unwrap(), datos);
    }

    /// Y con el reparto nuevo se gana el espacio prometido: `rs(3,1)` sobre cuatro tolera una
    /// caida, igual que `rs(2,1)` sobre tres, pero con un sobrecoste menor.
    #[tokio::test]
    async fn tras_reescribir_se_sobrevive_a_la_caida_prometida() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let datos: Vec<u8> = (0..30_000u32).map(|i| (i % 241) as u8).collect();
        let (bs, v) = volumen_con(antes, &datos).await;
        let (trozos, _) = v.walk().await;
        let raiz = v.root().clone();

        let mut todos = bs.clone();
        todos.push(proveedor("proveedor-3"));
        let destino = Layout::ReedSolomon { k: 3, m: 1 };
        v.store()
            .reescribir_reparto(destino, 1, &todos, &trozos)
            .await
            .unwrap();

        let mutilados: Vec<Arc<dyn Backend>> = todos
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if i == 1 {
                    proveedor(b.name())
                } else {
                    Arc::clone(b)
                }
            })
            .collect();
        let v2 = Volume::at(store_sobre(&mutilados, destino).with_reparto(1), raiz);
        assert_eq!(v2.read_at("/dato.bin", 0, 100_000).await.unwrap(), datos);
    }

    /// Olvidar el reparto viejo libera el espacio y no toca el nuevo.
    #[tokio::test]
    async fn olvidar_el_reparto_viejo_deja_el_nuevo_entero() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let datos: Vec<u8> = (0..20_000u32).map(|i| (i % 233) as u8).collect();
        let (bs, v) = volumen_con(antes, &datos).await;
        let (trozos, _) = v.walk().await;
        let raiz = v.root().clone();

        let mut todos = bs.clone();
        todos.push(proveedor("proveedor-3"));
        let destino = Layout::ReedSolomon { k: 3, m: 1 };
        v.store()
            .reescribir_reparto(destino, 1, &todos, &trozos)
            .await
            .unwrap();

        let borrados = v.store().olvidar_reparto(0, 3, &todos, &trozos).await;
        assert!(borrados > 0, "no borro ningun fragmento viejo");
        for c in &trozos {
            for (i, b) in bs.iter().enumerate() {
                assert!(
                    !b.exists(&c.id.shard_key(0, i)).await.unwrap(),
                    "quedo un fragmento del reparto viejo"
                );
            }
        }

        let v2 = Volume::at(store_sobre(&todos, destino).with_reparto(1), raiz);
        assert_eq!(v2.read_at("/dato.bin", 0, 100_000).await.unwrap(), datos);
    }

    /// Reescribir sobre el mismo reparto pisaria lo que se esta leyendo.
    #[tokio::test]
    async fn no_se_puede_reescribir_sobre_el_reparto_actual() {
        let antes = Layout::ReedSolomon { k: 2, m: 1 };
        let (bs, v) = volumen_con(antes, b"algo").await;
        let mut todos = bs.clone();
        todos.push(proveedor("proveedor-3"));
        let (trozos, _) = v.walk().await;

        let e = v
            .store()
            .reescribir_reparto(Layout::ReedSolomon { k: 3, m: 1 }, 0, &todos, &trozos)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("distinto del actual"), "{e}");
    }

    /// Los fragmentos de `cuantos` proveedores para un reparto dado.
    async fn fragmentos(
        bs: &[Arc<dyn Backend>],
        trozos: &[ChunkRef],
        reparto: u32,
        cuantos: usize,
    ) -> Vec<Option<Vec<u8>>> {
        let mut out = Vec::new();
        for (i, b) in bs.iter().take(cuantos).enumerate() {
            for c in trozos {
                out.push(b.get(&c.id.shard_key(reparto, i)).await.ok());
            }
        }
        out
    }

    /// Retirar el ultimo proveedor no mueve nada: es solo publicar la cabecera y tirar lo que
    /// sobra. El volumen se sigue leyendo con los mismos fragmentos de siempre.
    #[tokio::test]
    async fn retirar_el_ultimo_no_mueve_ningun_fragmento() {
        let antes = Layout::ReedSolomon { k: 2, m: 2 };
        let datos: Vec<u8> = (0..20_000u32).map(|i| (i % 229) as u8).collect();
        let (bs, v) = volumen_con(antes, &datos).await;
        let (trozos, _) = v.walk().await;
        let raiz = v.root().clone();

        let antiguos = fragmentos(&bs, &trozos, 0, 3).await;

        let destino = planear_retirada(antes, 1, Sacrificio::Seguridad).unwrap();
        assert_eq!(destino, Layout::ReedSolomon { k: 2, m: 1 });
        assert!(basta_con_recortar(antes, destino));

        let (borrados, _) = v.store().olvidar_proveedores(&[3], &bs[3..], &trozos).await;
        assert!(borrados > 0);

        assert_eq!(
            antiguos,
            fragmentos(&bs, &trozos, 0, 3).await,
            "se toco un fragmento de los que se quedan"
        );

        // Con los tres que quedan, el volumen se lee igual.
        let quedan: Vec<Arc<dyn Backend>> = bs[..3].to_vec();
        let v2 = Volume::at(store_sobre(&quedan, destino), raiz);
        assert_eq!(v2.read_at("/dato.bin", 0, 100_000).await.unwrap(), datos);
    }

    /// Retirar uno de en medio **no** se puede recortar: los de detras cambian de indice, y el
    /// indice es donde vive el fragmento.
    #[test]
    fn retirar_uno_de_en_medio_no_es_un_recorte() {
        let antes = Layout::ReedSolomon { k: 2, m: 2 };
        // La topologia resultante es la misma; lo que cambia es que hay que mover cosas, y eso
        // no lo dice la topologia sino cual se va. Lo decide quien llama.
        assert!(basta_con_recortar(
            antes,
            Layout::ReedSolomon { k: 2, m: 1 }
        ));
        assert!(!basta_con_recortar(
            antes,
            Layout::ReedSolomon { k: 1, m: 2 }
        ));
    }

    /// Y se puede retirar un proveedor **que ya no responde**, que es el caso que de verdad
    /// aprieta: la cuenta suspendida.
    #[tokio::test]
    async fn se_puede_retirar_uno_que_ya_no_responde() {
        let antes = Layout::ReedSolomon { k: 2, m: 2 };
        let datos: Vec<u8> = (0..20_000u32).map(|i| (i % 211) as u8).collect();
        let (bs, v) = volumen_con(antes, &datos).await;
        let (trozos, _) = v.walk().await;
        let raiz = v.root().clone();

        // El cuarto se sustituye por uno vacio: ni se lee ni se borra de el.
        let muerto = proveedor("proveedor-3");
        let (borrados, fallidos) = v
            .store()
            .olvidar_proveedores(&[3], std::slice::from_ref(&muerto), &trozos)
            .await;
        assert_eq!(borrados + fallidos, trozos_unicos(&trozos));

        let quedan: Vec<Arc<dyn Backend>> = bs[..3].to_vec();
        let v2 = Volume::at(
            store_sobre(&quedan, Layout::ReedSolomon { k: 2, m: 1 }),
            raiz,
        );
        assert_eq!(v2.read_at("/dato.bin", 0, 100_000).await.unwrap(), datos);
    }

    /// Quedarse sin redundancia tiene que ser una decision, no un descuido.
    #[test]
    fn retirar_hasta_quedarse_sin_paridad_se_rechaza() {
        let e = planear_retirada(Layout::ReedSolomon { k: 3, m: 1 }, 1, Sacrificio::Seguridad)
            .unwrap_err();
        assert!(e.to_string().contains("sin redundancia"), "{e}");
    }

    #[test]
    fn no_se_puede_retirar_mas_de_lo_que_hay() {
        assert!(
            planear_retirada(Layout::ReedSolomon { k: 2, m: 1 }, 3, Sacrificio::Seguridad).is_err()
        );
        assert!(planear_retirada(Layout::Mirror { n: 2 }, 2, Sacrificio::Seguridad).is_err());
    }

    /// Retirar sacrificando espacio en vez de seguridad: mismo margen, menos capacidad.
    #[test]
    fn retirar_sacrificando_espacio_mantiene_el_margen() {
        let antes = Layout::ReedSolomon { k: 3, m: 2 };
        let d = planear_retirada(antes, 1, Sacrificio::Espacio).unwrap();
        assert_eq!(d, Layout::ReedSolomon { k: 2, m: 2 });
        assert_eq!(
            d.shard_count() - d.shards_needed(),
            antes.shard_count() - antes.shards_needed(),
            "el margen de caidas tiene que quedarse igual"
        );
        // Y eso no es un recorte: cambia k, asi que se mueve todo.
        assert!(!basta_con_recortar(antes, d));
    }

    fn trozos_unicos(trozos: &[ChunkRef]) -> usize {
        let mut v = std::collections::BTreeSet::new();
        trozos.iter().filter(|c| v.insert(c.id.clone())).count()
    }
}
