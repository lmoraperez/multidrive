//! Cuanto espacio dice tener cada proveedor.
//!
//! Esto no pasa por OpenDAL, y no por gusto: OpenDAL abstrae objetos, y el cupo de una
//! cuenta no es un objeto. Cada nube lo cuenta en su propia API (`drive/v3/about` en Google,
//! `/me/drive` en Graph, `PROPFIND` con RFC 4331 en WebDAV), asi que aqui hay una llamada
//! HTTP por servicio.
//!
//! Tres reglas, que son las que hacen que la barra de la interfaz signifique algo:
//!
//! 1. **`None` no es cero.** Un proveedor que no declara total sale sin barra, no con una
//!    barra vacia. Un bucket S3 no tiene cupo: se factura por lo que ocupa.
//! 2. **`usado` es la cuenta entera**, no lo que ocupa LibreDisk. Son dos cifras distintas y
//!    solo una de ellas se consigue barata.
//! 3. **Un fallo es un fallo.** Si la peticion no sale, es `Err`, y la interfaz lo dice. No
//!    se devuelve una cuota vacia haciendola pasar por una respuesta.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::{BackendError, Cuota, OauthCreds};

/// De donde sale la cuota de un proveedor concreto.
pub(crate) enum Medidor {
    /// No hay a quien preguntar. Memoria (no es almacenamiento real) y S3 (no hay cupo).
    Muda,
    /// Un directorio local: lo sabe el sistema de ficheros, sin red de por medio.
    Disco(PathBuf),
    /// Una nube OAuth2. El token de acceso sale del de refresco y se guarda mientras dure.
    Nube {
        cual: Nube,
        creds: OauthCreds,
        token: Mutex<Option<(String, Instant)>>,
    },
    /// WebDAV, que lo dice en el mismo PROPFIND del estandar (RFC 4331).
    Webdav {
        endpoint: String,
        root: String,
        creds: Option<(String, String)>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nube {
    Gdrive,
    Onedrive,
}

impl Nube {
    /// El mismo sitio al que va OpenDAL a renovar. Que las dos piezas usen el mismo extremo
    /// importa: si aqui valiera otro, un token que sirve para escribir podria no servir para
    /// preguntar por el cupo, y el fallo pareceria de permisos.
    fn refresco(self) -> &'static str {
        match self {
            Self::Gdrive => "https://oauth2.googleapis.com/token",
            Self::Onedrive => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        }
    }

    fn consulta(self) -> &'static str {
        match self {
            Self::Gdrive => "https://www.googleapis.com/drive/v3/about?fields=storageQuota",
            Self::Onedrive => "https://graph.microsoft.com/v1.0/me/drive?$select=quota",
        }
    }
}

impl Medidor {
    /// Medidor de una nube OAuth2, con la cache de token todavia vacia.
    pub(crate) fn nube(cual: Nube, creds: &OauthCreds) -> Self {
        Self::Nube {
            cual,
            creds: creds.clone(),
            token: Mutex::new(None),
        }
    }

    pub(crate) async fn medir(&self, nombre: &str) -> Result<Cuota, BackendError> {
        match self {
            Self::Muda => Ok(Cuota::MUDA),
            Self::Disco(ruta) => Ok(disco(ruta)),
            Self::Nube { cual, creds, token } => nube(nombre, *cual, creds, token).await,
            Self::Webdav {
                endpoint,
                root,
                creds,
            } => webdav(nombre, endpoint, root, creds.as_ref()).await,
        }
    }
}

fn red(nombre: &str, detalle: impl std::fmt::Display) -> BackendError {
    BackendError::Cuota {
        name: nombre.to_string(),
        detalle: detalle.to_string(),
    }
}

// ---------------------------------------------------------------------------------------
// Nubes OAuth2
// ---------------------------------------------------------------------------------------

