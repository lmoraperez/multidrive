//! Conectar una cuenta desde el navegador.
//!
//! Lo que sustituye esto: antes, para usar OneDrive o Drive habia que registrar una
//! aplicacion en la consola del proveedor, correr un script que devolviera un token de
//! refresco, y pegar ese token en un formulario. Eso son tareas de desarrollador, y pedirselas
//! a quien solo quiere guardar sus ficheros no es una interfaz incompleta: es no tener
//! interfaz.
//!
//! # Como funciona
//!
//! Es el flujo de codigo de autorizacion con redireccion a `localhost`, que es el que usan
//! las aplicaciones de escritorio:
//!
//! 1. Se abre un servidor en `127.0.0.1` con un puerto que elige el sistema.
//! 2. Se manda el navegador a la pagina del proveedor, diciendole que al terminar redirija a
//!    ese puerto.
//! 3. El usuario entra en su cuenta y da permiso. **Sus credenciales no pasan por aqui**: van
//!    del navegador al proveedor, y este programa solo recibe un codigo de un solo uso.
//! 4. Ese codigo se canjea por un token de refresco, que es lo que se guarda.
//!
//! # Por que el puerto es el que sea
//!
//! Los dos proveedores admiten cualquier puerto en `127.0.0.1` sin declararlo por adelantado,
//! justamente para esto. Fijar uno seria fallar cuando ya estuviera ocupado.
//!
//! # PKCE
//!
//! Se manda un reto PKCE aunque la aplicacion lleve secreto y no sea obligatorio. El motivo es
//! concreto: el redirector es `localhost`, o sea que **cualquier otro programa de esta maquina
//! puede quedarse escuchando y recoger el codigo**. Sin PKCE le bastaria con eso; con PKCE el
//! codigo no sirve sin un valor que solo conoce este proceso y que nunca viaja por el
//! navegador.

use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::BackendError;
use crate::catalogo::Oauth;

/// Cuanto se espera a que el usuario termine en el navegador.
///
/// Cinco minutos da para entrar en la cuenta, buscar el segundo factor en el movil y leer la
/// pantalla de permisos. Pasado eso se suelta el puerto en vez de dejar un servidor abierto
/// para siempre: quien se fue a hacer otra cosa volvera y pulsara otra vez.
const ESPERA: Duration = Duration::from_secs(300);

/// Una cuenta recien conectada.
///
/// El `Debug` se escribe a mano y redactado, por el mismo motivo que el de `OauthCreds`: un
/// `{:?}` descuidado en un registro o en la salida de un test fallido publicaria acceso
/// permanente a la cuenta.
pub struct Conectada {
    /// Lo unico que hay que guardar. Con el, el programa renueva el acceso solo.
    pub refresh_token: String,
}

/// Abre el navegador, espera a que el usuario de permiso y devuelve el token de refresco.
///
/// `abrir` recibe la direccion a la que hay que mandar el navegador. Se pasa desde fuera para
/// poder probar el flujo entero sin abrir nada.
pub async fn conectar(
    oauth: &Oauth,
    client_id: &str,
    client_secret: &str,
    abrir: &(dyn Fn(&str) -> Result<(), String> + Sync),
) -> Result<Conectada, BackendError> {
    let fallo = |detalle: String| BackendError::Oauth { detalle };

    // Puerto 0: lo elige el sistema. Se ata **antes** de mandar el navegador, para que no
    // haya una ventana en la que el proveedor redirija a un puerto que todavia no escucha.
    let servidor = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| fallo(format!("no se pudo abrir un puerto local: {e}")))?;
    let puerto = servidor
        .local_addr()
        .map_err(|e| fallo(e.to_string()))?
        .port();
    let redirigir = format!("http://127.0.0.1:{puerto}");

    let verificador = aleatorio();
    let reto = reto_de(&verificador);
    let estado = aleatorio();

    let mut url = format!(
        "{}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}\
         &code_challenge={reto}&code_challenge_method=S256",
        oauth.autorizar,
        urlencode(client_id),
        urlencode(&redirigir),
        urlencode(oauth.ambito),
        urlencode(&estado),
    );
    for (clave, valor) in oauth.extra {
        url.push_str(&format!("&{clave}={}", urlencode(valor)));
    }

    abrir(&url).map_err(|e| {
        fallo(format!(
            "no se pudo abrir el navegador: {e}\nAbre esta direccion a mano:\n{url}"
        ))
    })?;

    let codigo = tokio::time::timeout(ESPERA, esperar_codigo(&servidor, &estado))
        .await
        .map_err(|_| fallo("se agoto la espera: nadie termino de dar permiso".into()))??;

    canjear(
        oauth,
        client_id,
        client_secret,
        &codigo,
        &redirigir,
        &verificador,
    )
    .await
}

