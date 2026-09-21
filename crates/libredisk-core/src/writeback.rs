//! Escritura diferida: escribir ahora, subir despues.
//!
//! Sin esto, cada escritura de 4 KiB seria una subida sincrona a varios proveedores. Con las
//! latencias medidas en `docs/VIABILIDAD.md`, eso son segundos por operacion, y ningun
//! programa esta escrito para eso.
//!
//! # El invariante
//!
//! **Nunca se publica una raiz que apunte a trozos que todavia no esten en los proveedores.**
//!
//! Si se rompiera, cualquier lector —otra maquina, o un `libredisk rescue`— veria una raiz
//! apuntando a nodos que no existen, y el volumen pareceria corrupto sin estarlo. Por eso la
//! raiz se encola aparte y solo se publica cuando la cola de trozos esta vacia. Si una subida
//! falla, la raiz se queda donde estaba: el volumen sigue viendose como antes de la escritura,
//! que es un estado viejo pero integro.
//!
//! # Lo que esto cuesta en seguridad
//!
//! Hasta ahora la maquina era desechable: todo lo necesario vivia en los proveedores. Esto lo
//! rompe a proposito. Entre escribir y sincronizar hay datos que **solo existen aqui**, y si
//! la maquina muere en esa ventana, se pierden.
//!
//! No se puede evitar —es la definicion misma de diferir— pero si se puede **decir**. Por eso
//! [`Pendiente`] existe y por eso la linea de ordenes la enseña: la diferencia entre "guardado
//! localmente" y "a salvo en la nube" tiene que ser visible, no un detalle de implementacion.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::tree::Root;
use crate::{ChunkId, CoreError};

/// Version del formato del diario.
const DIARIO_VERSION: u32 = 1;

const DIARIO: &str = "pendiente.json";

/// Lo que queda por subir, para poder decirselo al usuario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pendiente {
    pub trozos: usize,
    pub bytes: u64,
    /// Hay una raiz esperando a que suban sus trozos.
    pub raiz: bool,
}

impl Pendiente {
    /// `true` si todo lo escrito esta ya en los proveedores.
    pub fn a_salvo(&self) -> bool {
        self.trozos == 0 && !self.raiz
    }
}

/// Lo que se guarda en disco para sobrevivir a un corte.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Diario {
    version: u32,
    /// Trozos preparados, con su tamano. En orden de escritura.
    trozos: Vec<(ChunkId, u64)>,
    /// La raiz que espera. Solo puede haber una: las intermedias no interesan, porque
    /// publicar la ultima deja el volumen en el estado final de todas formas.
    raiz: Option<Root>,
}

impl Default for Diario {
    fn default() -> Self {
        Self {
            version: DIARIO_VERSION,
            trozos: Vec::new(),
            raiz: None,
        }
    }
}

/// Cola de escrituras pendientes, con su area de preparacion en disco.
pub struct WriteQueue {
    dir: PathBuf,
    interior: Mutex<Diario>,
}

impl WriteQueue {
    /// Abre o crea la cola en `dir`, recuperando lo que hubiera quedado de una ejecucion
    /// anterior.
    ///
    /// Un trozo anotado en el diario cuyo fichero no este se descarta: sin el blob no hay
    /// nada que subir. Eso puede dejar la raiz pendiente apuntando a algo que no existe, asi
    /// que en ese caso tambien se descarta la raiz. Perder una escritura a medias es malo;
    /// publicar una raiz rota es peor.
    pub async fn open(dir: impl AsRef<Path>) -> Result<Self, CoreError> {
        let dir = dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&dir).await?;

