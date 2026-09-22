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

use crate::cuota::{Medidor, Nube};

pub mod aplicacion;
pub mod catalogo;
pub mod config;
pub mod conformance;
mod cuota;
pub mod live;
pub mod oauth;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("el objeto '{0}' no existe en el proveedor")]
    NotFound(String),
    #[error("no se pudo configurar el proveedor '{name}': {source}")]
    Config {
        name: String,
        source: Box<opendal::Error>,
    },
    #[error("no se pudo leer el fichero de configuracion {fichero}: {detalle}")]
    FicheroDeConfig { fichero: String, detalle: String },
    #[error("configuracion incompleta de '{proveedor}': falta {falta}")]
    EntornoIncompleto {
        proveedor: &'static str,
        falta: String,
    },
    #[error("no se pudo consultar el espacio de '{name}': {detalle}")]
    Cuota { name: String, detalle: String },
    #[error("no se pudo conectar la cuenta: {detalle}")]
    Oauth { detalle: String },
    #[error("fallo de E/S en el proveedor '{name}' sobre '{key}': {source}")]
    Io {
        name: String,
        key: String,
        source: Box<opendal::Error>,
    },
}

/// Un objeto tal y como lo ve una enumeracion: su clave y lo que ocupa.
///
/// El tamano viene con el listado y **no cuesta ninguna peticion extra**: los seis
/// proveedores soportados lo incluyen en la misma respuesta que ya devuelve los nombres
/// (`size` en Drive y OneDrive, `Size` en S3, `getcontentlength` en WebDAV, el `stat` del
/// directorio en el sistema de ficheros local). Por eso va aqui y no en un metodo aparte:
/// separarlos obligaria a enumerar dos veces para saber dos cosas que llegan juntas, y una
/// enumeracion completa contra una nube se mide en minutos.
///
/// Un proveedor que no lo diga se detecta: la suite de conformidad lo comprueba contra un
/// objeto de tamano conocido. Vale mas fallar ahi que informar de una ocupacion de cero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Objeto {
    /// Clave completa, comparable tal cual con lo que produce `ChunkId::shard_key`.
    pub clave: String,
    /// Bytes que ocupa el objeto en el proveedor.
    pub bytes: u64,
}

/// Lo que el proveedor dice de su propio espacio.
///
/// Los dos campos son opcionales, y un `None` significa "este proveedor no lo declara", que
/// no es lo mismo que "no se pudo preguntar": lo segundo es un `Err`.
///
/// La distincion importa porque hay proveedores **sin limite que declarar**: un bucket S3 se
/// factura por lo que ocupa y no tiene cupo, asi que pintar ahi una barra al 0 % seria
/// inventarse un total. Cuando no hay total, la interfaz dice que no lo hay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cuota {
    /// Capacidad total de la cuenta, si el proveedor declara una.
    pub total: Option<u64>,
    /// Ocupado en la cuenta **entera**, no solo por LibreDisk.
    ///
    /// Es lo que responden las APIs de cupo de Drive y OneDrive, y es la cifra honesta para
    /// una barra: lo que le queda libre al usuario depende de sus fotos tanto como de sus
    /// fragmentos. Lo que ocupa el volumen es otra medida distinta, y se obtiene enumerando
    /// (ver [`Objeto`]).
    pub usado: Option<u64>,
}

impl Cuota {
    /// Ni total ni ocupado: el proveedor no dice nada de su espacio.
    pub const MUDA: Self = Self {
        total: None,
        usado: None,
    };

    /// Libre segun el proveedor, cuando dice las dos cosas.
    pub fn libre(&self) -> Option<u64> {
        match (self.total, self.usado) {
            (Some(t), Some(u)) => Some(t.saturating_sub(u)),
            _ => None,
        }
    }
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