/// Atiende hasta que llegue la redireccion buena.
///
/// Se sigue escuchando tras una peticion que no trae codigo porque el navegador pide cosas
/// por su cuenta —`/favicon.ico` es la clasica— y cortar en la primera conexion dejaria el
/// flujo colgado por una peticion que no es la del usuario.
async fn esperar_codigo(servidor: &TcpListener, estado: &str) -> Result<String, BackendError> {
    loop {
        let (mut socket, _) = servidor.accept().await.map_err(|e| BackendError::Oauth {
            detalle: e.to_string(),
        })?;

        let mut buf = vec![0u8; 8192];
        let leidos = socket.read(&mut buf).await.unwrap_or(0);
        let peticion = String::from_utf8_lossy(&buf[..leidos]).to_string();
        let ruta = peticion
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("")
            .to_string();

        if ruta.starts_with("/favicon.ico") {
            responder(&mut socket, 404, "no").await;
            continue;
        }

        let params = consulta(&ruta);
        let buscar = |c: &str| params.iter().find(|(k, _)| k == c).map(|(_, v)| v.clone());

        if let Some(error) = buscar("error") {
            let detalle = buscar("error_description").unwrap_or_else(|| error.clone());
            responder_pagina(&mut socket, "No se pudo conectar", &detalle).await;
            return Err(BackendError::Oauth {
                detalle: format!("el proveedor rechazo la peticion: {error}: {detalle}"),
            });
        }

        let Some(codigo) = buscar("code") else {
            // Cualquier otra peticion suelta al puerto. Ni se contesta con la pagina buena ni
            // se corta el flujo.
            responder(&mut socket, 400, "esto no es la redireccion esperada").await;
            continue;
        };

        // El `state` es lo que ata la respuesta a la peticion que hizo este proceso. Sin
        // comprobarlo, otro programa de la maquina podria empujar un codigo suyo a este
        // puerto y hacer que se conectara **su** cuenta en vez de la del usuario.
        if buscar("state").as_deref() != Some(estado) {
            responder_pagina(
                &mut socket,
                "No se pudo conectar",
                "la respuesta no corresponde a esta peticion",
            )
            .await;
            return Err(BackendError::Oauth {
                detalle: "la respuesta no corresponde a esta peticion (state distinto)".into(),
            });
        }

        responder_pagina(
            &mut socket,
            "Cuenta conectada",
            "Ya puedes cerrar esta pestaña y volver a LibreDisk.",
        )
        .await;
        return Ok(codigo);
    }
}

async fn canjear(
    oauth: &Oauth,
    client_id: &str,
    client_secret: &str,
    codigo: &str,
    redirigir: &str,
    verificador: &str,
) -> Result<Conectada, BackendError> {
    let cliente = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let respuesta = cliente
        .post(oauth.token)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", codigo),
            ("redirect_uri", redirigir),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code_verifier", verificador),
        ])
        .send()
        .await
        .map_err(|e| BackendError::Oauth {
            detalle: e.to_string(),
        })?;

    let estado = respuesta.status();
    let cuerpo = respuesta.text().await.unwrap_or_default();
    if !estado.is_success() {
        // Recortado y sin caracteres de control: este mensaje acaba en registros y en
        // capturas de pantalla, y el cuerpo de un error de OAuth puede traer trozos de lo
        // enviado.
        let pista: String = cuerpo
            .chars()
            .filter(|c| !c.is_control())
            .take(300)
            .collect();
        return Err(BackendError::Oauth {
            detalle: format!("el proveedor respondio {estado}: {pista}"),
        });
    }

    let json: serde_json::Value =
        serde_json::from_str(&cuerpo).map_err(|e| BackendError::Oauth {
            detalle: e.to_string(),
        })?;
    let refresh_token = json["refresh_token"]
        .as_str()
        .ok_or_else(|| BackendError::Oauth {
            // Pasa de verdad y tiene arreglo concreto, asi que se dice cual: Google no
            // reenvia el token de refresco a quien ya le habia dado permiso antes, salvo que
            // se le pida el consentimiento otra vez.
            detalle: "el proveedor no devolvio ningun token de refresco. Si ya habias \
                      conectado esta cuenta, retirale el permiso a la aplicacion en los \
                      ajustes de tu cuenta y vuelve a conectarla"
                .into(),
        })?;

    Ok(Conectada {
        refresh_token: refresh_token.to_string(),
    })
}