        let ruta = dir.join(DIARIO);
        let mut diario: Diario = match tokio::fs::read(&ruta).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(_) => Diario::default(),
        };

        let antes = diario.trozos.len();
        let mut vivos = Vec::with_capacity(antes);
        for (id, tam) in diario.trozos {
            if tokio::fs::try_exists(dir.join(id.as_str()))
                .await
                .unwrap_or(false)
            {
                vivos.push((id, tam));
            }
        }
        if vivos.len() != antes {
            // Falta algo que la raiz podria necesitar. No hay forma de saber cual, asi que
            // se renuncia a publicarla.
            diario.raiz = None;
        }
        diario.trozos = vivos;

        let cola = Self {
            dir,
            interior: Mutex::new(diario),
        };
        cola.persistir().await?;
        Ok(cola)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Diario> {
        self.interior.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Vuelca el diario. Temporal mas renombrado, para que un corte no lo deje a medias.
    async fn persistir(&self) -> Result<(), CoreError> {
        let bytes = {
            let d = self.lock();
            serde_json::to_vec_pretty(&*d)?
        };
        let destino = self.dir.join(DIARIO);
        let temporal = destino.with_extension("tmp");
        tokio::fs::write(&temporal, &bytes).await?;
        tokio::fs::rename(&temporal, &destino).await?;
        Ok(())
    }

    fn ruta(&self, id: &ChunkId) -> PathBuf {
        self.dir.join(id.as_str())
    }

    /// Prepara un trozo sellado para subirlo mas tarde.
    pub async fn stage(&self, id: &ChunkId, sellado: &[u8]) -> Result<(), CoreError> {
        // Escribir el blob antes de anotarlo: si se corta la luz entre las dos cosas, sobra
        // un fichero suelto, que es inofensivo. Al reves quedaria una anotacion sin blob.
        let temporal = self.ruta(id).with_extension("tmp");
        tokio::fs::write(&temporal, sellado).await?;
        tokio::fs::rename(&temporal, self.ruta(id)).await?;

        {
            let mut d = self.lock();
            if !d.trozos.iter().any(|(otro, _)| otro == id) {
                d.trozos.push((id.clone(), sellado.len() as u64));
            }
        }
        self.persistir().await
    }

    /// Anota la raiz que habra que publicar cuando no quede nada por subir.
    pub async fn stage_root(&self, root: Root) -> Result<(), CoreError> {
        self.lock().raiz = Some(root);
        self.persistir().await
    }

    /// El blob sellado de un trozo preparado, si esta aqui.
    ///
    /// Hace falta para poder **releer lo que se acaba de escribir**: hasta que suba, este es
    /// el unico sitio donde existe.
    pub async fn sealed(&self, id: &ChunkId) -> Option<Vec<u8>> {
        if !self.lock().trozos.iter().any(|(otro, _)| otro == id) {
            return None;
        }
        tokio::fs::read(self.ruta(id)).await.ok()
    }

    pub fn chunks(&self) -> Vec<ChunkId> {
        self.lock()
            .trozos
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn root(&self) -> Option<Root> {
        self.lock().raiz.clone()
    }

    pub fn pendiente(&self) -> Pendiente {
        let d = self.lock();
        Pendiente {
            trozos: d.trozos.len(),
            bytes: d.trozos.iter().map(|(_, tam)| tam).sum(),
            raiz: d.raiz.is_some(),
        }
    }

    /// Da un trozo por subido: lo quita del diario y borra el blob.
    pub async fn done(&self, id: &ChunkId) -> Result<(), CoreError> {
        self.lock().trozos.retain(|(otro, _)| otro != id);
        self.persistir().await?;
        let _ = tokio::fs::remove_file(self.ruta(id)).await;
        Ok(())
    }

    /// Da la raiz por publicada.
    pub async fn root_done(&self) -> Result<(), CoreError> {
        self.lock().raiz = None;
        self.persistir().await
    }
}

impl std::fmt::Debug for WriteQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = self.pendiente();
        f.debug_struct("WriteQueue")
            .field("dir", &self.dir)
            .field("trozos", &p.trozos)
            .field("raiz", &p.raiz)
            .finish()
    }
}

