//! Almacen de trozos: la pieza donde confluyen cifrado, RAID y proveedores.
//!
//! Ciclo de vida de un trozo al escribirlo:
//!
//! ```text
//!   datos --> BLAKE3 --> id
//!         --> sellar (AAD = id) --> repartir (RAID) --> un fragmento por proveedor
//! ```
//!
//! Y al leerlo, el camino inverso, terminando siempre con una verificacion del hash: si el
//! dato recuperado no corresponde a su identificador, se rechaza en vez de devolverse.
//!
//! Los trozos son **inmutables** y su nombre es su contenido. Nada se sobrescribe jamas,
//! que es lo unico que funciona sobre almacenes de objetos sin renombrado atomico.

use std::sync::Arc;

use futures::future::join_all;
use libredisk_backend::{Backend, BackendError};
use libredisk_crypto::MasterKey;
use libredisk_raid::{Layout, RaidError};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("la topologia {layout} necesita {needed} proveedores y se configuraron {given}")]
    BackendCount {
        layout: Layout,
        needed: usize,
        given: usize,
    },
    #[error(
        "trozo {id} irrecuperable: solo respondieron {available} de {needed} fragmentos necesarios"
    )]
    Unrecoverable {
        id: ChunkId,
        available: usize,
        needed: usize,
    },
    #[error("trozo {id} corrupto: el contenido recuperado tiene hash {actual}, que no corresponde")]
    HashMismatch { id: ChunkId, actual: ChunkId },
    #[error(transparent)]
    Crypto(#[from] libredisk_crypto::CryptoError),
    #[error(transparent)]
    Raid(#[from] RaidError),
    #[error(transparent)]
    Backend(#[from] BackendError),
}

/// Identificador de un trozo: el BLAKE3 de su contenido en claro, en hexadecimal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkId(String);

impl ChunkId {
    /// Calcula el identificador de un contenido.
    pub fn of(data: &[u8]) -> Self {
        Self(blake3::hash(data).to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Clave del fragmento `index` de este trozo dentro de un proveedor.
    ///
    /// Se antepone un prefijo de dos caracteres del hash porque muchos almacenes degradan
    /// al acumular millones de objetos en un mismo "directorio".
    pub fn shard_key(&self, index: usize) -> String {
        format!("chunks/{}/{}.{index:03}", &self.0[..2], self.0)
    }
}

impl std::fmt::Display for ChunkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Todo lo necesario para volver a montar un trozo. Es lo que guardan los manifiestos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub id: ChunkId,
    /// Longitud del contenido en claro.
    pub plain_len: usize,
    /// Longitud tras sellarlo, que es lo que realmente se reparte en fragmentos.
    pub sealed_len: usize,
}

/// Resultado de escribir un trozo.
#[derive(Debug, Clone)]
pub struct PutReport {
    pub chunk: ChunkRef,
    /// Proveedores que no aceptaron su fragmento. Si no esta vacio, el trozo quedo escrito
    /// pero con menos redundancia de la pedida: hay que pasarle [`ChunkStore::repair`].
    pub degraded: Vec<String>,
}

impl PutReport {
    pub fn is_fully_redundant(&self) -> bool {
        self.degraded.is_empty()
    }
}

/// Estado de salud de un trozo, tal y como lo ve [`ChunkStore::scrub`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkHealth {
    pub id: ChunkId,
    /// Fragmentos presentes, por indice de proveedor.
    pub present: Vec<usize>,
    /// Fragmentos ausentes, por indice de proveedor.
    pub missing: Vec<usize>,
    /// Cuantos proveedores mas pueden caerse antes de perder el trozo.
    pub remaining_tolerance: usize,
}

impl ChunkHealth {
    /// Si es `false`, el trozo ya no se puede leer.
    pub fn is_readable(&self) -> bool {
        self.remaining_tolerance != usize::MAX
    }
}

/// Almacen de trozos repartido entre varios proveedores.
pub struct ChunkStore {
    layout: Layout,
    backends: Vec<Arc<dyn Backend>>,
    key: MasterKey,
}

impl std::fmt::Debug for ChunkStore {
    /// Manual porque `Arc<dyn Backend>` no es `Debug` y porque la clave no debe imprimirse.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkStore")
            .field("layout", &self.layout)
            .field("backends", &self.backend_names())
            .finish_non_exhaustive()
    }
}

impl ChunkStore {
    /// Crea el almacen. Exige exactamente tantos proveedores como fragmentos produce la
    /// topologia: un `rs:4,2` sobre 5 proveedores no es un `rs:4,2`.
    pub fn new(
        layout: Layout,
        backends: Vec<Arc<dyn Backend>>,
        key: MasterKey,
    ) -> Result<Self, CoreError> {
        layout.validate().map_err(CoreError::Raid)?;
        if backends.len() != layout.shard_count() {
            return Err(CoreError::BackendCount {
                layout,
                needed: layout.shard_count(),
                given: backends.len(),
            });
        }
        Ok(Self {
            layout,
            backends,
            key,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn backend_names(&self) -> Vec<&str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    /// Cifra, reparte y sube un trozo.
    ///
    /// Tolera que fallen proveedores mientras sobrevivan los suficientes para poder leerlo
    /// despues; los que fallaron se listan en [`PutReport::degraded`]. Esta es la diferencia
    /// entre un sistema que se cae porque un proveedor tiene un mal dia y uno que sigue.
    pub async fn put(&self, data: &[u8]) -> Result<PutReport, CoreError> {
        let id = ChunkId::of(data);
        let sealed = self.key.seal(data, id.as_str().as_bytes())?;
        let shards = libredisk_raid::split(self.layout, &sealed)?;

        let writes = shards.into_iter().enumerate().map(|(i, shard)| {
            let backend = &self.backends[i];
            let key = id.shard_key(i);
            async move { (i, backend.put(&key, shard).await) }
        });

        let mut degraded = Vec::new();
        for (i, result) in join_all(writes).await {
            if result.is_err() {
                degraded.push(self.backends[i].name().to_string());
            }
        }

        let written = self.layout.shard_count() - degraded.len();
        if written < self.layout.shards_needed() {
            return Err(CoreError::Unrecoverable {
                id,
                available: written,
                needed: self.layout.shards_needed(),
            });
        }

        Ok(PutReport {
            chunk: ChunkRef {
                id,
                plain_len: data.len(),
                sealed_len: sealed.len(),
            },
            degraded,
        })
    }

    /// Descarga, reconstruye, descifra y **verifica** un trozo.
    pub async fn get(&self, chunk: &ChunkRef) -> Result<Vec<u8>, CoreError> {
        Ok(self.recover(chunk).await?.1)
    }

    /// Reconstruye un trozo y devuelve `(sellado, claro)`, ambos verificados.
    ///
    /// Devolver tambien el blob **sellado original** no es un capricho: `repair` lo
    /// necesita. Sellar de nuevo generaria un nonce distinto, y los fragmentos nuevos ya no
    /// casarian con los supervivientes, que provienen del sellado viejo. Los fragmentos se
    /// regeneran re-repartiendo este mismo blob, no volviendo a cifrar.
    async fn recover(&self, chunk: &ChunkRef) -> Result<(Vec<u8>, Vec<u8>), CoreError> {
        let shards = self.fetch_shards(&chunk.id).await;

        let available = shards.iter().filter(|s| s.is_some()).count();
        if available < self.layout.shards_needed() {
            return Err(CoreError::Unrecoverable {
                id: chunk.id.clone(),
                available,
                needed: self.layout.shards_needed(),
            });
        }

        let sealed = libredisk_raid::join(self.layout, shards, chunk.sealed_len)?;
        let data = self.key.open(&sealed, chunk.id.as_str().as_bytes())?;

        // El direccionamiento por contenido solo vale si se comprueba. Sin esto, un
        // proveedor que devuelva basura la propagaria hasta el usuario.
        let actual = ChunkId::of(&data);
        if actual != chunk.id {
            return Err(CoreError::HashMismatch {
                id: chunk.id.clone(),
                actual,
            });
        }
        Ok((sealed, data))
    }

    /// Revisa que fragmentos siguen vivos, sin descargarlos.
    pub async fn scrub(&self, chunk: &ChunkRef) -> ChunkHealth {
        let checks = self.backends.iter().enumerate().map(|(i, backend)| {
            let key = chunk.id.shard_key(i);
            async move { (i, backend.exists(&key).await.unwrap_or(false)) }
        });

        let (mut present, mut missing) = (Vec::new(), Vec::new());
        for (i, ok) in join_all(checks).await {
            if ok { present.push(i) } else { missing.push(i) }
        }

        let needed = self.layout.shards_needed();
        let remaining_tolerance = if present.len() >= needed {
            present.len() - needed
        } else {
            usize::MAX
        };

        ChunkHealth {
            id: chunk.id.clone(),
            present,
            missing,
            remaining_tolerance,
        }
    }

    /// Regenera los fragmentos que falten y los vuelve a subir.
    ///
    /// Es la operacion que devuelve la redundancia despues de perder un proveedor, ya sea
    /// porque se cayo o porque el usuario lo sustituyo por otro.
    pub async fn repair(&self, chunk: &ChunkRef) -> Result<Vec<usize>, CoreError> {
        let health = self.scrub(chunk).await;
        if health.missing.is_empty() {
            return Ok(Vec::new());
        }

        // Se recupera el blob sellado original y se vuelve a repartir. El reparto
        // Reed-Solomon es determinista, asi que los fragmentos regenerados son identicos,
        // byte a byte, a los que se perdieron.
        let (sealed, _) = self.recover(chunk).await?;
        let shards = libredisk_raid::split(self.layout, &sealed)?;

        let writes = health.missing.iter().map(|&i| {
            let backend = &self.backends[i];
            let key = chunk.id.shard_key(i);
            let shard = shards[i].clone();
            async move { (i, backend.put(&key, shard).await) }
        });

        let mut repaired = Vec::new();
        for (i, result) in join_all(writes).await {
            if result.is_ok() {
                repaired.push(i);
            }
        }
        repaired.sort_unstable();
        Ok(repaired)
    }

    async fn fetch_shards(&self, id: &ChunkId) -> Vec<Option<Vec<u8>>> {
        let reads = self.backends.iter().enumerate().map(|(i, backend)| {
            let key = id.shard_key(i);
            async move { (i, backend.get(&key).await.ok()) }
        });

        let mut shards = vec![None; self.backends.len()];
        for (i, data) in join_all(reads).await {
            shards[i] = data;
        }
        shards
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libredisk_backend::OpendalBackend;

    fn clave() -> MasterKey {
        MasterKey::derive_with("frase de prueba", b"sal-de-prueba-16", 8, 1).unwrap()
    }

    fn store(layout: Layout) -> ChunkStore {
        let backends: Vec<Arc<dyn Backend>> = (0..layout.shard_count())
            .map(|i| {
                Arc::new(OpendalBackend::memory(format!("proveedor-{i}")).unwrap())
                    as Arc<dyn Backend>
            })
            .collect();
        ChunkStore::new(layout, backends, clave()).unwrap()
    }

    /// Simula que un proveedor desaparece: borra todos sus fragmentos.
    async fn tirar_proveedor(store: &ChunkStore, chunk: &ChunkRef, index: usize) {
        store.backends[index]
            .delete(&chunk.id.shard_key(index))
            .await
            .unwrap();
    }

    fn datos(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[tokio::test]
    async fn ida_y_vuelta() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let d = datos(100_000);
        let report = s.put(&d).await.unwrap();
        assert!(report.is_fully_redundant());
        assert_eq!(report.chunk.plain_len, d.len());
        assert_eq!(s.get(&report.chunk).await.unwrap(), d);
    }

    #[tokio::test]
    async fn el_id_es_el_hash_del_contenido() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let d = datos(1000);
        let report = s.put(&d).await.unwrap();
        assert_eq!(report.chunk.id, ChunkId::of(&d));

        // Mismo contenido, mismo id: es lo que habilita la deduplicacion.
        assert_eq!(s.put(&d).await.unwrap().chunk.id, report.chunk.id);
    }

    #[tokio::test]
    async fn los_proveedores_solo_ven_datos_opacos() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let secreto = b"NUMERO DE CUENTA 123456789".repeat(40);
        let report = s.put(&secreto).await.unwrap();

        for (i, backend) in s.backends.iter().enumerate() {
            let fragmento = backend.get(&report.chunk.id.shard_key(i)).await.unwrap();
            assert!(
                !fragmento
                    .windows(26)
                    .any(|w| w == b"NUMERO DE CUENTA 123456789"),
                "el proveedor {i} puede leer el contenido en claro"
            );
        }
    }

    /// **El criterio de exito de la v0.1.** Se pierde un proveedor entero y el volumen sigue.
    #[tokio::test]
    async fn sobrevive_a_perder_un_proveedor_entero() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let d = datos(50_000);
        let chunk = s.put(&d).await.unwrap().chunk;

        tirar_proveedor(&s, &chunk, 2).await;

        assert_eq!(
            s.get(&chunk).await.unwrap(),
            d,
            "el dato debia seguir siendo legible"
        );
        let salud = s.scrub(&chunk).await;
        assert_eq!(salud.missing, vec![2]);
        assert_eq!(
            salud.remaining_tolerance, 1,
            "queda margen para una caida mas"
        );
    }

    #[tokio::test]
    async fn sobrevive_a_perder_m_proveedores() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let d = datos(50_000);
        let chunk = s.put(&d).await.unwrap().chunk;

        tirar_proveedor(&s, &chunk, 0).await;
        tirar_proveedor(&s, &chunk, 5).await;

        assert_eq!(s.get(&chunk).await.unwrap(), d);
        assert_eq!(
            s.scrub(&chunk).await.remaining_tolerance,
            0,
            "ya sin margen"
        );
    }

    #[tokio::test]
    async fn perder_mas_de_m_falla_con_un_error_claro() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let chunk = s.put(&datos(50_000)).await.unwrap().chunk;

        for i in [0, 1, 2] {
            tirar_proveedor(&s, &chunk, i).await;
        }

        let err = s.get(&chunk).await.unwrap_err();
        assert!(matches!(
            err,
            CoreError::Unrecoverable {
                available: 3,
                needed: 4,
                ..
            }
        ));
        assert!(!s.scrub(&chunk).await.is_readable());
    }

    #[tokio::test]
    async fn reparar_restaura_la_redundancia() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let d = datos(50_000);
        let chunk = s.put(&d).await.unwrap().chunk;

        tirar_proveedor(&s, &chunk, 1).await;
        tirar_proveedor(&s, &chunk, 3).await;
        assert_eq!(s.scrub(&chunk).await.remaining_tolerance, 0);

        assert_eq!(s.repair(&chunk).await.unwrap(), vec![1, 3]);

        let salud = s.scrub(&chunk).await;
        assert!(
            salud.missing.is_empty(),
            "debia quedar redundancia completa"
        );
        assert_eq!(salud.remaining_tolerance, 2);
        assert_eq!(s.get(&chunk).await.unwrap(), d, "y el dato intacto");
    }

    /// Regresion: `repair` no debe volver a sellar el trozo.
    ///
    /// El sellado usa un nonce aleatorio, asi que re-cifrar produciria fragmentos que no
    /// casan con los supervivientes y el trozo quedaria ilegible aun estando "reparado".
    /// Los fragmentos regenerados tienen que ser identicos byte a byte a los originales.
    #[tokio::test]
    async fn los_fragmentos_reparados_son_identicos_a_los_originales() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let chunk = s.put(&datos(20_000)).await.unwrap().chunk;

        let original = s.backends[3].get(&chunk.id.shard_key(3)).await.unwrap();
        tirar_proveedor(&s, &chunk, 3).await;
        s.repair(&chunk).await.unwrap();
        let regenerado = s.backends[3].get(&chunk.id.shard_key(3)).await.unwrap();

        assert_eq!(
            regenerado, original,
            "el fragmento regenerado difiere del original"
        );
    }

    #[tokio::test]
    async fn reparar_sin_nada_que_reparar_no_hace_nada() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let chunk = s.put(&datos(1000)).await.unwrap().chunk;
        assert!(s.repair(&chunk).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn un_fragmento_corrupto_no_llega_al_usuario() {
        let s = store(Layout::Mirror { n: 3 });
        let d = datos(1000);
        let chunk = s.put(&d).await.unwrap().chunk;

        // Un proveedor devuelve basura con el tamano correcto.
        s.backends[0]
            .put(&chunk.id.shard_key(0), vec![0xFF; chunk.sealed_len])
            .await
            .unwrap();

        // En mirror se coge la primera copia que responda, que aqui es la manipulada; el
        // sellado tiene que detectarlo antes de devolver nada.
        let err = s.get(&chunk).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Crypto(_) | CoreError::HashMismatch { .. }),
            "se esperaba fallo de autenticacion, se obtuvo {err:?}"
        );
    }

    #[tokio::test]
    async fn mirror_sobrevive_a_todas_las_copias_menos_una() {
        let s = store(Layout::Mirror { n: 3 });
        let d = datos(5000);
        let chunk = s.put(&d).await.unwrap().chunk;

        tirar_proveedor(&s, &chunk, 0).await;
        tirar_proveedor(&s, &chunk, 1).await;

        assert_eq!(s.get(&chunk).await.unwrap(), d);
    }

    #[tokio::test]
    async fn numero_de_proveedores_incoherente_se_rechaza() {
        let backends: Vec<Arc<dyn Backend>> =
            vec![Arc::new(OpendalBackend::memory("uno").unwrap())];
        let err =
            ChunkStore::new(Layout::ReedSolomon { k: 4, m: 2 }, backends, clave()).unwrap_err();
        assert!(matches!(
            err,
            CoreError::BackendCount {
                needed: 6,
                given: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn trozo_vacio_y_trozo_de_un_byte() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        for d in [vec![], vec![42u8]] {
            let chunk = s.put(&d).await.unwrap().chunk;
            assert_eq!(s.get(&chunk).await.unwrap(), d);
        }
    }

    #[tokio::test]
    async fn el_chunkref_viaja_como_json() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let chunk = s.put(&datos(1234)).await.unwrap().chunk;

        let json = serde_json::to_string(&chunk).unwrap();
        let vuelta: ChunkRef = serde_json::from_str(&json).unwrap();
        assert_eq!(vuelta, chunk);
    }
}
