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

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use futures::future::join_all;
use libredisk_backend::{Backend, BackendError};
use libredisk_crypto::MasterKey;
use libredisk_raid::{Layout, RaidError};
use serde::{Deserialize, Serialize};

pub mod cache;
pub mod diskcache;
pub mod file;
pub mod header;
pub mod rescue;
pub mod tree;
pub mod writeback;

#[cfg(test)]
mod testutil;

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
    #[error("el tamano de trozo debe ser mayor que cero")]
    ZeroChunkSize,
    #[error("manifiesto invalido: {0}")]
    InvalidManifest(String),
    #[error("fallo de E/S local: {0}")]
    Io(#[from] std::io::Error),
    #[error("fallo al serializar metadatos: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("la ruta '{0}' no existe")]
    PathNotFound(String),
    #[error("'{0}' no es un directorio")]
    NotADirectory(String),
    #[error("'{0}' no es un fichero")]
    NotAFile(String),
    #[error("el directorio '{0}' no esta vacio")]
    DirNotEmpty(String),
    #[error("ruta invalida: {0}")]
    InvalidPath(String),
    #[error("el volumen no tiene ninguna raiz publicada")]
    NoRoot,
    #[error("ya hay una raiz publicada: crear encima dejaria el arbol anterior inalcanzable")]
    RootExists,
    #[error("ningun proveedor acepto el puntero de raiz")]
    RootNotPublished,
    #[error("el trozo {0} no pertenece a este volumen, o se cifro con otra clave")]
    Foreign(ChunkId),
    #[error("ya hay un volumen creado en estos proveedores")]
    VolumeExists,
    #[error("no hay ningun volumen en estos proveedores")]
    NoVolume,
    #[error("los proveedores no coinciden con los del volumen: {0}")]
    BackendMismatch(String),
    #[error("los proveedores devuelven cabeceras distintas: {0}")]
    HeaderConflict(String),
    #[error("ningun proveedor acepto la cabecera del volumen")]
    HeaderNotWritten,
    #[error(transparent)]
    Crypto(#[from] libredisk_crypto::CryptoError),
    #[error(transparent)]
    Raid(#[from] RaidError),
    #[error(transparent)]
    Backend(#[from] BackendError),
}

/// Identificador de un trozo: el BLAKE3 de su contenido en claro, en hexadecimal.
///
/// Ordena por su hexadecimal. No significa nada semanticamente, pero hace que cualquier
/// recorrido o informe salga en el mismo orden dos veces seguidas.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChunkId(String);

/// Prefijo comun de todos los fragmentos, y por tanto lo que se le pasa a
/// [`Backend::list`] para enumerar un proveedor entero de una vez.
pub const SHARD_PREFIX: &str = "chunks/";

impl ChunkId {
    /// Calcula el identificador de un contenido.
    pub fn of(data: &[u8]) -> Self {
        Self(blake3::hash(data).to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reconstruye un identificador a partir de su hexadecimal.
    ///
    /// Solo hace falta para el rescate, que parte de las claves que hay en los
    /// proveedores en vez de un manifiesto. Valida la forma antes de aceptarla: 64
    /// caracteres hexadecimales en minuscula, que es lo que produce BLAKE3.
    pub fn parse(s: &str) -> Option<Self> {
        let bien = s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        bien.then(|| Self(s.to_string()))
    }

    /// Clave del fragmento `index` de este trozo dentro de un proveedor.
    ///
    /// Se antepone un prefijo de dos caracteres del hash porque muchos almacenes degradan
    /// al acumular millones de objetos en un mismo "directorio".
    pub fn shard_key(&self, index: usize) -> String {
        format!("{SHARD_PREFIX}{}/{}.{index:03}", &self.0[..2], self.0)
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
    /// El trozo esta preparado en local pero **todavia no en los proveedores**.
    ///
    /// Mientras sea `true`, ese contenido solo existe en esta maquina.
    pub pending: bool,
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

/// Lo que ve [`ChunkStore::scrub_many`] tras enumerar los proveedores.
#[derive(Debug, Clone)]
pub struct ScrubReport {
    /// Salud de cada trozo, en el mismo orden en que se pidieron.
    pub health: Vec<ChunkHealth>,
    /// Proveedores que no se dejaron enumerar, por indice y con el motivo.
    ///
    /// Sus fragmentos figuran como ausentes en `health`, pero eso es lo que no se sabe, no
    /// lo que se ha perdido. Antes de dar un trozo por degradado por culpa de uno de estos,
    /// hay que volver a preguntar.
    pub unlistable: Vec<(usize, String)>,
}

impl ScrubReport {
    /// `true` si todos los proveedores respondieron. Si es `false`, las cifras de `health`
    /// son un suelo pesimista, no una medida.
    pub fn is_complete(&self) -> bool {
        self.unlistable.is_empty()
    }
}

/// Almacen de trozos repartido entre varios proveedores.
pub struct ChunkStore {
    layout: Layout,
    backends: Vec<Arc<dyn Backend>>,
    key: MasterKey,
    /// Cache de lectura en memoria. Sin ella cada operacion del sistema de ficheros va a la
    /// red.
    cache: Option<cache::ChunkCache>,
    /// Segundo escalon, en disco y sobreviviendo al reinicio. Guarda el blob **sellado**:
    /// los datos del usuario no se quedan en claro en la maquina.
    disco: Option<diskcache::DiskCache>,
    /// Cola de escritura diferida. Con ella, escribir no sube nada: prepara en local y
    /// espera a `sync`.
    pending: Option<writeback::WriteQueue>,
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
            cache: None,
            disco: None,
            pending: None,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Activa una cache de lectura de `bytes` de capacidad.
    ///
    /// No hace falta invalidarla nunca: los trozos son inmutables y su nombre es su
    /// contenido. Ver [`cache`].
    pub fn with_cache(mut self, bytes: usize) -> Self {
        self.cache = Some(cache::ChunkCache::new(bytes));
        self
    }

    /// Anade el segundo escalon: una cache en disco que sobrevive al proceso.
    pub fn with_disk_cache(mut self, disco: diskcache::DiskCache) -> Self {
        self.disco = Some(disco);
        self
    }

    /// Difiere las escrituras: `put` prepara en local en vez de subir, y la raiz espera.
    pub fn with_writeback(mut self, cola: writeback::WriteQueue) -> Self {
        self.pending = Some(cola);
        self
    }

    /// La cola de escritura diferida, si la hay.
    pub fn writeback(&self) -> Option<&writeback::WriteQueue> {
        self.pending.as_ref()
    }

    /// Que queda por subir.
    pub fn pendiente(&self) -> writeback::Pendiente {
        self.pending
            .as_ref()
            .map(writeback::WriteQueue::pendiente)
            .unwrap_or_default()
    }

    /// Identificadores alcanzables desde una raiz, mirando primero en la cola.
    ///
    /// Los nodos por los que pasa pueden estar todavia sin subir: `get_shared` los encuentra
    /// en la cola, que es el unico sitio donde existen hasta que se sincronice.
    async fn alcanzables(&self, raiz: &ChunkRef) -> Result<BTreeSet<ChunkId>, CoreError> {
        let mut vivos = BTreeSet::new();
        let mut pendientes = vec![raiz.clone()];

        while let Some(nodo) = pendientes.pop() {
            if !vivos.insert(nodo.id.clone()) {
                continue;
            }
            let bytes = self.get_shared(&nodo).await?;
            match serde_json::from_slice::<tree::Node>(&bytes)? {
                tree::Node::Dir(entradas) => {
                    pendientes.extend(entradas.into_values().map(|e| e.node));
                }
                tree::Node::File(manifest) => {
                    for c in manifest.chunks {
                        vivos.insert(c.id);
                    }
                }
            }
        }
        Ok(vivos)
    }

    /// Sube todo lo que haya preparado y, **solo si todo llego**, publica la raiz.
    ///
    /// Ese "solo si" es el invariante entero: una raiz publicada antes que sus trozos dejaria
    /// el volumen apuntando a nodos que no existen, y cualquier lector lo veria corrupto sin
    /// estarlo. Si algo falla, la raiz se queda en la cola y el volumen sigue viendose como
    /// antes de la escritura: un estado viejo, pero integro.
    pub async fn sync(&self) -> Result<writeback::SyncReport, CoreError> {
        let Some(cola) = &self.pending else {
            return Ok(writeback::SyncReport::default());
        };

        let mut informe = writeback::SyncReport::default();

        // Lo que no cuelgue de la raiz pendiente es basura: versiones intermedias que quedaron
        // superadas antes de llegar a subir. Subirlas seria pagar ancho de banda y cuota por
        // algo que nadie va a leer jamas.
        //
        // Es seguro descartarlas porque un trozo preparado se escribio **en esta sesion**, y
        // la raiz que ya estuviera publicada es anterior: no puede referenciarlo. Si el
        // recorrido falla no se descarta nada, que es el lado prudente del error.
        let vivos = match cola.root() {
            Some(root) => self.alcanzables(&root.node).await.ok(),
            None => None,
        };

        for id in cola.chunks() {
            if let Some(vivos) = &vivos
                && !vivos.contains(&id)
            {
                let tam = cola.sealed(&id).await.map(|b| b.len() as u64).unwrap_or(0);
                cola.done(&id).await?;
                informe.descartados += 1;
                informe.bytes_descartados += tam;
                continue;
            }

            let Some(sellado) = cola.sealed(&id).await else {
                // El diario lo anota pero el blob no esta. No hay nada que subir y no se
                // puede inventar: se declara y se sigue, pero la raiz no se publicara.
                informe
                    .fallidos
                    .push((id, "no queda el blob preparado en disco".into()));
                continue;
            };

            match self.subir_fragmentos(&id, &sellado).await {
                Ok(degraded) => {
                    for nombre in degraded {
                        if !informe.degraded.contains(&nombre) {
                            informe.degraded.push(nombre);
                        }
                    }
                    informe.bytes += sellado.len() as u64;
                    informe.subidos += 1;
                    cola.done(&id).await?;
                }
                Err(e) => informe.fallidos.push((id, e.to_string())),
            }
        }

        // La raiz solo si TODO subio. Si algo fallo se queda esperando, y un `sync` posterior
        // la publicara cuando sus trozos esten arriba.
        if let Some(root) = cola.root()
            && informe.todo_arriba()
        {
            tree::publicar_ahora(self, &root).await?;
            cola.root_done().await?;
            informe.raiz_publicada = true;
        }

        Ok(informe)
    }

    /// Estadisticas de la cache en disco, si la hay.
    pub fn disk_stats(&self) -> Option<diskcache::Stats> {
        self.disco.as_ref().map(diskcache::DiskCache::stats)
    }

    /// Estadisticas de la cache, si la hay.
    pub fn cache_stats(&self) -> Option<cache::Stats> {
        self.cache.as_ref().map(cache::ChunkCache::stats)
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

        // Con escritura diferida no se sube nada: se prepara en local y se espera a `sync`.
        // El trozo es legible desde ya, porque `get_shared` mira tambien en la cola.
        if let Some(cola) = &self.pending {
            cola.stage(&id, &sealed).await?;
            return Ok(PutReport {
                chunk: ChunkRef {
                    id,
                    plain_len: data.len(),
                    sealed_len: sealed.len(),
                },
                degraded: Vec::new(),
                pending: true,
            });
        }

        let degraded = self.subir_fragmentos(&id, &sealed).await?;
        Ok(PutReport {
            chunk: ChunkRef {
                id,
                plain_len: data.len(),
                sealed_len: sealed.len(),
            },
            degraded,
            pending: false,
        })
    }

    /// Reparte un blob sellado entre los proveedores y devuelve los que fallaron.
    ///
    /// Sale de `put` para que `sync` pueda reutilizarlo: subir un trozo preparado es
    /// exactamente lo mismo que subirlo en el momento, solo que mas tarde.
    pub(crate) async fn subir_fragmentos(
        &self,
        id: &ChunkId,
        sealed: &[u8],
    ) -> Result<Vec<String>, CoreError> {
        let shards = libredisk_raid::split(self.layout, sealed)?;

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
                id: id.clone(),
                available: written,
                needed: self.layout.shards_needed(),
            });
        }
        Ok(degraded)
    }

    /// Descarga, reconstruye, descifra y **verifica** un trozo.
    pub async fn get(&self, chunk: &ChunkRef) -> Result<Vec<u8>, CoreError> {
        Ok(self.get_shared(chunk).await?.as_ref().clone())
    }

    /// Igual que [`ChunkStore::get`] pero compartiendo el buffer en vez de copiarlo.
    ///
    /// Existe por la cache: con trozos de 16 MiB, devolver un `Vec` propio obligaria a
    /// copiar 16 MiB en cada acierto. Es rapido comparado con la red, pero es gratis
    /// evitarlo, y `read_at` hace muchas lecturas pequenas sobre el mismo trozo.
    pub async fn get_shared(&self, chunk: &ChunkRef) -> Result<Arc<Vec<u8>>, CoreError> {
        // Escalon 1: memoria. Ya en claro, sin descifrar nada.
        if let Some(cache) = &self.cache
            && let Some(datos) = cache.get(&chunk.id)
        {
            return Ok(datos);
        }

        // Escalon 1,5: la cola de escritura. Lo que se acaba de escribir y todavia no ha
        // subido **solo existe aqui**, asi que hay que mirar antes que en ningun otro sitio
        // remoto. Sin esto, releer lo recien escrito fallaria.
        if let Some(cola) = &self.pending
            && let Some(sellado) = cola.sealed(&chunk.id).await
        {
            let aad = chunk.id.as_str().as_bytes();
            let claro = Arc::new(self.key.open(&sellado, aad)?);
            if ChunkId::of(&claro) == chunk.id {
                if let Some(cache) = &self.cache {
                    cache.put(chunk.id.clone(), Arc::clone(&claro));
                }
                return Ok(claro);
            }
        }

        // Escalon 2: disco. Guarda el blob sellado, asi que hay que abrirlo; a cambio, los
        // datos del usuario no estan en claro en la maquina.
        if let Some(disco) = &self.disco {
            let aad = chunk.id.as_str().as_bytes().to_vec();
            // Se descifra una sola vez: el verificado y el resultado salen del mismo paso.
            // Comprobar el hash ademas del tag separa una cache de una fuente de corrupcion
            // silenciosa.
            let abierto = disco
                .get(&chunk.id, |bytes| {
                    let claro = self.key.open(bytes, &aad).ok()?;
                    (ChunkId::of(&claro) == chunk.id).then_some(claro)
                })
                .await;

            if let Some(claro) = abierto {
                let claro = Arc::new(claro);
                if let Some(cache) = &self.cache {
                    cache.put(chunk.id.clone(), Arc::clone(&claro));
                }
                return Ok(claro);
            }
        }

        // Escalon 3: los proveedores.
        let (sellado, claro) = self.recover(chunk).await?;
        let claro = Arc::new(claro);

        if let Some(disco) = &self.disco {
            disco.put(&chunk.id, &sellado).await;
        }
        if let Some(cache) = &self.cache {
            cache.put(chunk.id.clone(), Arc::clone(&claro));
        }
        Ok(claro)
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

        self.health(chunk, present, missing)
    }

    /// Revisa muchos trozos con **una** enumeracion por proveedor, en vez de una pregunta
    /// por fragmento.
    ///
    /// Es la diferencia entre `n * m` llamadas y `m`, donde `n` es el numero de trozos. Con
    /// lo medido contra Google Drive (~6,5 s por consultar algo que no existe, que es justo
    /// el caso normal de `scrub`) y un volumen de 1 TB, revisarlo trozo a trozo son semanas.
    /// Para eso existe [`Backend::list`].
    ///
    /// Que sean `m` llamadas **no** quiere decir `m` peticiones de red: contra un proveedor
    /// jerarquico cada `list` recorre el arbol y cuesta una peticion por directorio. Lo que
    /// desaparece es la dependencia de `n`, que era lo que hacia inviable el scrub; el coste
    /// que queda depende de como este repartido el arbol, y eso esta medido en
    /// [`Backend::list`].
    ///
    /// Un proveedor que no se deja enumerar **no** se cuenta como un proveedor vacio: eso
    /// convertiria una caida pasajera en una perdida total sobre el papel. Sus fragmentos
    /// figuran como ausentes, pero el proveedor queda declarado en
    /// [`ScrubReport::unlistable`] para que quien lea el informe sepa que eso es lo que no se
    /// sabe, no lo que se ha perdido.
    pub async fn scrub_many(&self, chunks: &[ChunkRef]) -> ScrubReport {
        let listados = join_all(
            self.backends
                .iter()
                .enumerate()
                .map(|(i, backend)| async move { (i, backend.list(SHARD_PREFIX).await) }),
        )
        .await;

        let mut presentes: Vec<HashSet<String>> = vec![HashSet::new(); self.backends.len()];
        let mut unlistable = Vec::new();
        for (i, resultado) in listados {
            match resultado {
                Ok(claves) => presentes[i] = claves.into_iter().collect(),
                Err(e) => unlistable.push((i, e.to_string())),
            }
        }

        let health = chunks
            .iter()
            .map(|chunk| {
                let (mut present, mut missing) = (Vec::new(), Vec::new());
                for (i, vistas) in presentes.iter().enumerate() {
                    if vistas.contains(&chunk.id.shard_key(i)) {
                        present.push(i);
                    } else {
                        missing.push(i);
                    }
                }
                self.health(chunk, present, missing)
            })
            .collect();

        ScrubReport { health, unlistable }
    }

    /// Traduce fragmentos presentes y ausentes a salud.
    ///
    /// Comun a `scrub` y `scrub_many`: la cuenta de tolerancia tiene que salir igual se haya
    /// averiguado preguntando o enumerando.
    fn health(&self, chunk: &ChunkRef, present: Vec<usize>, missing: Vec<usize>) -> ChunkHealth {
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
    use std::sync::atomic::Ordering;

    use async_trait::async_trait;
    use libredisk_backend::OpendalBackend;

    use crate::testutil::{clave, como_backends, contadores, reiniciar, store};

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

    #[tokio::test]
    async fn scrub_many_da_la_misma_salud_que_preguntar_trozo_a_trozo() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let mut chunks = Vec::new();
        for n in [1_000, 50_000, 120_000] {
            chunks.push(s.put(&datos(n)).await.unwrap().chunk);
        }
        tirar_proveedor(&s, &chunks[1], 2).await;
        tirar_proveedor(&s, &chunks[2], 0).await;
        tirar_proveedor(&s, &chunks[2], 5).await;

        let informe = s.scrub_many(&chunks).await;
        assert!(informe.is_complete());
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(
                informe.health[i],
                s.scrub(chunk).await,
                "enumerar y preguntar deben dar la misma respuesta (trozo {i})"
            );
        }
    }

    #[tokio::test]
    async fn scrub_many_sobre_una_lista_vacia_no_llama_a_nadie() {
        let s = store(Layout::ReedSolomon { k: 4, m: 2 });
        let informe = s.scrub_many(&[]).await;
        assert!(informe.health.is_empty());
        assert!(informe.is_complete());
    }

    #[tokio::test]
    async fn scrub_many_enumera_una_vez_por_proveedor_en_vez_de_preguntar_por_fragmento() {
        let layout = Layout::ReedSolomon { k: 4, m: 2 };
        let cs = contadores(layout.shard_count());
        let s = ChunkStore::new(layout, como_backends(&cs), clave()).unwrap();

        let mut chunks = Vec::new();
        for n in 0..25 {
            chunks.push(s.put(&datos(500 + n)).await.unwrap().chunk);
        }
        reiniciar(&cs);

        s.scrub_many(&chunks).await;
        for c in &cs {
            assert_eq!(
                c.lists.load(Ordering::Relaxed),
                1,
                "una enumeracion por proveedor, no una por trozo"
            );
            assert_eq!(
                c.exists.load(Ordering::Relaxed),
                0,
                "enumerando no hace falta preguntar por ningun fragmento"
            );
        }

        // Lo que cuesta trozo a trozo, para que la comparacion quede escrita y no de palabra.
        for chunk in &chunks {
            s.scrub(chunk).await;
        }
        for c in &cs {
            assert_eq!(
                c.exists.load(Ordering::Relaxed),
                chunks.len(),
                "scrub uno a uno cuesta una pregunta por trozo y proveedor"
            );
        }
    }

    /// Un proveedor que no se deja enumerar no es un proveedor vacio. Confundirlos
    /// convertiria una caida pasajera en una perdida total sobre el papel.
    #[tokio::test]
    async fn un_proveedor_que_falla_al_enumerar_se_declara_en_vez_de_darse_por_vacio() {
        struct NoEnumera(OpendalBackend);

        #[async_trait]
        impl Backend for NoEnumera {
            fn name(&self) -> &str {
                self.0.name()
            }
            async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackendError> {
                self.0.put(key, data).await
            }
            async fn get(&self, key: &str) -> Result<Vec<u8>, BackendError> {
                self.0.get(key).await
            }
            async fn delete(&self, key: &str) -> Result<(), BackendError> {
                self.0.delete(key).await
            }
            async fn exists(&self, key: &str) -> Result<bool, BackendError> {
                self.0.exists(key).await
            }
            /// Cualquier fallo sirve: lo que se prueba es que no se traduce a "vacio".
            async fn list(&self, _prefix: &str) -> Result<Vec<String>, BackendError> {
                Err(BackendError::NotFound("el proveedor no responde".into()))
            }
        }

        let layout = Layout::ReedSolomon { k: 4, m: 2 };
        let backends: Vec<Arc<dyn Backend>> = (0..layout.shard_count())
            .map(|i| {
                let mem = OpendalBackend::memory(format!("proveedor-{i}")).unwrap();
                if i == 3 {
                    Arc::new(NoEnumera(mem)) as Arc<dyn Backend>
                } else {
                    Arc::new(mem) as Arc<dyn Backend>
                }
            })
            .collect();
        let s = ChunkStore::new(layout, backends, clave()).unwrap();
        let chunk = s.put(&datos(20_000)).await.unwrap().chunk;

        let informe = s.scrub_many(std::slice::from_ref(&chunk)).await;
        assert!(!informe.is_complete(), "el fallo debia quedar declarado");
        assert_eq!(informe.unlistable.len(), 1);
        assert_eq!(informe.unlistable[0].0, 3);
        assert_eq!(informe.health[0].missing, vec![3]);

        // Y preguntando de uno en uno se ve que el fragmento si estaba: el informe era un
        // suelo pesimista, que es exactamente lo que declara ser.
        assert!(s.scrub(&chunk).await.missing.is_empty());
    }
}
