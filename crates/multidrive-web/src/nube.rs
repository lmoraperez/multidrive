//! Las dos nubes, leidas desde el navegador.
//!
//! Hacen lo mismo que los backends de OpenDAL del escritorio, pero **solo leen**: la version web
//! es de solo lectura, y un `put` que no existe no se puede usar por descuido. Y las claves
//! tienen que caer exactamente donde las deja OpenDAL, o el volumen parece vacio.
//!
//! # Donde deja OpenDAL cada clave
//!
//! Igual en las dos: la carpeta raiz de la ranura (`/multidrive` si no se dijo otra) y, debajo,
//! la clave tal cual, con sus `/` convertidas en carpetas. `chunks/ab/<hash>.0` es la carpeta
//! `chunks`, dentro la `ab`, y dentro el fichero.
//!
//! - **OneDrive** direcciona por ruta: una peticion para saber donde descargar y otra para
//!   descargar.
//! - **Drive** no tiene rutas: cada fichero tiene un id y uno o mas padres. Hay que bajar carpeta
//!   a carpeta preguntando por el nombre, como hace OpenDAL, y por eso se recuerdan los ids de
//!   las carpetas ya vistas. Sin eso, cada trozo costaria cuatro peticiones en vez de dos.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use multidrive_backend_api::{Backend, BackendError, Objeto};
use send_wrapper::SendWrapper;

use crate::red;

/// De que nube es una cuenta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tipo {
    Gdrive,
    Onedrive,
}

impl Tipo {
    /// El tipo de una ranura por su nombre: `gdrive2` es una cuenta de Drive.
    ///
    /// Es la misma regla que sigue el escritorio --el nombre de la ranura es el tipo con un
    /// numero detras--, y la cabecera solo guarda nombres. `None` si es de un tipo que el
    /// navegador no sabe leer: una carpeta local, un S3, un WebDAV.
    pub fn de_ranura(nombre: &str) -> Option<Self> {
        match nombre.trim_end_matches(|c: char| c.is_ascii_digit()) {
            "gdrive" => Some(Self::Gdrive),
            "onedrive" => Some(Self::Onedrive),
            _ => None,
        }
    }
}

/// La raiz de una ranura cuando nadie dijo otra. La misma que usa el escritorio.
pub const RAIZ_POR_DEFECTO: &str = "/multidrive";

/// Una cuenta conectada desde el navegador.
pub struct Nube {
    nombre: String,
    tipo: Tipo,
    /// Las carpetas de la raiz, ya separadas.
    raiz: Vec<String>,
    /// Se cambia sin rehacer la cuenta: el de Drive dura una hora, y volver a entrar no tiene
    /// por que perder lo que ya se sabe de las carpetas.
    token: Mutex<String>,
    /// Ruta de carpeta → id en Drive. Ver el modulo.
    carpetas: Mutex<HashMap<String, String>>,
}

impl Nube {
    pub fn nueva(nombre: &str, tipo: Tipo, raiz: &str, token: &str) -> Self {
        Self {
            nombre: nombre.to_string(),
            tipo,
            raiz: segmentos(raiz),
            token: Mutex::new(token.to_string()),
            carpetas: Mutex::new(HashMap::new()),
        }
    }

    pub fn tipo(&self) -> Tipo {
        self.tipo
    }

    pub fn renovar(&self, token: &str) {
        *self
            .token
            .lock()
            .expect("nadie suelta este cerrojo a medias") = token.to_string();
    }

    fn token(&self) -> String {
        self.token
            .lock()
            .expect("nadie suelta este cerrojo a medias")
            .clone()
    }

    async fn leer(&self, clave: &str) -> Result<Vec<u8>, BackendError> {
        let ruta = ruta_completa(&self.raiz, clave);
        match self.tipo {
            Tipo::Onedrive => self.leer_onedrive(clave, &ruta).await,
            Tipo::Gdrive => self.leer_gdrive(clave, &ruta).await,
        }
    }

