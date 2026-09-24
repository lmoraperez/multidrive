//! Utilidades compartidas por los tests de este crate.
//!
//! Vive en su propio modulo porque tres modulos de prueba necesitan lo mismo: un proveedor
//! que cuente las llamadas que recibe. Las afirmaciones sobre coste de este crate —"revisar
//! n trozos no puede costar n llamadas por proveedor", "leer dentro de un trozo no toca los
//! otros diecinueve"— solo valen si se cuentan de verdad en vez de suponerlas.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use libredisk_backend::{Backend, BackendError, Objeto, OpendalBackend};
use libredisk_crypto::MasterKey;
use libredisk_raid::Layout;

use crate::ChunkStore;

/// Clave de pruebas.
///
/// Los parametros de Argon2 van flojos a proposito: aqui no se mide el cifrado, y los de
/// verdad harian que cada test tardara segundos.
pub fn clave() -> MasterKey {
    MasterKey::derive_with("frase de prueba", b"sal-de-prueba-16", 8, 1).unwrap()
}

/// Un proveedor en memoria que cuenta cuantas veces se le llama.
pub struct Contador {
    inner: OpendalBackend,
    pub gets: AtomicUsize,
    pub exists: AtomicUsize,
    pub lists: AtomicUsize,
}

impl Contador {
    pub fn nuevo(nombre: impl Into<String>) -> Self {
        Self {
            inner: OpendalBackend::memory(nombre.into()).unwrap(),
            gets: AtomicUsize::new(0),
            exists: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
        }
    }

    pub fn reiniciar(&self) {
        self.gets.store(0, Ordering::Relaxed);
        self.exists.store(0, Ordering::Relaxed);
        self.lists.store(0, Ordering::Relaxed);
    }
}

#[async_trait]
impl Backend for Contador {
    fn name(&self) -> &str {
        self.inner.name()
    }
    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackendError> {
        self.inner.put(key, data).await
    }
    async fn get(&self, key: &str) -> Result<Vec<u8>, BackendError> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        self.inner.get(key).await
    }
    async fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.inner.delete(key).await
    }
    async fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.exists.fetch_add(1, Ordering::Relaxed);
        self.inner.exists(key).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<Objeto>, BackendError> {
        self.lists.fetch_add(1, Ordering::Relaxed);
        self.inner.list(prefix).await
    }
}

/// `n` proveedores que cuentan.
pub fn contadores(n: usize) -> Vec<Arc<Contador>> {
    (0..n)
        .map(|i| Arc::new(Contador::nuevo(format!("proveedor-{i}"))))
        .collect()
}

/// Los mismos, vistos como proveedores corrientes.
pub fn como_backends(cs: &[Arc<Contador>]) -> Vec<Arc<dyn Backend>> {
    cs.iter().map(|c| c.clone() as Arc<dyn Backend>).collect()
}

pub fn reiniciar(cs: &[Arc<Contador>]) {
    for c in cs {
        c.reiniciar();
    }
}

pub fn total_gets(cs: &[Arc<Contador>]) -> usize {
    cs.iter().map(|c| c.gets.load(Ordering::Relaxed)).sum()
}

/// Proveedores en memoria corrientes, sin contar nada.
pub fn backends(n: usize) -> Vec<Arc<dyn Backend>> {
    (0..n)
        .map(|i| {
            Arc::new(OpendalBackend::memory(format!("proveedor-{i}")).unwrap()) as Arc<dyn Backend>
        })
        .collect()
}

/// Un almacen nuevo sobre proveedores en memoria recien creados.
pub fn store(layout: Layout) -> ChunkStore {
    ChunkStore::new(layout, backends(layout.shard_count()), clave()).unwrap()
}

/// Un almacen sobre proveedores que ya existen.
///
/// Hace falta para reabrir un volumen: el [`ChunkStore`] se consume al crear el volumen, y
/// comprobar que la raiz persiste exige construir otro sobre los mismos proveedores.
pub fn store_sobre(backends: &[Arc<dyn Backend>], layout: Layout) -> ChunkStore {
    ChunkStore::new(layout, backends.to_vec(), clave()).unwrap()
}

/// Un proveedor que no se deja enumerar.
///
/// Lo usan dos modulos, y por motivos que son el mismo mirado desde dos lados: que un
/// proveedor mudo **no se tome por vacio**. En `scrub` eso convertiria una caida pasajera en
/// una perdida total sobre el papel; al retirar un volumen, haria que se borrara a medias
/// diciendo que termino.
pub struct NoEnumera(pub OpendalBackend);

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
    async fn list(&self, _prefix: &str) -> Result<Vec<Objeto>, BackendError> {
        Err(BackendError::NotFound("el proveedor no responde".into()))
    }
}

/// Un proveedor que no contesta a nada.
///
/// Distinto de [`NoEnumera`], que si sirve objetos pero no los lista. Este falla tambien al
/// leer, que es lo que hace un token caducado o un corte de red.
pub struct NoResponde(pub OpendalBackend);

#[async_trait]
impl Backend for NoResponde {
    fn name(&self) -> &str {
        self.0.name()
    }
    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackendError> {
        self.0.put(key, data).await
    }
    /// **No** es `NotFound`: eso significaria que el objeto no esta, que es justo lo que hay
    /// que distinguir de "no se pudo preguntar".
    async fn get(&self, key: &str) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Cuota {
            name: self.0.name().to_string(),
            detalle: format!("el proveedor no responde (pedian '{key}')"),
        })
    }
    async fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.0.delete(key).await
    }
    async fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.0.exists(key).await
    }
    async fn list(&self, _prefix: &str) -> Result<Vec<Objeto>, BackendError> {
        Err(BackendError::NotFound("el proveedor no responde".into()))
    }
}

/// Unos cuantos proveedores en memoria, sin contar llamadas.
pub fn memoria(n: usize) -> Vec<Arc<dyn Backend>> {
    (0..n)
        .map(|i| Arc::new(OpendalBackend::memory(format!("p{i}")).unwrap()) as Arc<dyn Backend>)
        .collect()
}
