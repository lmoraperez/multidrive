//! Construccion de proveedores reales a partir del entorno.
//!
//! Las credenciales no se guardan en el repositorio ni en un fichero de configuracion de
//! pruebas: se leen del entorno, para que ejecutar la suite contra una cuenta de verdad sea
//! una decision explicita de quien la lanza.
//!
//! Un proveedor "no configurado" se omite en silencio, pero uno **a medio configurar** es un
//! error ruidoso. Esa distincion importa: la forma clasica de tener pruebas que no prueban
//! nada es que un tipo tomado por el usuario se salte todo por una variable mal escrita.

use crate::{BackendError, OauthCreds, OpendalBackend};

const PREFIJO: &str = "LIBREDISK_TEST_";

/// Carpeta por defecto dentro de la cuenta. Dedicada a proposito: la suite escribe blobs
/// opacos y borra lo que crea, y no debe compartir carpeta con archivos del usuario.
const RAIZ_POR_DEFECTO: &str = "/libredisk-pruebas";

/// Las variables que reconoce, para poder decirselo al usuario cuando no hay ninguna.
pub const AYUDA: &str = "\
Proveedores y variables de entorno reconocidas (prefijo LIBREDISK_TEST_):

  Directorio    FS_ROOT
                No prueba ninguna nube: sirve para comprobar que el arnes funciona
                antes de pelearse con OAuth.

  OneDrive      ONEDRIVE_ACCESS_TOKEN
                o bien ONEDRIVE_REFRESH_TOKEN + ONEDRIVE_CLIENT_ID + ONEDRIVE_CLIENT_SECRET
                opcional: ONEDRIVE_ROOT

  Google Drive  GDRIVE_ACCESS_TOKEN
                o bien GDRIVE_REFRESH_TOKEN + GDRIVE_CLIENT_ID + GDRIVE_CLIENT_SECRET
                opcional: GDRIVE_ROOT

  S3            S3_BUCKET + S3_ENDPOINT + S3_REGION
                opcional: S3_ACCESS_KEY_ID + S3_SECRET_ACCESS_KEY, S3_ROOT

  WebDAV        WEBDAV_ENDPOINT
                opcional: WEBDAV_USERNAME + WEBDAV_PASSWORD, WEBDAV_ROOT

Ver docs/PRUEBAS-PROVEEDORES.md para obtener los tokens de OneDrive y Google Drive.";

/// Proveedores configurados en el entorno del proceso.
pub fn desde_entorno() -> Result<Vec<OpendalBackend>, BackendError> {
    construir(&|clave| std::env::var(clave).ok())
}