/// Margen con el que se da por caducado un token antes de que lo este de verdad.
///
/// Sin el hay una carrera perdida por definicion: un token al que le quedan dos segundos
/// pasa la comprobacion y llega caducado al servidor.
const MARGEN: Duration = Duration::from_secs(60);

async fn nube(
    nombre: &str,
    cual: Nube,
    creds: &OauthCreds,
    cache: &Mutex<Option<(String, Instant)>>,
) -> Result<Cuota, BackendError> {
    let token = match creds {
        // Un token de acceso suelto no se renueva: es lo que hay, y caduca en una hora.
        OauthCreds::Acceso(t) => t.clone(),
        OauthCreds::Refresco {
            refresh_token,
            client_id,
            client_secret,
        } => {
            let mut guardado = cache.lock().await;
            match guardado
                .as_ref()
                .filter(|(_, hasta)| *hasta > Instant::now())
            {
                Some((t, _)) => t.clone(),
                None => {
                    let (t, dura) =
                        refrescar(nombre, cual, refresh_token, client_id, client_secret).await?;
                    *guardado = Some((t.clone(), Instant::now() + dura.saturating_sub(MARGEN)));
                    t
                }
            }
        }
    };

    let respuesta = cliente()
        .get(cual.consulta())
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| red(nombre, e))?;
    let cuerpo = exigir_ok(nombre, respuesta).await?;
    let json: serde_json::Value = serde_json::from_str(&cuerpo).map_err(|e| red(nombre, e))?;

    match cual {
        // Google da las cifras como cadenas y **omite `limit` en las cuentas sin limite**.
        // Ese hueco es justo lo que representa el `Option`: no hay total, no que sea cero.
        Nube::Gdrive => {
            let q = &json["storageQuota"];
            Ok(Cuota {
                total: numero(&q["limit"]),
                usado: numero(&q["usage"]),
            })
        }
        Nube::Onedrive => {
            let q = &json["quota"];
            Ok(Cuota {
                total: numero(&q["total"]),
                usado: numero(&q["used"]),
            })
        }
    }
}