// ---------------------------------------------------------------------------------------
// Navegador
// ---------------------------------------------------------------------------------------

/// Abre la direccion en el navegador que tenga puesto el usuario.
///
/// Sin dependencias: son tres ordenes del sistema. La direccion lleva `&`, que en `cmd` es
/// un separador de ordenes, y por eso va como **un solo argumento** —Rust lo entrecomilla— y
/// detras de un titulo vacio, que es lo que `start` espera para no confundir la direccion con
/// el titulo de la ventana.
pub fn abrir_en_navegador(url: &str) -> Result<(), String> {
    let salida = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    salida.map(|_| ()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------
// Piezas
// ---------------------------------------------------------------------------------------

/// Valor aleatorio, en la forma que admiten `state` y el verificador PKCE.
///
/// 32 bytes del generador del sistema, escritos en base64 de URL. Salen 43 caracteres, que
/// es justo el minimo que pide el RFC 7636, y todos son de los que ese RFC permite sin
/// escapar: no hay que codificarlo al meterlo en la direccion.
fn aleatorio() -> String {
    base64_url(&rand::random::<[u8; 32]>())
}

/// El reto PKCE: SHA-256 del verificador, en base64 de URL y sin relleno.
fn reto_de(verificador: &str) -> String {
    let resumen = Sha256::digest(verificador.as_bytes());
    base64_url(&resumen)
}

fn base64_url(bytes: &[u8]) -> String {
    const TABLA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for trozo in bytes.chunks(3) {
        let b = [
            trozo[0],
            trozo.get(1).copied().unwrap_or(0),
            trozo.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        // Sin relleno: el RFC 7636 lo exige asi, y un `=` de mas hace que el proveedor
        // calcule otro resumen y rechace el canje.
        for i in 0..trozo.len() + 1 {
            out.push(TABLA[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out
}

/// Escapa lo que va en la parte de consulta de una URL.
///
/// A mano y no con una caja: son cuatro reglas y aqui solo pasan direcciones y valores que
/// produce este mismo programa.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Deshace el escapado de un valor de la consulta.
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(b) => out.push(b),
                    // Un `%` suelto se deja tal cual en vez de tirar la respuesta entera: el
                    // valor que importa es el codigo, y perderlo por un caracter raro en otro
                    // parametro obligaria a repetir todo el flujo.
                    Err(_) => out.push(b'%'),
                }
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Los pares `clave=valor` de la parte de consulta de una ruta.
fn consulta(ruta: &str) -> Vec<(String, String)> {
    let Some((_, cola)) = ruta.split_once('?') else {
        return Vec::new();
    };
    cola.split('&')
        .filter_map(|par| {
            let (k, v) = par.split_once('=')?;
            Some((urldecode(k), urldecode(v)))
        })
        .collect()
}

async fn responder(socket: &mut tokio::net::TcpStream, codigo: u16, texto: &str) {
    let cuerpo = texto.as_bytes();
    let cabecera = format!(
        "HTTP/1.1 {codigo} .\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        cuerpo.len()
    );
    socket.write_all(cabecera.as_bytes()).await.ok();
    socket.write_all(cuerpo).await.ok();
    socket.shutdown().await.ok();
}

/// La pagina que ve el usuario al volver del proveedor.
///
/// Se sirve desde aqui y no se redirige a ningun sitio: **esta pestana ya lleva el codigo en
/// la direccion**, y mandarla a una pagina externa se lo entregaria a ese servidor en la
/// cabecera de procedencia.
async fn responder_pagina(socket: &mut tokio::net::TcpStream, titulo: &str, detalle: &str) {
    let cuerpo = format!(
        "<!doctype html><html lang=\"es\"><meta charset=\"utf-8\">\
         <title>LibreDisk</title>\
         <body style=\"font:16px/1.6 system-ui,sans-serif;background:#0e1116;color:#e6edf3;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0\">\
         <div style=\"text-align:center;max-width:32rem;padding:0 1rem\">\
         <h1 style=\"font-weight:600\">{}</h1><p style=\"color:#8b949e\">{}</p></div>",
        escapar(titulo),
        escapar(detalle)
    );
    let bytes = cuerpo.as_bytes();
    let cabecera = format!(
        "HTTP/1.1 200 .\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    socket.write_all(cabecera.as_bytes()).await.ok();
    socket.write_all(bytes).await.ok();
    socket.shutdown().await.ok();
}

/// Parte del detalle viene del proveedor, asi que no se mete en HTML tal cual.
fn escapar(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl std::fmt::Debug for Conectada {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conectada")
            .field("refresh_token", &"<redactado>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El ejemplo del RFC 7636, apendice B. Si esto no sale bordado, el proveedor calcula
    /// otro resumen y rechaza el canje con un error que no dice por que.
    #[test]
    fn el_reto_pkce_es_el_del_rfc() {
        let verificador = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            reto_de(verificador),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn el_base64_de_url_no_lleva_relleno_ni_caracteres_de_los_otros() {
        // Un `=` de mas, o un `+` en vez de un `-`, y el proveedor rechaza el canje.
        for n in 1..40usize {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
            let s = base64_url(&bytes);
            assert!(!s.contains('='), "{s}");
            assert!(!s.contains('+'), "{s}");
            assert!(!s.contains('/'), "{s}");
        }
    }

    #[test]
    fn los_valores_aleatorios_no_se_repiten_ni_hay_que_escaparlos() {
        let a = aleatorio();
        let b = aleatorio();
        assert_ne!(a, b);
        assert_eq!(
            urlencode(&a),
            a,
            "el alfabeto elegido no necesita escaparse"
        );
        assert!(a.len() >= 43, "el RFC 7636 pide al menos 43 caracteres");
    }

    #[test]
    fn se_leen_los_parametros_de_la_redireccion() {
        let p = consulta("/?code=4%2F0Ab_c&state=xyz&scope=https%3A%2F%2Fa.b%2Fc");
        let buscar = |c: &str| p.iter().find(|(k, _)| k == c).map(|(_, v)| v.as_str());
        // El codigo de Google lleva `/`, que viaja escapado. Devolverlo sin deshacer el
        // escapado haria que el canje fallara con "codigo invalido".
        assert_eq!(buscar("code"), Some("4/0Ab_c"));
        assert_eq!(buscar("state"), Some("xyz"));
        assert_eq!(buscar("scope"), Some("https://a.b/c"));
    }

    #[test]
    fn una_ruta_sin_consulta_no_da_parametros_ni_revienta() {
        assert!(consulta("/").is_empty());
        assert!(consulta("").is_empty());
    }

    #[test]
    fn lo_que_venga_del_proveedor_no_se_cuela_como_html() {
        // El detalle del error lo escribe el proveedor. Ponerlo tal cual en la pagina seria
        // dejarle escribir HTML en una pagina servida desde esta maquina.
        assert_eq!(
            escapar("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
    }

    /// Un proveedor de mentira: atiende el canje y devuelve lo que se le diga.
    ///
    /// Devuelve tambien lo que recibio, para poder comprobar que se le mando lo que el
    /// protocolo exige y no solo que el flujo termina.
    async fn proveedor_falso(
        respuesta: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let servidor = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let puerto = servidor.local_addr().unwrap().port();
        let (avisar, recibido) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (mut socket, _) = servidor.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            avisar
                .send(String::from_utf8_lossy(&buf[..n]).to_string())
                .ok();
            let cabecera = format!(
                "HTTP/1.1 200 .\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                respuesta.len()
            );
            socket.write_all(cabecera.as_bytes()).await.ok();
            socket.write_all(respuesta.as_bytes()).await.ok();
            socket.shutdown().await.ok();
        });

        (format!("http://127.0.0.1:{puerto}/token"), recibido)
    }

    /// Hace de navegador: saca de la direccion el puerto y el `state`, y llama de vuelta.
    fn navegador_que_acepta(url: &str) -> Result<(), String> {
        let params = consulta(url);
        let buscar = |c: &str| {
            params
                .iter()
                .find(|(k, _)| k == c)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let vuelta = buscar("redirect_uri");
        let estado = buscar("state");
        tokio::spawn(async move {
            let destino = vuelta.trim_start_matches("http://");
            let mut socket = tokio::net::TcpStream::connect(destino).await.unwrap();
            let peticion =
                format!("GET /?code=un-codigo&state={estado} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            socket.write_all(peticion.as_bytes()).await.unwrap();
            let mut basura = Vec::new();
            socket.read_to_end(&mut basura).await.ok();
        });
        Ok(())
    }

    fn oauth_falso(token: &str) -> Oauth {
        Oauth {
            autorizar: "http://127.0.0.1:1/autorizar",
            token: Box::leak(token.to_string().into_boxed_str()),
            ambito: "un.ambito",
            cuota: "http://127.0.0.1:1/cuota",
            extra: &[("access_type", "offline")],
        }
    }

    /// El flujo entero: abrir puerto, mandar el navegador, recoger el codigo y canjearlo.
    #[tokio::test]
    async fn se_conecta_una_cuenta_de_punta_a_punta() {
        let (url, recibido) =
            proveedor_falso(r#"{"access_token":"a","refresh_token":"el-bueno","expires_in":3599}"#)
                .await;
        let oauth = oauth_falso(&url);

        let c = conectar(&oauth, "id-de-la-app", "secreto", &navegador_que_acepta)
            .await
            .unwrap();
        assert_eq!(c.refresh_token, "el-bueno");

        // Y se comprueba que el canje llevaba lo que el protocolo exige. Sin el
        // `code_verifier`, el proveedor de verdad rechaza el canje con un error que no dice
        // por que; sin el `redirect_uri`, tambien.
        let peticion = recibido.await.unwrap();
        for exigido in [
            "grant_type=authorization_code",
            "code=un-codigo",
            "code_verifier=",
            "redirect_uri=",
            "client_id=id-de-la-app",
        ] {
            assert!(
                peticion.contains(exigido),
                "falta {exigido} en:\n{peticion}"
            );
        }
    }

    /// Un codigo que llega con otro `state` no se canjea.
    ///
    /// Es lo que impide que otro programa de esta maquina empuje un codigo suyo al puerto y
    /// consiga que LibreDisk conecte **su** cuenta en vez de la del usuario.
    #[tokio::test]
    async fn un_codigo_con_otro_state_se_rechaza() {
        let (url, _recibido) = proveedor_falso(r#"{"refresh_token":"no-deberia-usarse"}"#).await;
        let oauth = oauth_falso(&url);

        let impostor = |u: &str| -> Result<(), String> {
            let vuelta = consulta(u)
                .into_iter()
                .find(|(k, _)| k == "redirect_uri")
                .map(|(_, v)| v)
                .unwrap();
            tokio::spawn(async move {
                let destino = vuelta.trim_start_matches("http://");
                let mut socket = tokio::net::TcpStream::connect(destino).await.unwrap();
                socket
                    .write_all(b"GET /?code=el-mio&state=inventado HTTP/1.1\r\nHost: x\r\n\r\n")
                    .await
                    .unwrap();
                let mut basura = Vec::new();
                socket.read_to_end(&mut basura).await.ok();
            });
            Ok(())
        };

        let e = conectar(&oauth, "id", "secreto", &impostor)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("state"), "{e}");
    }

    /// El proveedor puede contestar sin token de refresco, y eso tiene un arreglo concreto
    /// que el mensaje tiene que decir. Google lo hace cuando la cuenta ya habia dado permiso.
    #[tokio::test]
    async fn sin_token_de_refresco_se_dice_que_hacer() {
        let (url, _recibido) = proveedor_falso(r#"{"access_token":"solo-este"}"#).await;
        let oauth = oauth_falso(&url);

        let e = conectar(&oauth, "id", "secreto", &navegador_que_acepta)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("retirale el permiso"), "{e}");
    }

    /// Una peticion suelta al puerto no tumba el flujo.
    ///
    /// El navegador pide cosas por su cuenta —`/favicon.ico` es la clasica— y cortar en la
    /// primera conexion dejaria colgado el flujo por una peticion que no es la del usuario.
    #[tokio::test]
    async fn una_peticion_que_no_es_la_buena_no_corta_el_flujo() {
        let (url, _recibido) = proveedor_falso(r#"{"refresh_token":"el-bueno"}"#).await;
        let oauth = oauth_falso(&url);

        let con_ruido = |u: &str| -> Result<(), String> {
            let params = consulta(u);
            let buscar = |c: &str| {
                params
                    .iter()
                    .find(|(k, _)| k == c)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            let vuelta = buscar("redirect_uri");
            let estado = buscar("state");
            tokio::spawn(async move {
                let destino = vuelta.trim_start_matches("http://").to_string();
                for peticion in [
                    "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
                    "GET /cualquier-cosa HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
                    format!("GET /?code=un-codigo&state={estado} HTTP/1.1\r\nHost: x\r\n\r\n"),
                ] {
                    let mut socket = tokio::net::TcpStream::connect(&destino).await.unwrap();
                    socket.write_all(peticion.as_bytes()).await.unwrap();
                    let mut basura = Vec::new();
                    socket.read_to_end(&mut basura).await.ok();
                }
            });
            Ok(())
        };

        let c = conectar(&oauth, "id", "secreto", &con_ruido).await.unwrap();
        assert_eq!(c.refresh_token, "el-bueno");
    }
}
