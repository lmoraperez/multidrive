//! Construccion de proveedores reales a partir de la configuracion.
//!
//! Las credenciales no se guardan en el repositorio. Salen del entorno del proceso o de un
//! fichero de configuracion (ver [`crate::config`]), y el entorno gana: quien exporta una
//! variable esta diciendo algo sobre **esta** ejecucion.
//!
//! El fichero existe porque un servicio no tiene consola: arranca como LocalSystem y ahi no
//! hay ningun `.env` que nadie haya exportado.
//!
//! Un proveedor "no configurado" se omite en silencio, pero uno **a medio configurar** es un
//! error ruidoso. Esa distincion importa: la forma clasica de tener pruebas que no prueban
//! nada es que un tipo tomado por el usuario se salte todo por una variable mal escrita.

use crate::config::Config;
use crate::{BackendError, OauthCreds, OpendalBackend};

const PREFIJO: &str = "LIBREDISK_";

/// El prefijo con el que nacio esto, cuando solo configuraba la suite de conformidad. Se
/// sigue aceptando: quien ya tenia un `.env` escrito no deberia tener que reescribirlo
/// porque el proyecto haya crecido. El nuevo gana si estan los dos.
const PREFIJO_VIEJO: &str = "LIBREDISK_TEST_";

/// Carpeta por defecto dentro de la cuenta. Dedicada a proposito: la suite escribe blobs
/// opacos y borra lo que crea, y no debe compartir carpeta con archivos del usuario.
const RAIZ_POR_DEFECTO: &str = "/libredisk-pruebas";

/// Cuantos directorios locales numerados se buscan.
///
/// El limite existe para no preguntar indefinidamente; treinta y dos son mas proveedores de
/// los que tiene sentido repartir un volumen.
const MAX_FS: usize = 32;

/// Las variables que reconoce, para poder decirselo al usuario cuando no hay ninguna.
pub const AYUDA: &str = "\
Proveedores y claves reconocidas (prefijo LIBREDISK_; tambien se acepta el antiguo
LIBREDISK_TEST_). Se leen del entorno o del fichero de configuracion; el entorno gana.

  Directorio    FS_ROOT, y ademas FS1_ROOT, FS2_ROOT... hasta FS32_ROOT
                No prueban ninguna nube: sirven para comprobar que el arnes funciona
                antes de pelearse con OAuth, y para montar un volumen con redundancia
                sobre varios discos o puntos de montaje locales.

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

Ver `libredisk config ruta` para saber donde se busca el fichero, y
docs/PRUEBAS-PROVEEDORES.md para obtener los tokens de OneDrive y Google Drive.";

/// Proveedores configurados, vengan de donde vengan.
///
/// El entorno gana al fichero. Quien exporta una variable esta diciendo algo sobre **esta**
/// ejecucion, y eso es mas especifico que lo que dejo escrito en disco hace un mes.
pub fn configurados() -> Result<Vec<OpendalBackend>, BackendError> {
    let config = Config::cargar()?;
    construir(&|clave| std::env::var(clave).ok().or_else(|| config.get(clave)))
}