    async fn leer_onedrive(&self, clave: &str, ruta: &[String]) -> Result<Vec<u8>, BackendError> {
        let camino: Vec<String> = ruta.iter().map(|s| red::codificar(s)).collect();
        let url = format!(
            "https://graph.microsoft.com/v1.0/me/drive/root:/{}",
            camino.join("/")
        );
        let token = self.token();
        let r = self.pedir(clave, &url, Some(&token)).await?;
        let meta: serde_json::Value = serde_json::from_slice(&r.cuerpo)
            .map_err(|e| self.fallo(clave, format!("Graph no devolvio JSON: {e}")))?;

        // Se descarga por la URL que da Graph y no por `/content`. `/content` redirige a otro
        // dominio, y esa redireccion no siempre trae las cabeceras de CORS que el navegador
        // exige; la URL directa si, y ya va firmada, asi que no lleva token.
        let Some(descarga) = meta["@microsoft.graph.downloadUrl"].as_str() else {
            return Err(self.fallo(clave, "Graph no dio por donde descargarlo".into()));
        };
        Ok(self.pedir(clave, descarga, None).await?.cuerpo)
    }

    async fn leer_gdrive(&self, clave: &str, ruta: &[String]) -> Result<Vec<u8>, BackendError> {
        let token = self.token();
        let (nombre, carpetas) = ruta
            .split_last()
            .ok_or_else(|| self.fallo(clave, "una clave vacia no es un fichero".into()))?;

        // «Mi unidad» se llama `root` en la API, sea cual sea la cuenta.
        let mut padre = "root".to_string();
        let mut camino = String::new();
        for carpeta in carpetas {
            camino.push('/');
            camino.push_str(carpeta);
            let sabido = self
                .carpetas
                .lock()
                .expect("ver arriba")
                .get(&camino)
                .cloned();
            padre = match sabido {
                Some(id) => id,
                None => {
                    let id = self.buscar_gdrive(clave, &padre, carpeta, &token).await?;
                    self.carpetas
                        .lock()
                        .expect("ver arriba")
                        .insert(camino.clone(), id.clone());
                    id
                }
            };
        }

        let id = self.buscar_gdrive(clave, &padre, nombre, &token).await?;
        let url = format!("https://www.googleapis.com/drive/v3/files/{id}?alt=media");
        Ok(self.pedir(clave, &url, Some(&token)).await?.cuerpo)
    }

    /// El id de lo que se llame `nombre` dentro de `padre`. `NotFound` si no hay nada.
    async fn buscar_gdrive(
        &self,
        clave: &str,
        padre: &str,
        nombre: &str,
        token: &str,
    ) -> Result<String, BackendError> {
        let q = format!(
            "name = '{}' and '{}' in parents and trashed = false",
            escapar_consulta(nombre),
            escapar_consulta(padre)
        );
        let url = format!(
            "https://www.googleapis.com/drive/v3/files?q={}&fields=files(id)&pageSize=1&spaces=drive",
            red::codificar(&q)
        );
        let r = self.pedir(clave, &url, Some(token)).await?;
        let lista: serde_json::Value = serde_json::from_slice(&r.cuerpo)
            .map_err(|e| self.fallo(clave, format!("Drive no devolvio JSON: {e}")))?;
        lista["files"][0]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BackendError::NotFound(clave.to_string()))
    }

    /// Un GET que ya traduce los codigos que importan: 404 es que no esta, y 401 es que la
    /// sesion caduco, que en la web pasa cada hora y tiene que decirse como tal.
    async fn pedir(
        &self,
        clave: &str,
        url: &str,
        token: Option<&str>,
    ) -> Result<red::Respuesta, BackendError> {
        let r = red::get(url, token)
            .await
            .map_err(|e| self.fallo(clave, format!("no se pudo llegar: {e}")))?;
        match r.estado {
            _ if r.ok() => Ok(r),
            404 => Err(BackendError::NotFound(clave.to_string())),
            401 => Err(BackendError::Oauth {
                detalle: format!(
                    "la sesion de '{}' ha caducado; vuelve a entrar en esa cuenta",
                    self.nombre
                ),
            }),
            e => Err(self.fallo(clave, format!("respondio {e}: {}", r.texto()))),
        }
    }

    fn fallo(&self, clave: &str, detalle: String) -> BackendError {
        BackendError::Io {
            name: self.nombre.clone(),
            key: clave.to_string(),
            source: detalle.into(),
        }
    }

    fn solo_lectura(&self, clave: &str) -> BackendError {
        self.fallo(clave, "la version web es de solo lectura".into())
    }
}