/// Acepta la cifra venga como numero o como cadena: Graph la manda como numero y Drive como
/// cadena, porque un cupo de varios terabytes no cabe en el entero exacto de JavaScript.
fn numero(v: &serde_json::Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

async fn refrescar(
    nombre: &str,
    cual: Nube,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<(String, Duration), BackendError> {
    let respuesta = cliente()
        .post(cual.refresco())
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await
        .map_err(|e| red(nombre, e))?;
    let cuerpo = exigir_ok(nombre, respuesta).await?;
    let json: serde_json::Value = serde_json::from_str(&cuerpo).map_err(|e| red(nombre, e))?;
    let token = json["access_token"]
        .as_str()
        .ok_or_else(|| red(nombre, "la renovacion no devolvio ningun token de acceso"))?;
    // Si no dice cuanto dura se supone lo poco en vez de lo mucho: pasarse por corto cuesta
    // una peticion de mas, y pasarse por largo, un fallo.
    let dura = Duration::from_secs(json["expires_in"].as_u64().unwrap_or(300));
    Ok((token.to_string(), dura))
}

// ---------------------------------------------------------------------------------------
// WebDAV (RFC 4331)
// ---------------------------------------------------------------------------------------

const PROPFIND: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
    "<D:propfind xmlns:D=\"DAV:\"><D:prop>",
    "<D:quota-available-bytes/><D:quota-used-bytes/>",
    "</D:prop></D:propfind>",
);

async fn webdav(
    nombre: &str,
    endpoint: &str,
    root: &str,
    creds: Option<&(String, String)>,
) -> Result<Cuota, BackendError> {
    let url = format!(
        "{}/{}",
        endpoint.trim_end_matches('/'),
        root.trim_matches('/')
    );
    let metodo = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND es un metodo valido");
    let mut peticion = cliente()
        .request(metodo, url)
        // Profundidad 0: se pregunta por esta coleccion, no por lo que cuelga de ella. Con
        // `Depth: 1`, un Nextcloud con muchos ficheros devolveria un XML enorme para dar dos
        // numeros.
        .header("Depth", "0")
        .header(reqwest::header::CONTENT_TYPE, "application/xml")
        .body(PROPFIND);
    if let Some((usuario, contrasena)) = creds {
        peticion = peticion.basic_auth(usuario, Some(contrasena));
    }
    let respuesta = peticion.send().await.map_err(|e| red(nombre, e))?;
    let cuerpo = exigir_ok(nombre, respuesta).await?;

    // Se saca recortando texto y no con un analizador de XML: son dos etiquetas de nombre
    // fijo definidas por un RFC, y traer una dependencia de XML para esto seria
    // desproporcionado. Si algun dia hay que leer XML de verdad, este es el sitio.
    let libre = etiqueta(&cuerpo, "quota-available-bytes");
    let usado = etiqueta(&cuerpo, "quota-used-bytes");
    // El estandar dice que `available` es lo que le queda a **esta** coleccion, y no declara
    // total: se deduce sumando. Si falta cualquiera de los dos, no hay total que deducir.
    Ok(Cuota {
        total: libre.zip(usado).map(|(l, u)| l + u),
        usado,
    })
}

/// Contenido de `<X:etiqueta>` sea cual sea el prefijo de espacio de nombres que use el
/// servidor: `D:`, `d:`, ninguno... El RFC fija el nombre, no el prefijo.
fn etiqueta(xml: &str, nombre: &str) -> Option<u64> {
    let abre = xml.find(&format!("{nombre}>"))?;
    let resto = &xml[abre + nombre.len() + 1..];
    let cierra = resto.find("</")?;
    resto[..cierra].trim().parse().ok()
}

// ---------------------------------------------------------------------------------------
// Disco local
// ---------------------------------------------------------------------------------------

/// Espacio del sistema de ficheros donde vive el directorio.
///
/// Se pregunta por la ruta y no por la unidad: en Unix puede haber otro punto de montaje
/// debajo, y en Windows la ruta puede ser un recurso de red.
///
/// Va contra la API del sistema con lo que ya hay en el arbol de dependencias, en vez de
/// traer una caja nueva para dos llamadas.
fn disco(ruta: &Path) -> Cuota {
    match espacio(ruta) {
        Some((total, libre)) => Cuota {
            total: Some(total),
            usado: Some(total.saturating_sub(libre)),
        },
        // Un directorio que todavia no existe, o una ruta que el sistema no sabe resolver.
        // No es un error del proveedor: es que aun no hay nada que decir.
        None => Cuota::MUDA,
    }
}

#[cfg(windows)]
fn espacio(ruta: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let ancho: Vec<u16> = ruta.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut libre, mut total) = (0u64, 0u64);
    // SEGURIDAD: `ancho` acaba en cero y vive durante toda la llamada; los dos punteros de
    // salida apuntan a variables de esta pila. El tercero es opcional y va nulo.
    let ok = unsafe {
        GetDiskFreeSpaceExW(ancho.as_ptr(), &mut libre, &mut total, std::ptr::null_mut())
    };
    (ok != 0).then_some((total, libre))
}

#[cfg(unix)]
fn espacio(ruta: &Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(ruta.as_os_str().as_bytes()).ok()?;
    // SEGURIDAD: `statvfs` solo escribe en la estructura, que se le pasa recien puesta a
    // ceros, y `c` acaba en cero por construccion.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    // `f_frsize` es el tamano real del bloque; `f_bsize` es el preferido para E/S y no tiene
    // por que coincidir. Y `f_bavail`, no `f_bfree`, porque parte de lo libre esta reservado
    // para root y esto no corre como root.
    let bloque = s.f_frsize as u64;
    Some((s.f_blocks as u64 * bloque, s.f_bavail as u64 * bloque))
}

// ---------------------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------------------

