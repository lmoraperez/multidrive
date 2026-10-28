//! MultiDrive en el navegador, de solo lectura.
//!
//! Lo que hace el escritorio al abrir un volumen, pero desde una pagina: entrar en una cuenta,
//! leer la cabecera, abrir la maleta con la frase para saber donde esta cada una de las demas,
//! entrar en ellas, y navegar y descargar. **Los datos van del navegador a las nubes del
//! usuario directamente**: por `multidrive.net` no pasa ni un byte, y el descifrado se hace
//! aqui, con el mismo codigo que el escritorio.
//!
//! # Lo que no hace, a proposito
//!
//! - **No escribe.** Ni subir, ni crear, ni borrar: los backends de [`nube`] no saben.
//! - **No guarda nada.** Ni tokens ni frase: todo vive en esta [`Sesion`] y se va al cerrar
//!   la pestana. Un token de refresco en el almacenamiento del navegador seria acceso
//!   permanente al alcance de cualquier fallo de la pagina.
//! - **No inicia sesion.** Eso lo hace la pagina con la ventana de cada proveedor; aqui solo
//!   llega el token de acceso, ya hecho.
//!
//! # Como se usa desde JavaScript
//!
//! Todo lo que devuelve datos devuelve una promesa con un JSON en texto, salvo [`Sesion::leer`],
//! que da los bytes. Los errores son texto en castellano, listos para enseñarse.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use multidrive_backend_api::Backend;
use multidrive_core::header::{self, Header};
use multidrive_core::maleta;
use multidrive_core::tree::{Kind, Volume};
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

pub mod nube;
mod red;

use nube::{Nube, Tipo};

/// Cuanta memoria se dedica a trozos ya descifrados. Menos que en el escritorio: una pestana
/// comparte memoria con todo lo demas que tenga abierto el navegador.
const CACHE: usize = 64 * 1024 * 1024;

/// Una visita a la pagina: las cuentas conectadas y, si ya se abrio, el volumen.
#[wasm_bindgen]
pub struct Sesion {
    estado: Rc<RefCell<Estado>>,
}

#[derive(Default)]
struct Estado {
    cuentas: BTreeMap<String, Arc<Nube>>,
    cabecera: Option<Header>,
    volumen: Option<Rc<Volume>>,
}

/// Una cuenta del volumen, tal como se le cuenta a la pagina.
#[derive(Serialize)]
struct CuentaDelVolumen {
    nombre: String,
    /// `gdrive`, `onedrive`, o `null` si es de un tipo que el navegador no lee.
    tipo: Option<&'static str>,
    conectada: bool,
}

#[derive(Serialize)]
struct Cabecera {
    topologia: String,
    cuentas: Vec<CuentaDelVolumen>,
}

#[derive(Serialize)]
struct Entrada {
    nombre: String,
    dir: bool,
    /// Bytes si es un fichero; entradas si es un directorio.
    len: u64,
    modificado: Option<u64>,
}