/// Igual que [`desde_entorno`], pero leyendo de donde se le diga. Existe para poder probar
/// la logica de configuracion sin tocar el entorno del proceso, que es global y compartido
/// entre los hilos de los tests.
fn construir(leer: &dyn Fn(&str) -> Option<String>) -> Result<Vec<OpendalBackend>, BackendError> {
    let campo = |proveedor: &str, nombre: &str| -> Option<String> {
        leer(&format!("{PREFIJO}{proveedor}_{nombre}"))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let raiz =
        |proveedor: &str| campo(proveedor, "ROOT").unwrap_or_else(|| RAIZ_POR_DEFECTO.to_string());

    let mut backends = Vec::new();

    // Directorio local. No prueba ninguna nube, pero recorre el arnes entero.
    if let Some(raiz) = campo("FS", "ROOT") {
        backends.push(OpendalBackend::fs("fs", &raiz)?);
    }

    for (proveedor, etiqueta) in [("ONEDRIVE", "onedrive"), ("GDRIVE", "gdrive")] {
        let campos = [
            "ACCESS_TOKEN",
            "REFRESH_TOKEN",
            "CLIENT_ID",
            "CLIENT_SECRET",
            "ROOT",
        ];
        if !campos.iter().any(|c| campo(proveedor, c).is_some()) {
            continue;
        }
        let creds = oauth(proveedor, &campo)?;
        let raiz = raiz(proveedor);
        backends.push(match proveedor {
            "ONEDRIVE" => OpendalBackend::onedrive(etiqueta, &raiz, &creds)?,
            _ => OpendalBackend::gdrive(etiqueta, &raiz, &creds)?,
        });
    }

    if [
        "BUCKET",
        "ENDPOINT",
        "REGION",
        "ACCESS_KEY_ID",
        "SECRET_ACCESS_KEY",
        "ROOT",
    ]
    .iter()
    .any(|c| campo("S3", c).is_some())
    {
        let bucket = requerida("s3", campo("S3", "BUCKET"), "S3_BUCKET")?;
        let endpoint = requerida("s3", campo("S3", "ENDPOINT"), "S3_ENDPOINT")?;
        let region = requerida("s3", campo("S3", "REGION"), "S3_REGION")?;
        let creds = pareja(
            "s3",
            campo("S3", "ACCESS_KEY_ID"),
            campo("S3", "SECRET_ACCESS_KEY"),
            "S3_ACCESS_KEY_ID",
            "S3_SECRET_ACCESS_KEY",
        )?;
        backends.push(OpendalBackend::s3(
            "s3",
            &bucket,
            &endpoint,
            &region,
            creds.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
        )?);
    }

    if ["ENDPOINT", "USERNAME", "PASSWORD", "ROOT"]
        .iter()
        .any(|c| campo("WEBDAV", c).is_some())
    {
        let endpoint = requerida("webdav", campo("WEBDAV", "ENDPOINT"), "WEBDAV_ENDPOINT")?;
        let creds = pareja(
            "webdav",
            campo("WEBDAV", "USERNAME"),
            campo("WEBDAV", "PASSWORD"),
            "WEBDAV_USERNAME",
            "WEBDAV_PASSWORD",
        )?;
        backends.push(OpendalBackend::webdav(
            "webdav",
            &endpoint,
            &raiz("WEBDAV"),
            creds.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
        )?);
    }

    Ok(backends)
}

fn oauth(
    proveedor: &'static str,
    campo: &dyn Fn(&str, &str) -> Option<String>,
) -> Result<OauthCreds, BackendError> {
    if let Some(token) = campo(proveedor, "ACCESS_TOKEN") {
        return Ok(OauthCreds::Acceso(token));
    }
    let (Some(refresh_token), Some(client_id), Some(client_secret)) = (
        campo(proveedor, "REFRESH_TOKEN"),
        campo(proveedor, "CLIENT_ID"),
        campo(proveedor, "CLIENT_SECRET"),
    ) else {
        return Err(BackendError::EntornoIncompleto {
            proveedor,
            falta: format!(
                "{PREFIJO}{proveedor}_ACCESS_TOKEN, o bien {PREFIJO}{proveedor}_REFRESH_TOKEN \
                 + {PREFIJO}{proveedor}_CLIENT_ID + {PREFIJO}{proveedor}_CLIENT_SECRET"
            ),
        });
    };
    Ok(OauthCreds::Refresco {
        refresh_token,
        client_id,
        client_secret,
    })
}

fn requerida(
    proveedor: &'static str,
    valor: Option<String>,
    nombre: &str,
) -> Result<String, BackendError> {
    valor.ok_or_else(|| BackendError::EntornoIncompleto {
        proveedor,
        falta: format!("{PREFIJO}{nombre}"),
    })
}

/// Dos variables que solo valen juntas: o las dos o ninguna.
fn pareja(
    proveedor: &'static str,
    uno: Option<String>,
    dos: Option<String>,
    nombre_uno: &str,
    nombre_dos: &str,
) -> Result<Option<(String, String)>, BackendError> {
    match (uno, dos) {
        (Some(a), Some(b)) => Ok(Some((a, b))),
        (None, None) => Ok(None),
        (Some(_), None) => Err(BackendError::EntornoIncompleto {
            proveedor,
            falta: format!("{PREFIJO}{nombre_dos}"),
        }),
        (None, Some(_)) => Err(BackendError::EntornoIncompleto {
            proveedor,
            falta: format!("{PREFIJO}{nombre_uno}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::Backend;

    fn entorno(pares: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let mapa: HashMap<String, String> = pares
            .iter()
            .map(|(k, v)| (format!("{PREFIJO}{k}"), v.to_string()))
            .collect();
        move |clave: &str| mapa.get(clave).cloned()
    }

    #[test]
    fn sin_variables_no_hay_proveedores() {
        assert!(construir(&entorno(&[])).unwrap().is_empty());
    }

    #[test]
    fn onedrive_con_token_de_acceso() {
        let b = construir(&entorno(&[("ONEDRIVE_ACCESS_TOKEN", "abc")])).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name(), "onedrive");
    }

    #[test]
    fn gdrive_con_token_de_refresco() {
        let b = construir(&entorno(&[
            ("GDRIVE_REFRESH_TOKEN", "r"),
            ("GDRIVE_CLIENT_ID", "i"),
            ("GDRIVE_CLIENT_SECRET", "s"),
        ]))
        .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name(), "gdrive");
    }

    /// El caso que justifica todo este modulo: media configuracion debe doler, no colarse.
    #[test]
    fn configuracion_a_medias_es_error() {
        let err = construir(&entorno(&[("ONEDRIVE_REFRESH_TOKEN", "r")])).unwrap_err();
        let texto = err.to_string();
        assert!(texto.contains("ONEDRIVE"), "{texto}");
        assert!(texto.contains("CLIENT_ID"), "{texto}");
    }

    #[test]
    fn solo_la_raiz_tambien_cuenta_como_intento() {
        // Poner ONEDRIVE_ROOT y olvidarse del token no debe traducirse en "no configurado".
        assert!(construir(&entorno(&[("ONEDRIVE_ROOT", "/x")])).is_err());
    }

    #[test]
    fn pareja_de_credenciales_incompleta_es_error() {
        let err = construir(&entorno(&[
            ("WEBDAV_ENDPOINT", "https://nube/remote.php/dav"),
            ("WEBDAV_USERNAME", "luis"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("WEBDAV_PASSWORD"), "{err}");
    }

    #[test]
    fn webdav_sin_credenciales_es_valido() {
        let b = construir(&entorno(&[("WEBDAV_ENDPOINT", "http://localhost/dav")])).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name(), "webdav");
    }

    #[test]
    fn varios_proveedores_a_la_vez() {
        let b = construir(&entorno(&[
            ("ONEDRIVE_ACCESS_TOKEN", "a"),
            ("GDRIVE_ACCESS_TOKEN", "b"),
            ("WEBDAV_ENDPOINT", "http://localhost/dav"),
        ]))
        .unwrap();
        let nombres: Vec<_> = b.iter().map(|x| x.name()).collect();
        assert_eq!(nombres, ["onedrive", "gdrive", "webdav"]);
    }

    #[test]
    fn directorio_local_como_proveedor() {
        let b = construir(&entorno(&[("FS_ROOT", "/tmp/libredisk-arnes")])).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name(), "fs");
    }

    #[test]
    fn s3_incompleto_dice_que_falta() {
        let err = construir(&entorno(&[("S3_BUCKET", "cubo")])).unwrap_err();
        assert!(err.to_string().contains("S3_ENDPOINT"), "{err}");
    }
}