/// Igual que [`configurados`], pero leyendo de donde se le diga. Existe para poder probar
/// la logica de configuracion sin tocar el entorno del proceso, que es global y compartido
/// entre los hilos de los tests.
fn construir(leer: &dyn Fn(&str) -> Option<String>) -> Result<Vec<OpendalBackend>, BackendError> {
    // Se prueban los dos prefijos por orden: el nuevo manda.
    let campo = |proveedor: &str, nombre: &str| -> Option<String> {
        [PREFIJO, PREFIJO_VIEJO].into_iter().find_map(|p| {
            leer(&format!("{p}{proveedor}_{nombre}"))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
    };
    let raiz =
        |proveedor: &str| campo(proveedor, "ROOT").unwrap_or_else(|| RAIZ_POR_DEFECTO.to_string());

    let mut backends = Vec::new();

    // Directorios locales. No prueban ninguna nube, pero recorren el arnes entero.
    //
    // Numerados en vez de una lista separada por comas: una ruta de Windows lleva `:` y
    // puede llevar `,`, asi que cualquier separador seria ambiguo. Y el nombre va pegado a
    // la variable, que es lo que evita el fallo silencioso de reordenar la lista y que cada
    // fragmento se lea del sitio equivocado.
    if let Some(raiz) = campo("FS", "ROOT") {
        backends.push(OpendalBackend::fs("fs", &raiz)?);
    }
    for i in 1..=MAX_FS {
        let proveedor = format!("FS{i}");
        if let Some(raiz) = campo(&proveedor, "ROOT") {
            backends.push(OpendalBackend::fs(proveedor.to_lowercase(), &raiz)?);
        }
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

    /// Lee con el prefijo que se le diga, para poder mezclarlos.
    fn entorno_crudo(pares: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let mapa: HashMap<String, String> = pares
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |clave: &str| mapa.get(clave).cloned()
    }

    /// El `.env` que ya existia sigue funcionando. Romperlo obligaria a reescribir la
    /// configuracion de todo el mundo por un cambio de nombre.
    #[test]
    fn el_prefijo_antiguo_se_sigue_aceptando() {
        let b = construir(&entorno_crudo(&[(
            "LIBREDISK_TEST_FS_ROOT",
            "/tmp/libredisk-viejo",
        )]))
        .unwrap();
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn el_prefijo_nuevo_gana_al_antiguo() {
        let b = construir(&entorno_crudo(&[
            ("LIBREDISK_GDRIVE_ACCESS_TOKEN", "nuevo"),
            ("LIBREDISK_TEST_GDRIVE_ACCESS_TOKEN", "viejo"),
            ("LIBREDISK_GDRIVE_ROOT", "/nueva"),
            ("LIBREDISK_TEST_GDRIVE_ROOT", "/vieja"),
        ]))
        .unwrap();
        assert_eq!(
            b.len(),
            1,
            "no puede construir dos veces el mismo proveedor"
        );
    }

    /// Un valor en blanco con el prefijo nuevo no debe tapar uno bueno con el antiguo: la
    /// forma tipica de tener eso es una linea a medio escribir en el fichero.
    #[test]
    fn un_valor_vacio_no_tapa_al_del_otro_prefijo() {
        let b = construir(&entorno_crudo(&[
            ("LIBREDISK_FS_ROOT", "   "),
            ("LIBREDISK_TEST_FS_ROOT", "/tmp/libredisk-bueno"),
        ]))
        .unwrap();
        assert_eq!(b.len(), 1, "el vacio se ignora y se cae al antiguo");
    }

    /// Varios directorios locales, para poder montar un volumen con redundancia sin nubes.
    #[test]
    fn varios_directorios_locales_numerados() {
        let b = construir(&entorno(&[
            ("FS1_ROOT", "/tmp/uno"),
            ("FS2_ROOT", "/tmp/dos"),
            ("FS3_ROOT", "/tmp/tres"),
        ]))
        .unwrap();
        let nombres: Vec<&str> = b.iter().map(Backend::name).collect();
        assert_eq!(nombres, vec!["fs1", "fs2", "fs3"]);
    }

    /// El nombre va pegado al numero de la variable, no a la posicion en que se lea.
    ///
    /// Es lo que impide el fallo silencioso: si el nombre dependiera del orden, quitar el
    /// segundo de tres renombraria al tercero y cada fragmento se leeria del sitio
    /// equivocado, sin que la cabecera se enterara.
    #[test]
    fn el_nombre_no_depende_de_cuales_falten() {
        let b = construir(&entorno(&[
            ("FS1_ROOT", "/tmp/uno"),
            ("FS7_ROOT", "/tmp/siete"),
        ]))
        .unwrap();
        let nombres: Vec<&str> = b.iter().map(Backend::name).collect();
        assert_eq!(nombres, vec!["fs1", "fs7"]);
    }

    /// El de siempre sigue llamandose `fs` y conviviendo con los numerados.
    #[test]
    fn el_directorio_sin_numero_sigue_valiendo() {
        let b = construir(&entorno(&[
            ("FS_ROOT", "/tmp/viejo"),
            ("FS1_ROOT", "/tmp/uno"),
        ]))
        .unwrap();
        let nombres: Vec<&str> = b.iter().map(Backend::name).collect();
        assert_eq!(nombres, vec!["fs", "fs1"]);
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