#[wasm_bindgen]
impl Sesion {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            estado: Rc::new(RefCell::new(Estado::default())),
        }
    }

    /// Conecta una cuenta, o le cambia el token si ya estaba.
    ///
    /// `nombre` es el de la ranura (`gdrive`, `onedrive2`…) y de el sale el tipo. `raiz` vacia
    /// es la de siempre, `/multidrive`.
    pub fn cuenta(&self, nombre: &str, raiz: &str, token: &str) -> Result<(), JsError> {
        let tipo = Tipo::de_ranura(nombre).ok_or_else(|| {
            JsError::new(&format!(
                "'{nombre}' no es una cuenta de Drive ni de OneDrive, que son las que la \
                 version web sabe leer"
            ))
        })?;
        let mut estado = self.estado.borrow_mut();
        if let Some(ya) = estado.cuentas.get(nombre) {
            ya.renovar(token);
            return Ok(());
        }
        let raiz = if raiz.trim().is_empty() {
            nube::RAIZ_POR_DEFECTO
        } else {
            raiz
        };
        estado.cuentas.insert(
            nombre.to_string(),
            Arc::new(Nube::nueva(nombre, tipo, raiz, token)),
        );
        Ok(())
    }

    /// Desconecta una cuenta.
    ///
    /// La pagina conecta la primera con un nombre provisional, porque hasta leer la cabecera no
    /// sabe de que ranura es, y luego la vuelve a conectar con el bueno.
    pub fn quitar(&self, nombre: &str) {
        self.estado.borrow_mut().cuentas.remove(nombre);
    }

    /// Lee la cabecera en las cuentas conectadas y dice de que cuentas es el volumen.
    ///
    /// No necesita la frase: la cabecera va en claro, y es lo que permite decir que cuentas
    /// faltan antes de pedir nada mas.
    pub fn cabecera(&self) -> js_sys::Promise {
        let estado = Rc::clone(&self.estado);
        future_to_promise(async move {
            let cuentas = conectadas(&estado);
            let leida = header::read(&cuentas)
                .await
                .map_err(error)?
                .ok_or_else(|| {
                    JsValue::from_str(
                        "en esta cuenta no hay ningun volumen de MultiDrive. Si lo tienes en \
                         otra carpeta, indicala",
                    )
                })?;

            let mut st = estado.borrow_mut();
            let respuesta = Cabecera {
                topologia: leida.layout.clone(),
                cuentas: leida
                    .backends
                    .iter()
                    .map(|n| CuentaDelVolumen {
                        nombre: n.clone(),
                        tipo: Tipo::de_ranura(n).map(|t| match t {
                            Tipo::Gdrive => "gdrive",
                            Tipo::Onedrive => "onedrive",
                        }),
                        conectada: st.cuentas.contains_key(n),
                    })
                    .collect(),
            };
            st.cabecera = Some(leida);
            json(&respuesta)
        })
    }

    /// Abre la maleta con la frase y devuelve la carpeta de cada cuenta del volumen.
    ///
    /// Es para poder entrar en las demas cuentas sabiendo donde mirar: la maleta trae la
    /// configuracion entera del equipo que la hizo, pero aqui **solo se usa la carpeta**. Los
    /// tokens que lleva no se tocan: son de la aplicacion de escritorio, y la web entra en cada
    /// cuenta con los suyos, que caducan.
    ///
    /// Si no hay maleta no es un error: se devuelve un objeto vacio, y la pagina usa la carpeta
    /// de siempre.
    pub fn maleta(&self, frase: String) -> js_sys::Promise {
        let estado = Rc::clone(&self.estado);
        future_to_promise(async move {
            let cuentas = conectadas(&estado);
            let raices: BTreeMap<String, String> =
                match maleta::recoger(&cuentas, &frase).await.map_err(error)? {
                    Some((_, m)) => raices_de(&m.claves),
                    None => BTreeMap::new(),
                };
            json(&raices)
        })
    }

    /// Abre el volumen con la frase. Hacen falta todas las cuentas que pida la cabecera.
    pub fn abrir(&self, frase: String) -> js_sys::Promise {
        let estado = Rc::clone(&self.estado);
        future_to_promise(async move {
            let (cabecera, ordenadas) = {
                let st = estado.borrow();
                let cabecera = st.cabecera.clone().ok_or_else(|| {
                    JsValue::from_str("primero hay que leer la cabecera del volumen")
                })?;
                let faltan: Vec<&str> = cabecera
                    .backends
                    .iter()
                    .filter(|n| !st.cuentas.contains_key(*n))
                    .map(String::as_str)
                    .collect();
                if !faltan.is_empty() {
                    return Err(JsValue::from_str(&format!(
                        "faltan cuentas del volumen: {}",
                        faltan.join(", ")
                    )));
                }
                // En el orden de la cabecera, que es parte del formato: el fragmento `i` es el
                // de la cuenta `i`.
                let ordenadas: Vec<Arc<dyn Backend>> = cabecera
                    .backends
                    .iter()
                    .map(|n| Arc::clone(&st.cuentas[n]) as Arc<dyn Backend>)
                    .collect();
                (cabecera, ordenadas)
            };

            let almacen = cabecera.almacen(ordenadas, &frase, CACHE).map_err(error)?;
            let volumen = Volume::open(almacen).await.map_err(error)?;
            let generacion = volumen.root().generation;
            estado.borrow_mut().volumen = Some(Rc::new(volumen));
            json(&serde_json::json!({ "generacion": generacion }))
        })
    }

    /// Lo que hay en un directorio. `""` o `"/"` es la raiz.
    pub fn listar(&self, ruta: String) -> js_sys::Promise {
        let estado = Rc::clone(&self.estado);
        future_to_promise(async move {
            let volumen = abierto(&estado)?;
            let dentro = volumen.read_dir(&ruta).await.map_err(error)?;
            let entradas: Vec<Entrada> = dentro
                .into_iter()
                .map(|(nombre, e)| Entrada {
                    nombre,
                    dir: e.kind == Kind::Dir,
                    len: e.len,
                    modificado: e.modificado,
                })
                .collect();
            json(&entradas)
        })
    }

    /// Un fichero entero, reconstruido y descifrado, en un `Uint8Array`.
    ///
    /// Entero en memoria: es lo que hay hasta que la descarga vaya por trozos. Para lo que se
    /// mira desde un navegador basta; un fichero de varios gigas no.
    pub fn leer(&self, ruta: String) -> js_sys::Promise {
        let estado = Rc::clone(&self.estado);
        future_to_promise(async move {
            let volumen = abierto(&estado)?;
            let mut bytes = Vec::new();
            volumen.read_file(&ruta, &mut bytes).await.map_err(error)?;
            Ok(js_sys::Uint8Array::from(bytes.as_slice()).into())
        })
    }
}