    /// Enumera las claves que cuelgan de un prefijo.
    ///
    /// Existe por una razon medida, no por simetria con el resto del trait: `scrub` necesita
    /// saber que fragmentos siguen vivos, y preguntarlo de uno en uno con
    /// [`Backend::exists`] no escala. Contra Google Drive una consulta por algo que no existe
    /// cuesta ~6,5 s; un volumen de 1 TB son ~400 000 fragmentos, que en serie serian
    /// semanas. Enumerar cambia ese coste por uno que ya no depende del numero de fragmentos.
    ///
    /// **Una llamada a `list` no es una peticion.** En un almacen plano (S3 y compatibles) se
    /// le parece, pero OneDrive y Google Drive son jerarquicos y OpenDAL recorre el arbol
    /// nivel a nivel: cuesta una peticion por directorio. Medido con cinco pasadas contra
    /// cuentas reales de las dos nubes, un `list` sobre un prefijo vacio cuesta ~1,1 s en
    /// ambas, y del orden de 3 a 4 s por directorio cuando hay algo debajo. Con la disposicion
    /// que produce `ChunkId::shard_key` (`chunks/<2 hex>/<hash>.<n>`) son 257 directorios por
    /// proveedor: una enumeracion completa esta entre 5 y 17 minutos, no es un instante. Sigue
    /// siendo el orden de magnitud correcto frente a preguntar 400 000 veces, pero no es
    /// gratis y conviene no contarlo como si lo fuera. Cifras y metodo en
    /// `docs/VIABILIDAD.md`, seccion 7.
    ///
    /// Contrato del que depende el resto de LibreDisk:
    ///
    /// - Devuelve **claves completas**, no relativas al prefijo, y baja por los subniveles.
    ///   Quien enumera las compara contra lo que produce `ChunkId::shard_key`, asi que
    ///   tienen que servir tal cual, sin recomponerlas.
    /// - Solo objetos. Los "directorios" intermedios que algunos proveedores materializan no
    ///   aparecen: aguas arriba nadie sabe que existen.
    /// - Un prefijo sin nada debajo devuelve la lista vacia. **No** es `NotFound`: un volumen
    ///   recien creado esta vacio, no roto.
    /// - Cada entrada trae **el tamano ademas del nombre**. Los proveedores lo mandan en la
    ///   misma respuesta, asi que no cuesta nada pedirlo, y evita tener que enumerar otra vez
    ///   para medir lo que ocupa el volumen.
    /// - **Sin orden garantizado.** Quien necesite determinismo, que ordene.
    ///
    /// La lista viene entera en memoria. A ~75 bytes por clave, ese mismo volumen de 1 TB son
    /// ~30 MB: asumible hoy. El dia que deje de serlo habra que paginar, y esta firma
    /// cambiara.
    async fn list(&self, prefix: &str) -> Result<Vec<Objeto>, BackendError>;

    /// Lo que el proveedor dice de su propio espacio: cuanto cabe y cuanto hay ocupado.
    ///
    /// Es **una peticion**, no una enumeracion: se le pregunta al servicio por el cupo de la
    /// cuenta. Se puede llamar al abrir una ventana sin que se note, al reves que `list`.
    ///
    /// Cuidado con lo que significa `usado`: es la cuenta entera del usuario, fotos y
    /// correos incluidos, no lo que ocupa LibreDisk. Son dos cifras distintas y conviene no
    /// confundirlas al pintarlas.
    ///
    /// Por defecto, [`Cuota::MUDA`]: un proveedor que no implemente esto no declara nada, y
    /// eso es lo que se muestra. Devolver ceros seria decir que esta vacio.
    async fn cuota(&self) -> Result<Cuota, BackendError> {
        Ok(Cuota::MUDA)
    }
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
    /// A quien preguntarle cuanto espacio queda. No lo sabe OpenDAL: el cupo de una cuenta
    /// no es un objeto, y cada servicio lo cuenta en su propia API.
    medidor: Medidor,
}

impl OpendalBackend {
    fn build(
        name: impl Into<String>,
        builder: impl opendal::Builder,
        medidor: Medidor,
    ) -> Result<Self, BackendError> {
        let name = name.into();
        let op = Operator::new(builder)
            .map_err(|source| BackendError::Config {
                name: name.clone(),
                source: Box::new(source),
            })?
            // Reintentar no es una optimizacion, es correccion. Probando contra cuentas
            // reales se perdio una escritura suelta a OneDrive y una lectura suelta a
            // Drive, las dos transitorias: la primera dejo un volumen con menos
            // redundancia de la pedida sin que nadie se enterara, y la segunda hizo que
            // un scrub se saltara medio arbol.
            //
            // Es seguro en las cinco operaciones del trait, y lo es **porque** los trozos
            // son inmutables: repetir un put reescribe los mismos bytes en la misma clave,
            // y get, exists, list y delete no tienen efectos que se acumulen.
            .layer(opendal::layers::RetryLayer::new().with_jitter());
        Ok(Self { name, op, medidor })
    }

    /// Proveedor en memoria. Solo para pruebas.
    pub fn memory(name: impl Into<String>) -> Result<Self, BackendError> {
        Self::build(name, services::Memory::default(), Medidor::Muda)
    }