// Los futuros van dentro de `SendWrapper` porque el trait los pide `Send` y los de `fetch` no
// lo son: no hay hilos en el navegador. `SendWrapper` no lo finge, lo comprueba, y si alguien
// llegara a moverlo de hilo daria un panico en vez de una carrera.
#[async_trait]
impl Backend for Nube {
    fn name(&self) -> &str {
        &self.nombre
    }

    async fn put(&self, key: &str, _data: Vec<u8>) -> Result<(), BackendError> {
        Err(self.solo_lectura(key))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, BackendError> {
        SendWrapper::new(self.leer(key)).await
    }

    async fn delete(&self, key: &str) -> Result<(), BackendError> {
        Err(self.solo_lectura(key))
    }

    async fn exists(&self, key: &str) -> Result<bool, BackendError> {
        match self.get(key).await {
            Ok(_) => Ok(true),
            Err(BackendError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Enumerar solo lo usan `scrub` y la reparacion, que son de quien escribe.
    async fn list(&self, prefix: &str) -> Result<Vec<Objeto>, BackendError> {
        Err(self.fallo(
            prefix,
            "la version web no enumera: eso es para revisar o reparar el volumen".into(),
        ))
    }
}

/// Las partes de una ruta, sin vacias: `/multidrive/` y `multidrive` son lo mismo.
fn segmentos(ruta: &str) -> Vec<String> {
    ruta.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// La raiz de la ranura y la clave, juntas.
fn ruta_completa(raiz: &[String], clave: &str) -> Vec<String> {
    let mut todo = raiz.to_vec();
    todo.extend(segmentos(clave));
    todo
}

/// Un literal dentro de una consulta de Drive va entre comillas simples, y dentro solo hay
/// que escapar la comilla y la barra. Los nombres de MultiDrive no llevan ninguna de las dos,
/// pero la raiz la escribe el usuario.
fn escapar_consulta(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_tipo_sale_del_nombre_de_la_ranura() {
        assert_eq!(Tipo::de_ranura("gdrive"), Some(Tipo::Gdrive));
        assert_eq!(Tipo::de_ranura("gdrive2"), Some(Tipo::Gdrive));
        assert_eq!(Tipo::de_ranura("onedrive13"), Some(Tipo::Onedrive));
        // Lo que el navegador no sabe leer no se disfraza de otra cosa.
        assert_eq!(Tipo::de_ranura("s3"), None);
        assert_eq!(Tipo::de_ranura("fs2"), None);
        assert_eq!(Tipo::de_ranura("webdav"), None);
    }

    /// Tiene que caer donde lo dejo OpenDAL, o el volumen parece vacio.
    #[test]
    fn la_clave_cuelga_de_la_raiz_carpeta_a_carpeta() {
        let raiz = segmentos(RAIZ_POR_DEFECTO);
        assert_eq!(
            ruta_completa(&raiz, "chunks/ab/abcdef.0"),
            ["multidrive", "chunks", "ab", "abcdef.0"]
        );
        assert_eq!(
            ruta_completa(&raiz, "multidrive-volume.json"),
            ["multidrive", "multidrive-volume.json"]
        );
    }

    #[test]
    fn las_barras_de_sobra_no_crean_carpetas_vacias() {
        assert_eq!(segmentos("/multidrive/"), ["multidrive"]);
        assert_eq!(segmentos("//a//b"), ["a", "b"]);
        assert!(segmentos("/").is_empty());
    }

    #[test]
    fn una_comilla_en_la_raiz_no_rompe_la_consulta() {
        assert_eq!(escapar_consulta("de Luis'"), "de Luis\\'");
        assert_eq!(escapar_consulta("a\\b"), "a\\\\b");
        assert_eq!(escapar_consulta("multidrive"), "multidrive");
    }
}