fn conectadas(estado: &Rc<RefCell<Estado>>) -> Vec<Arc<dyn Backend>> {
    estado
        .borrow()
        .cuentas
        .values()
        .map(|c| Arc::clone(c) as Arc<dyn Backend>)
        .collect()
}

fn abierto(estado: &Rc<RefCell<Estado>>) -> Result<Rc<Volume>, JsValue> {
    estado
        .borrow()
        .volumen
        .clone()
        .ok_or_else(|| JsValue::from_str("el volumen todavia no esta abierto"))
}

/// La carpeta de cada ranura, sacada de las claves de una maleta.
///
/// `MULTIDRIVE_GDRIVE2_ROOT=/otra` es la carpeta de `gdrive2`. Las ranuras sin `ROOT` no salen:
/// usan la de siempre, y quien pregunte ya lo sabe.
fn raices_de(claves: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    claves
        .iter()
        .filter_map(|(k, v)| {
            let ranura = k.strip_prefix("MULTIDRIVE_")?.strip_suffix("_ROOT")?;
            Tipo::de_ranura(&ranura.to_lowercase())?;
            Some((ranura.to_lowercase(), v.clone()))
        })
        .collect()
}

fn json<T: Serialize>(valor: &T) -> Result<JsValue, JsValue> {
    serde_json::to_string(valor)
        .map(|s| JsValue::from_str(&s))
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

fn error(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn de_la_maleta_solo_salen_las_carpetas_de_lo_que_se_sabe_leer() {
        let claves: BTreeMap<String, String> = [
            ("MULTIDRIVE_GDRIVE_ROOT", "/multidrive"),
            ("MULTIDRIVE_ONEDRIVE2_ROOT", "/Documentos/md"),
            ("MULTIDRIVE_GDRIVE_REFRESH_TOKEN", "secreto"),
            ("MULTIDRIVE_FS_ROOT", "C:/datos"),
            ("MULTIDRIVE_S3_ROOT", "prefijo"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let raices = raices_de(&claves);
        assert_eq!(raices.len(), 2);
        assert_eq!(raices["gdrive"], "/multidrive");
        assert_eq!(raices["onedrive2"], "/Documentos/md");
        // El token de refresco no sale de aqui por ningun camino.
        assert!(!raices.values().any(|v| v == "secreto"));
    }
}