    /// Directorio local. Util para pruebas y para usar un disco propio como un proveedor mas.
    pub fn fs(name: impl Into<String>, root: &str) -> Result<Self, BackendError> {
        Self::build(
            name,
            services::Fs::default().root(root),
            Medidor::Disco(root.into()),
        )
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
        // Un bucket no tiene cupo que declarar: se factura por lo que ocupa. Preguntarle
        // cuanto le queda no tiene respuesta, y responder "0 de 0" seria inventarsela.
        Self::build(name, builder, Medidor::Muda)
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
        Self::build(
            name,
            builder,
            Medidor::Webdav {
                endpoint: endpoint.to_string(),
                root: root.to_string(),
                creds: creds.map(|(u, p)| (u.to_string(), p.to_string())),
            },
        )
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
        Self::build(name, builder, Medidor::nube(Nube::Onedrive, creds))
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
        Self::build(name, builder, Medidor::nube(Nube::Gdrive, creds))
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

    async fn list(&self, prefix: &str) -> Result<Vec<Objeto>, BackendError> {
        match self.op.list_with(prefix).recursive(true).await {
            Ok(entradas) => Ok(entradas
                .into_iter()
                // Algunos proveedores materializan los directorios y los devuelven como una
                // entrada mas. Aguas arriba solo existen objetos.
                .filter(|e| e.metadata().is_file())
                .map(|e| Objeto {
                    bytes: e.metadata().content_length(),
                    clave: e.path().to_string(),
                })
                .collect()),
            // Un prefijo que todavia no existe no es un fallo: es un volumen vacio.
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(self.io_err(prefix)(e)),
        }
    }

    async fn cuota(&self) -> Result<Cuota, BackendError> {
        self.medidor.medir(&self.name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Solo los nombres, para los tests a los que el tamano les da igual.
    fn claves(objetos: Vec<Objeto>) -> Vec<String> {
        objetos.into_iter().map(|o| o.clave).collect()
    }

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

    #[tokio::test]
    async fn listar_enumera_claves_completas_y_recursivas() {
        let b = OpendalBackend::memory("prueba").unwrap();
        for k in [
            "chunks/aa/uno.000",
            "chunks/aa/dos.001",
            "chunks/bb/tres.000",
        ] {
            b.put(k, b"x".to_vec()).await.unwrap();
        }
        b.put("otra-cosa/z", b"x".to_vec()).await.unwrap();

        let mut vistas = claves(b.list("chunks/").await.unwrap());
        vistas.sort();
        assert_eq!(
            vistas,
            vec![
                "chunks/aa/dos.001",
                "chunks/aa/uno.000",
                "chunks/bb/tres.000"
            ],
            "list debe bajar por los subniveles, devolver la clave completa y nada de fuera \
             del prefijo"
        );
    }

    /// Si se colara un directorio, `scrub` lo compararia contra las claves de `shard_key` y
    /// contaria un fragmento que no existe.
    #[tokio::test]
    async fn listar_no_devuelve_directorios() {
        let b = OpendalBackend::memory("prueba").unwrap();
        b.put("chunks/aa/uno.000", b"x".to_vec()).await.unwrap();
        let vistas = b.list("chunks/").await.unwrap();
        assert!(
            vistas.iter().all(|o| !o.clave.ends_with('/')),
            "se colo una entrada de directorio: {vistas:?}"
        );
    }

    #[tokio::test]
    async fn listar_un_prefijo_sin_nada_da_lista_vacia_no_error() {
        let b = OpendalBackend::memory("prueba").unwrap();
        // Un volumen recien creado: el prefijo todavia no existe.
        assert!(b.list("chunks/").await.unwrap().is_empty());
    }

    /// El caso del prefijo inexistente contra un backend que de verdad tiene directorios:
    /// en memoria no hay nada que pueda dar `NotFound`, en disco si.
    #[tokio::test]
    async fn listar_en_directorio_local() {
        let dir = std::env::temp_dir().join(format!("libredisk-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let b = OpendalBackend::fs("disco", dir.to_str().unwrap()).unwrap();
        assert!(
            b.list("chunks/").await.unwrap().is_empty(),
            "un prefijo que no existe en disco no es un error"
        );

        b.put("chunks/aa/uno.000", vec![1, 2, 3]).await.unwrap();
        assert_eq!(
            b.list("chunks/").await.unwrap(),
            vec![Objeto {
                clave: "chunks/aa/uno.000".into(),
                bytes: 3
            }],
            "el listado trae la clave y el tamano, que es de donde sale lo que ocupa el volumen"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