/// Lo que hizo una sincronizacion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub subidos: usize,
    pub bytes: u64,
    /// Proveedores que fallaron en algun trozo, sin repetir.
    pub degraded: Vec<String>,
    /// La raiz llego a publicarse.
    pub raiz_publicada: bool,
    /// Trozos que no se pudieron subir, con el motivo. Si no esta vacio, la raiz **no** se
    /// publica.
    pub fallidos: Vec<(ChunkId, String)>,
}

impl SyncReport {
    pub fn todo_arriba(&self) -> bool {
        self.fallidos.is_empty()
    }
}

/// Como se nombra el directorio de preparacion de un volumen.
///
/// Separado por volumen, igual que la cache en disco y por el mismo motivo: dos volumenes
/// distintos no pueden mezclar sus escrituras pendientes.
pub fn dir_de_volumen(base: impl AsRef<Path>, salt: &[u8]) -> PathBuf {
    let etiqueta = ChunkId::of(salt).as_str()[..16].to_string();
    base.as_ref().join("pendiente").join(etiqueta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChunkRef;

    fn dir(nombre: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "libredisk-wb-{}-{nombre}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn id(n: u8) -> ChunkId {
        ChunkId::of(&[n])
    }

    fn raiz(n: u8) -> Root {
        Root {
            generation: n as u64,
            node: ChunkRef {
                id: id(n),
                plain_len: 1,
                sealed_len: 41,
            },
        }
    }

    #[tokio::test]
    async fn prepara_y_devuelve() {
        let d = dir("basico");
        let c = WriteQueue::open(&d).await.unwrap();
        assert!(c.pendiente().a_salvo());

        c.stage(&id(1), b"sellado uno").await.unwrap();
        c.stage(&id(2), b"sellado dos").await.unwrap();
        c.stage_root(raiz(9)).await.unwrap();

        let p = c.pendiente();
        assert_eq!(p.trozos, 2);
        assert_eq!(p.bytes, 22);
        assert!(p.raiz);
        assert!(!p.a_salvo());

        assert_eq!(c.sealed(&id(1)).await.unwrap(), b"sellado uno");
        assert!(c.sealed(&id(3)).await.is_none());

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Lo que se escribio antes de un corte tiene que seguir ahi al volver.
    #[tokio::test]
    async fn sobrevive_a_cerrar_y_abrir() {
        let d = dir("persiste");
        {
            let c = WriteQueue::open(&d).await.unwrap();
            c.stage(&id(1), b"datos").await.unwrap();
            c.stage_root(raiz(7)).await.unwrap();
        }

        let c = WriteQueue::open(&d).await.unwrap();
        assert_eq!(c.chunks(), vec![id(1)]);
        assert_eq!(c.root().unwrap().generation, 7);
        assert_eq!(c.sealed(&id(1)).await.unwrap(), b"datos");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Si falta el blob de un trozo, la raiz pendiente se descarta: publicarla podria dejar
    /// el volumen apuntando a algo que no existe.
    #[tokio::test]
    async fn un_blob_perdido_se_lleva_la_raiz_por_delante() {
        let d = dir("blob-perdido");
        {
            let c = WriteQueue::open(&d).await.unwrap();
            c.stage(&id(1), b"uno").await.unwrap();
            c.stage(&id(2), b"dos").await.unwrap();
            c.stage_root(raiz(3)).await.unwrap();
        }

        // Alguien borra uno de los blobs por debajo.
        std::fs::remove_file(d.join(id(1).as_str())).unwrap();

        let c = WriteQueue::open(&d).await.unwrap();
        assert_eq!(c.chunks(), vec![id(2)], "el trozo sin blob se descarta");
        assert!(
            c.root().is_none(),
            "y con el se va la raiz: mas vale un estado viejo que uno roto"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn dar_por_subido_limpia_diario_y_disco() {
        let d = dir("done");
        let c = WriteQueue::open(&d).await.unwrap();
        c.stage(&id(1), b"x").await.unwrap();

        c.done(&id(1)).await.unwrap();
        assert!(c.pendiente().a_salvo());
        assert!(!d.join(id(1).as_str()).exists());

        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn preparar_dos_veces_lo_mismo_no_lo_duplica() {
        let d = dir("dup");
        let c = WriteQueue::open(&d).await.unwrap();
        c.stage(&id(1), b"mismo").await.unwrap();
        c.stage(&id(1), b"mismo").await.unwrap();

        assert_eq!(c.pendiente().trozos, 1);
        assert_eq!(c.pendiente().bytes, 5);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn el_directorio_depende_del_volumen() {
        let a = dir_de_volumen("/base", b"sal-a");
        let b = dir_de_volumen("/base", b"sal-b");
        assert_ne!(a, b, "dos volumenes no pueden compartir las pendientes");
    }
}

#[cfg(test)]
mod tests_con_almacen {
    use std::sync::Arc;

    use async_trait::async_trait;
    use libredisk_backend::{Backend, BackendError, OpendalBackend};
    use libredisk_raid::Layout;

    use super::*;
    use crate::ChunkStore;
    use crate::testutil::{clave, como_backends, contadores, reiniciar, total_gets};
    use crate::tree::{ROOT_KEY, Volume};

    fn rs() -> Layout {
        Layout::ReedSolomon { k: 4, m: 2 }
    }

    fn dir(nombre: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "libredisk-wb2-{}-{nombre}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Escribir con la cola puesta no puede tocar los proveedores, y lo escrito tiene que
    /// poder releerse igualmente.
    #[tokio::test]
    async fn escribir_no_sube_nada_pero_se_relee() {
        let d = dir("difiere");
        let layout = rs();
        let cs = contadores(layout.shard_count());

        let store = ChunkStore::new(layout, como_backends(&cs), clave())
            .unwrap()
            .with_writeback(WriteQueue::open(&d).await.unwrap());
        let (mut v, _) = Volume::create(store).await.unwrap();

        reiniciar(&cs);
        v.write_file("/notas.txt", &b"contenido diferido"[..])
            .await
            .unwrap();

        assert_eq!(total_gets(&cs), 0, "no se toca a los proveedores");
        let p = v.store().pendiente();
        assert!(p.trozos >= 2, "el fichero y los nodos del arbol");
        assert!(p.raiz, "y una raiz esperando");

        // Se relee: la cola es el unico sitio donde existe ese contenido.
        assert_eq!(
            v.read_at("/notas.txt", 0, 100).await.unwrap(),
            b"contenido diferido"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// **El invariante.** Antes de sincronizar, nadie de fuera puede ver la escritura.
    #[tokio::test]
    async fn hasta_que_no_se_sincroniza_el_volumen_se_ve_como_antes() {
        let d = dir("invariante");
        let layout = rs();
        let backends = crate::testutil::backends(layout.shard_count());

        {
            let store = ChunkStore::new(layout, backends.clone(), clave()).unwrap();
            let (mut v, _) = Volume::create(store).await.unwrap();
            v.write_file("/viejo.txt", &b"ya estaba"[..]).await.unwrap();
        }

        let store = ChunkStore::new(layout, backends.clone(), clave())
            .unwrap()
            .with_writeback(WriteQueue::open(&d).await.unwrap());
        let mut v = Volume::open(store).await.unwrap();
        v.write_file("/nuevo.txt", &b"todavia no"[..])
            .await
            .unwrap();

        // Un lector independiente sigue viendo el estado anterior.
        let otro = Volume::open(ChunkStore::new(layout, backends.clone(), clave()).unwrap())
            .await
            .unwrap();
        let visto = otro.read_dir("/").await.unwrap();
        assert!(visto.contains_key("viejo.txt"));
        assert!(
            !visto.contains_key("nuevo.txt"),
            "lo no sincronizado no puede verse desde fuera"
        );

        let informe = v.store().sync().await.unwrap();
        assert!(informe.todo_arriba() && informe.raiz_publicada);

        let otro = Volume::open(ChunkStore::new(layout, backends, clave()).unwrap())
            .await
            .unwrap();
        assert!(otro.read_dir("/").await.unwrap().contains_key("nuevo.txt"));

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Proveedor que acepta todo menos escribir fragmentos.
    struct NoEscribe(OpendalBackend);

    #[async_trait]
    impl Backend for NoEscribe {
        fn name(&self) -> &str {
            self.0.name()
        }
        async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackendError> {
            if key.starts_with("chunks/") {
                return Err(BackendError::NotFound("sin sitio, digamos".into()));
            }
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
        async fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
            self.0.list(prefix).await
        }
    }

    /// **La prueba que de verdad importa**: si las subidas fallan, la raiz NO se publica.
    ///
    /// Publicarla dejaria el volumen apuntando a nodos que no existen en ningun sitio.
    #[tokio::test]
    async fn si_falla_la_subida_la_raiz_no_se_publica() {
        let d = dir("falla");
        let layout = rs();

        // Tantos proveedores rotos que no se alcanza el minimo de fragmentos.
        let backends: Vec<Arc<dyn Backend>> = (0..layout.shard_count())
            .map(|i| {
                let mem = OpendalBackend::memory(format!("proveedor-{i}")).unwrap();
                if i >= 2 {
                    Arc::new(NoEscribe(mem)) as Arc<dyn Backend>
                } else {
                    Arc::new(mem) as Arc<dyn Backend>
                }
            })
            .collect();

        let store = ChunkStore::new(layout, backends.clone(), clave())
            .unwrap()
            .with_writeback(WriteQueue::open(&d).await.unwrap());
        let (mut v, _) = Volume::create(store).await.unwrap();
        v.write_file("/a.txt", &b"no va a llegar"[..])
            .await
            .unwrap();

        let informe = v.store().sync().await.unwrap();

        assert!(!informe.todo_arriba(), "las subidas tenian que fallar");
        assert!(
            !informe.raiz_publicada,
            "la raiz NO puede publicarse si algun trozo no subio"
        );
        assert!(
            v.store().writeback().unwrap().root().is_some(),
            "y se queda esperando, para poder reintentar"
        );

        for b in &backends {
            assert!(
                !b.exists(ROOT_KEY).await.unwrap(),
                "no puede haber raiz publicada"
            );
        }

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Reintentar tras arreglar el problema tiene que terminar el trabajo.
    #[tokio::test]
    async fn reintentar_despues_publica_la_raiz() {
        let d = dir("reintento");
        let layout = rs();
        let backends = crate::testutil::backends(layout.shard_count());

        {
            let store = ChunkStore::new(layout, backends.clone(), clave())
                .unwrap()
                .with_writeback(WriteQueue::open(&d).await.unwrap());
            let (mut v, _) = Volume::create(store).await.unwrap();
            v.write_file("/a.txt", &b"pendiente"[..]).await.unwrap();
            // Se cierra sin sincronizar, como si se fuera la luz.
        }

        // Otra ejecucion: la cola se recupera del disco y termina el trabajo.
        let store = ChunkStore::new(layout, backends.clone(), clave())
            .unwrap()
            .with_writeback(WriteQueue::open(&d).await.unwrap());
        assert!(!store.pendiente().a_salvo());

        let informe = store.sync().await.unwrap();
        assert!(informe.todo_arriba() && informe.raiz_publicada);
        assert!(store.pendiente().a_salvo());

        let v = Volume::open(ChunkStore::new(layout, backends, clave()).unwrap())
            .await
            .unwrap();
        assert_eq!(v.read_at("/a.txt", 0, 100).await.unwrap(), b"pendiente");

        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn sin_cola_sync_no_hace_nada() {
        let store = crate::testutil::store(rs());
        assert_eq!(store.sync().await.unwrap(), SyncReport::default());
        assert!(store.pendiente().a_salvo());
    }
}