/// Cliente compartido: reutiliza conexiones y, sobre todo, no rehace el saludo TLS en cada
/// pregunta.
fn cliente() -> &'static reqwest::Client {
    static CLIENTE: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENTE.get_or_init(|| {
        reqwest::Client::builder()
            // Preguntar por el cupo es un adorno de la interfaz: mas vale decir "no se pudo"
            // que dejar una ventana colgada esperando a una nube que no contesta.
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default()
    })
}

/// Convierte un codigo de error en `Err`, con el cuerpo dentro.
///
/// Sin esto, un 401 se colaria como un JSON sin los campos esperados y acabaria mostrandose
/// como "este proveedor no declara su cupo", que es mentira: lo declara, pero no nos deja
/// verlo.
async fn exigir_ok(nombre: &str, r: reqwest::Response) -> Result<String, BackendError> {
    let estado = r.status();
    let cuerpo = r.text().await.map_err(|e| red(nombre, e))?;
    if estado.is_success() {
        return Ok(cuerpo);
    }
    // El cuerpo de un error de OAuth puede llevar detalles utiles, pero tambien puede llevar
    // trozos de lo enviado. Se recorta: un mensaje de error acaba en registros y en capturas
    // de pantalla.
    let pista: String = cuerpo
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    Err(red(
        nombre,
        format!("el servicio respondio {estado}: {pista}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn se_lee_la_cifra_venga_como_numero_o_como_cadena() {
        // Graph la manda como numero y Drive como cadena. Las dos tienen que valer.
        assert_eq!(numero(&serde_json::json!(1234)), Some(1234));
        assert_eq!(numero(&serde_json::json!("1234")), Some(1234));
        assert_eq!(numero(&serde_json::json!(null)), None);
        // Un cupo de 16 TB no cabe en el entero exacto de JavaScript: por eso Drive lo manda
        // como cadena, y por eso no se puede leer como `f64`.
        assert_eq!(
            numero(&serde_json::json!("16106127360000")),
            Some(16_106_127_360_000)
        );
    }

    #[test]
    fn la_etiqueta_se_lee_con_cualquier_prefijo() {
        let con = |p: &str| format!("<{p}quota-used-bytes>42</{p}quota-used-bytes>");
        for prefijo in ["", "D:", "d:", "ns1:"] {
            assert_eq!(
                etiqueta(&con(prefijo), "quota-used-bytes"),
                Some(42),
                "prefijo {prefijo:?}"
            );
        }
    }

    #[test]
    fn sin_la_etiqueta_no_se_inventa_un_cero() {
        let solo_usado = "<D:quota-used-bytes>42</D:quota-used-bytes>";
        assert_eq!(etiqueta(solo_usado, "quota-available-bytes"), None);
        // Nextcloud devuelve -1 en "available" cuando la cuota es ilimitada. No es una
        // cantidad de bytes, asi que no se acepta como tal.
        let ilimitado = "<D:quota-available-bytes>-1</D:quota-available-bytes>";
        assert_eq!(etiqueta(ilimitado, "quota-available-bytes"), None);
    }

    #[test]
    fn el_disco_local_de_verdad_dice_algo() {
        // No se puede afirmar cuanto: se afirma que contesta y que lo que dice es coherente.
        let c = disco(Path::new("."));
        let total = c
            .total
            .expect("el directorio actual esta en algun sistema de ficheros");
        assert!(total > 0);
        assert!(c.usado.unwrap() <= total);
    }

    #[test]
    fn un_directorio_que_no_existe_no_es_un_error_sino_un_silencio() {
        let c = disco(Path::new("/no/existe/en/ninguna/maquina/de/este/mundo"));
        assert_eq!(c, Cuota::MUDA);
    }

    #[test]
    fn sin_las_dos_cifras_no_hay_libre() {
        assert_eq!(
            Cuota {
                total: None,
                usado: Some(1)
            }
            .libre(),
            None
        );
        assert_eq!(
            Cuota {
                total: Some(10),
                usado: Some(4)
            }
            .libre(),
            Some(6)
        );
    }
}
