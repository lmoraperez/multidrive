//! Una peticion HTTP desde el navegador, y nada mas.
//!
//! Se usa el `fetch` del navegador porque es el unico que hay: ni sockets ni TLS propios. Eso
//! trae dos cosas gratis --el certificado lo comprueba el navegador, y la peticion sale con
//! las reglas de CORS que cada nube ya publica-- y una limitacion: solo se llega a donde el
//! proveedor deje entrar desde `multidrive.net`. Drive y Graph dejan; por eso son los dos.

use js_sys::{Function, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

/// Lo que vuelve: el codigo y el cuerpo entero.
pub(crate) struct Respuesta {
    pub estado: u16,
    pub cuerpo: Vec<u8>,
}

impl Respuesta {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.estado)
    }

    /// El cuerpo como texto, para los mensajes de error. Recortado: una pagina de error
    /// entera no cabe en una linea de la interfaz y no dice mas por ser larga.
    pub fn texto(&self) -> String {
        let t = String::from_utf8_lossy(&self.cuerpo);
        let t = t.trim();
        match t.char_indices().nth(300) {
            Some((i, _)) => format!("{}…", &t[..i]),
            None => t.to_string(),
        }
    }
}

/// Un GET, con el token en la cabecera si lo hay.
///
/// El token va **en la cabecera y nunca en la URL**: una URL acaba en el historial, en los
/// registros de quien la sirva y en los mensajes de error.
pub(crate) async fn get(url: &str, token: Option<&str>) -> Result<Respuesta, String> {
    let init = RequestInit::new();
    init.set_method("GET");
    let req = Request::new_with_str_and_init(url, &init).map_err(texto)?;
    if let Some(t) = token {
        req.headers()
            .set("Authorization", &format!("Bearer {t}"))
            .map_err(texto)?;
    }

    // Por `globalThis` y no por `window`: asi sirve igual dentro de un Web Worker, que es
    // donde acabara esto cuando Argon2 deje de congelar la pestana.
    let global = js_sys::global();
    let fetch: Function = Reflect::get(&global, &JsValue::from_str("fetch"))
        .map_err(texto)?
        .dyn_into()
        .map_err(texto)?;
    let promesa: Promise = fetch
        .call1(&global, &req)
        .map_err(texto)?
        .dyn_into()
        .map_err(texto)?;
    let resp: Response = JsFuture::from(promesa)
        .await
        .map_err(texto)?
        .dyn_into()
        .map_err(texto)?;

    let buffer = JsFuture::from(resp.array_buffer().map_err(texto)?)
        .await
        .map_err(texto)?;
    Ok(Respuesta {
        estado: resp.status(),
        cuerpo: Uint8Array::new(&buffer).to_vec(),
    })
}

/// Un error de JavaScript como texto. Casi siempre es un `TypeError` de `fetch`, que es como
/// el navegador dice «no hay red» o «CORS no deja».
pub(crate) fn texto(e: JsValue) -> String {
    e.as_string()
        .or_else(|| {
            Reflect::get(&e, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{e:?}"))
}

/// `encodeURIComponent`, para meter un nombre o una consulta en una URL.
pub(crate) fn codificar(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}
