//! Abstraccion de proveedor de almacenamiento.
//!
//! El resto de LibreDisk no sabe si detras hay un bucket S3, un Nextcloud o un directorio
//! local: solo ve el trait [`Backend`], un almacen de objetos con cuatro operaciones.
//!
//! Deliberadamente **no** hay `rename` ni escritura parcial. Los object stores no los
//! ofrecen de forma atomica, asi que exponerlos aqui invitaria al resto del codigo a
//! depender de algo que no existe. Los trozos son inmutables; eso es todo lo que hace falta.

use async_trait::async_trait;
use opendal::{Operator, services};

pub mod conformance;
pub mod live;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("el objeto '{0}' no existe en el proveedor")]
    NotFound(String),
    #[error("no se pudo configurar el proveedor '{name}': {source}")]
    Config {
        name: String,
        source: Box<opendal::Error>,
    },
    #[error("configuracion incompleta de '{proveedor}': falta {falta}")]
    EntornoIncompleto {
        proveedor: &'static str,
        falta: String,
    },
    #[error("fallo de E/S en el proveedor '{name}' sobre '{key}': {source}")]
    Io {
        name: String,
        key: String,
        source: Box<opendal::Error>,
    },
}

/// Un proveedor de almacenamiento donde viven fragmentos.
///
/// Las claves son rutas con `/` como separador. Las implementaciones deben ser seguras de
/// compartir entre tareas: el motor RAID habla con todos los proveedores en paralelo.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Nombre con el que el usuario configuro este proveedor.
    fn name(&self) -> &str;

    /// Escribe un objeto. Debe sobrescribir si ya existe.
    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackendError>;

    /// Lee un objeto entero.
    async fn get(&self, key: &str) -> Result<Vec<u8>, BackendError>;

    /// Borra un objeto. Borrar algo que no existe no es un error.
    async fn delete(&self, key: &str) -> Result<(), BackendError>;

    /// Indica si el objeto existe.
    async fn exists(&self, key: &str) -> Result<bool, BackendError>;
}

/// Credenciales OAuth2 de una nube de consumo (OneDrive, Google Drive).
///
/// Estas nubes no tienen claves estaticas como S3: hay que pasar por el consentimiento del
/// usuario en un navegador. Ver [`docs/PRUEBAS-PROVEEDORES.md`] para obtenerlas.
#[derive(Clone)]
pub enum OauthCreds {
    /// Token de acceso ya obtenido. Caduca en una hora: sirve para una prueba suelta, no
    /// para un volumen que debe seguir vivo manana.
    Acceso(String),
    /// Token de refresco mas las credenciales de la aplicacion. OpenDAL renueva el acceso
    /// solo, asi que es lo unico valido para uso real.
    Refresco {
        refresh_token: String,
        client_id: String,
        client_secret: String,
    },
}

/// Se imprime redactado a proposito: un `{:?}` descuidado en un log o en la salida de un
/// test fallido publicaria el token.
impl std::fmt::Debug for OauthCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Acceso(_) => f.write_str("OauthCreds::Acceso(<redactado>)"),
            Self::Refresco { client_id, .. } => f
                .debug_struct("OauthCreds::Refresco")
                .field("client_id", client_id)
                .field("refresh_token", &"<redactado>")
                .field("client_secret", &"<redactado>")
                .finish(),
        }
    }
}

/// Implementacion de [`Backend`] sobre OpenDAL, que aporta los ~40 proveedores.
pub struct OpendalBackend {
    name: String,
    op: Operator,
}

impl OpendalBackend {
    fn build(
        name: impl Into<String>,
        builder: impl opendal::Builder,
    ) -> Result<Self, BackendError> {
        let name = name.into();
        let op = Operator::new(builder)
            .map_err(|source| BackendError::Config {
                name: name.clone(),
                source: Box::new(source),
            })?
            .finish();
        Ok(Self { name, op })
    }

    /// Proveedor en memoria. Solo para pruebas.
    pub fn memory(name: impl Into<String>) -> Result<Self, BackendError> {
        Self::build(name, services::Memory::default())
    }

    /// Directorio local. Util para pruebas y para usar un disco propio como un proveedor mas.
    pub fn fs(name: impl Into<String>, root: &str) -> Result<Self, BackendError> {
        Self::build(name, services::Fs::default().root(root))
    }

    /// Cualquier servicio compatible con S3: Backblaze B2, Cloudflare R2, Wasabi, MinIO.
    ///
    /// Con `creds` a `None` se usan las variables de entorno habituales de AWS.
    pub fn s3(
        name: impl Into<String>,
        bucket: &str,
        endpoint: &str,
        region: &str,
        creds: Option<(&str, &str)>,
    ) -> Result<Self, BackendError> {
        let mut builder = services::S3::default()
            .bucket(bucket)
            .endpoint(endpoint)
            .region(region);
        if let Some((key_id, secret)) = creds {
            builder = builder.access_key_id(key_id).secret_access_key(secret);
        }
        Self::build(name, builder)
    }

    /// WebDAV, que es como se habla con Nextcloud.
    pub fn webdav(
        name: impl Into<String>,
        endpoint: &str,
        root: &str,
        creds: Option<(&str, &str)>,
    ) -> Result<Self, BackendError> {
        let mut builder = services::Webdav::default().endpoint(endpoint).root(root);
        if let Some((usuario, contrasena)) = creds {
            builder = builder.username(usuario).password(contrasena);
        }
        Self::build(name, builder)
    }

    /// OneDrive, personal o de empresa.
    ///
    /// `root` debe ser una carpeta dedicada: LibreDisk escribe blobs opacos y no tiene
    /// sentido mezclarlos con los archivos que el usuario ve en la web del proveedor.
    pub fn onedrive(
        name: impl Into<String>,
        root: &str,
        creds: &OauthCreds,
    ) -> Result<Self, BackendError> {
        let builder = services::Onedrive::default().root(root);
        let builder = match creds {
            OauthCreds::Acceso(token) => builder.access_token(token),
            OauthCreds::Refresco {
                refresh_token,
                client_id,
                client_secret,
            } => builder
                .refresh_token(refresh_token)
                .client_id(client_id)
                .client_secret(client_secret),
        };
        Self::build(name, builder)
    }

    /// Google Drive. Mismo aviso sobre `root` que en [`OpendalBackend::onedrive`].
    pub fn gdrive(
        name: impl Into<String>,
        root: &str,
        creds: &OauthCreds,
    ) -> Result<Self, BackendError> {
        let builder = services::Gdrive::default().root(root);
        let builder = match creds {
            OauthCreds::Acceso(token) => builder.access_token(token),
            OauthCreds::Refresco {
                refresh_token,
                client_id,
                client_secret,
            } => builder
                .refresh_token(refresh_token)
                .client_id(client_id)
                .client_secret(client_secret),
        };
        Self::build(name, builder)
    }

    fn io_err<'a>(&'a self, key: &'a str) -> impl Fn(opendal::Error) -> BackendError + 'a {
        move |source| BackendError::Io {
            name: self.name.clone(),
            key: key.to_string(),
            // Encajonado: `opendal::Error` ocupa ~136 bytes y engordaria cada `Result`
            // del arbol de llamadas, incluidos los caminos de exito.
            source: Box::new(source),
        }
    }
}

/// Solo el nombre, por el mismo motivo que [`OauthCreds`]: el `Operator` lleva dentro las
/// credenciales con las que se construyo.
impl std::fmt::Debug for OpendalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpendalBackend")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Backend for OpendalBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackendError> {
        self.op
            .write(key, data)
            .await
            .map(|_| ())
            .map_err(self.io_err(key))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, BackendError> {
        match self.op.read(key).await {
            Ok(buf) => Ok(buf.to_vec()),
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => {
                Err(BackendError::NotFound(key.to_string()))
            }
            Err(e) => Err(self.io_err(key)(e)),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), BackendError> {
        // OpenDAL ya trata el borrado de algo inexistente como exito.
        self.op.delete(key).await.map_err(self.io_err(key))
    }

    async fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.op.exists(key).await.map_err(self.io_err(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ida_y_vuelta_en_memoria() {
        let b = OpendalBackend::memory("prueba").unwrap();
        assert_eq!(b.name(), "prueba");

        assert!(!b.exists("a/b").await.unwrap());
        b.put("a/b", b"hola".to_vec()).await.unwrap();
        assert!(b.exists("a/b").await.unwrap());
        assert_eq!(b.get("a/b").await.unwrap(), b"hola");

        b.delete("a/b").await.unwrap();
        assert!(!b.exists("a/b").await.unwrap());
    }

    #[tokio::test]
    async fn leer_lo_que_no_existe_da_not_found() {
        let b = OpendalBackend::memory("prueba").unwrap();
        assert!(matches!(
            b.get("fantasma").await,
            Err(BackendError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn borrar_lo_que_no_existe_no_es_error() {
        let b = OpendalBackend::memory("prueba").unwrap();
        b.delete("fantasma").await.unwrap();
    }

    #[tokio::test]
    async fn escribir_sobrescribe() {
        let b = OpendalBackend::memory("prueba").unwrap();
        b.put("k", b"viejo".to_vec()).await.unwrap();
        b.put("k", b"nuevo".to_vec()).await.unwrap();
        assert_eq!(b.get("k").await.unwrap(), b"nuevo");
    }

    #[tokio::test]
    async fn backend_de_directorio_local() {
        let dir = std::env::temp_dir().join(format!("libredisk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let b = OpendalBackend::fs("disco", dir.to_str().unwrap()).unwrap();
        b.put("x/y.bin", vec![7u8; 100]).await.unwrap();
        assert_eq!(b.get("x/y.bin").await.unwrap(), vec![7u8; 100]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// El trait debe poder usarse como objeto: el motor RAID guarda `Vec<Arc<dyn Backend>>`
    /// con proveedores de tipos distintos mezclados.
    #[tokio::test]
    async fn el_trait_es_usable_como_objeto() {
        let backends: Vec<std::sync::Arc<dyn Backend>> =
            vec![std::sync::Arc::new(OpendalBackend::memory("uno").unwrap())];
        backends[0].put("k", b"v".to_vec()).await.unwrap();
        assert_eq!(backends[0].get("k").await.unwrap(), b"v");
    }
}
