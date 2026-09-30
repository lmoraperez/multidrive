//! Crear un volumen desde el telefono.
//!
//! # Por que el movil tambien crea
//!
//! Porque si no, la primera vez hay que ir a buscar un ordenador. Quien instala MultiDrive en
//! el telefono ya tiene ahi sus cuentas de Drive y OneDrive --son las del propio telefono-- y
//! pedirle un escritorio para empezar es pedirle que no empiece.
//!
//! # Lo que no se hace aqui
//!
//! No se sobrescribe una cabecera que ya exista. Una cabecera nueva trae una sal nueva, y con
//! otra sal la clave es otra: todo lo que hubiera escrito antes quedaria inalcanzable sin
//! ningun aviso. El nucleo se niega, y aqui solo se traduce la negativa a algo que se pueda
//! leer en una pantalla de movil.

use std::path::Path;
use std::sync::Arc;

use multidrive_backend::Backend;
use multidrive_core::pesos::pesos_para;
use multidrive_core::tree::Volume;
use multidrive_core::{CoreError, header, maleta};
use multidrive_raid::Layout;

/// Los proveedores de ese fichero de configuracion, ya envueltos.
fn proveedores(config: &str) -> Result<Vec<Arc<dyn Backend>>, String> {
    let backends = multidrive_backend::live::configurados_de(Path::new(config))
        .map_err(|e| format!("configuracion invalida: {e}"))?;
    if backends.is_empty() {
        return Err("no hay ninguna cuenta enlazada todavia: conecta una primero".into());
    }
    Ok(backends
        .into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect())
}

/// Las topologias posibles con las cuentas que haya, en JSON, con el espacio que daria cada una.
///
/// **Bloquea**: una peticion de cupo por cuenta. Nunca desde el hilo de la interfaz.
pub fn topologias(config: &str) -> Result<String, String> {
    let backends = proveedores(config)?;
    let opciones = crate::sesion::en_el_runtime(async move {
        Ok(multidrive_core::topologias::medir(&backends).await)
    })?;
    serde_json::to_string(&opciones).map_err(|e| e.to_string())
}

/// Crea el volumen y devuelve, en JSON, lo que hay que contar de el.
///
/// **Bloquea** y cuesta segundos: cabecera en cada cuenta, primera raiz, y la maleta.
pub fn crear(config: &str, frase: &str, spec: &str) -> Result<String, String> {
    if frase.trim().is_empty() {
        return Err("hace falta una frase de paso: es la unica llave del volumen".into());
    }
    let layout: Layout = spec
        .parse()
        .map_err(|_| format!("topologia invalida: {spec}"))?;
    let backends = proveedores(config)?;

    // La maleta de mas abajo los necesita, y para entonces `crear` ya se los ha llevado.
    let para_la_maleta = backends.clone();
    let config = config.to_string();
    let frase = frase.to_string();

    crate::sesion::en_el_runtime(async move {
        let pesos = pesos_para(layout, &backends).await;
        let store = header::crear(backends.clone(), layout, &frase, pesos)
            .await
            .map_err(|e| match e {
                CoreError::YaHayVolumen => "ya hay un volumen en estas cuentas. Crear otro \
                     encima generaria una sal nueva y dejaria inalcanzable todo lo anterior, \
                     asi que no se hace."
                    .to_string(),
                CoreError::BackendCount { needed, given, .. } => format!(
                    "'{spec}' necesita {needed} cuenta(s) y hay {given}. Enlaza las que falten \
                     o elige otra opcion de la lista."
                ),
                otro => otro.to_string(),
            })?;

        let (volumen, degradados) = Volume::create(store).await.map_err(|e| e.to_string())?;
        if !degradados.is_empty() {
            // Un volumen recien creado al que ya le falta un proveedor no es un volumen sano, y
            // callarlo seria empezar mintiendo sobre la redundancia.
            return Err(format!(
                "el volumen se creo, pero estas cuentas no aceptaron la escritura: {}. No \
                 tiene la redundancia que dice tener.",
                degradados.join(", ")
            ));
        }

        let nombres = volumen.store().backend_names().join(", ");
        let maleta = guardar_maleta(&para_la_maleta, &config, &frase).await;

        serde_json::to_string(&serde_json::json!({
            "topologia": layout.to_string(),
            "proveedores": nombres,
            "tolera": layout.tolerated_failures(),
            "maleta": maleta,
        }))
        .map_err(|e| e.to_string())
    })
}

/// Sube la configuracion sellada, y **no tumba nada si falla**.
///
/// El volumen ya existe cuando esto corre. Perderlo porque no se pudo subir un fichero de
/// configuracion seria cambiar un incordio por un desastre: se devuelve el texto de lo que
/// paso y quien llama lo ensena como aviso.
async fn guardar_maleta(backends: &[Arc<dyn Backend>], config: &str, frase: &str) -> String {
    let claves = match maleta::claves_de(Path::new(config)) {
        Ok(c) => c,
        Err(e) => return format!("No se pudo preparar tu configuracion para guardarla: {e}"),
    };
    let bulto = match maleta::empaquetar(&claves, frase) {
        Ok(b) => b,
        Err(e) => return format!("No se pudo sellar tu configuracion: {e}"),
    };

    let resultados = maleta::guardar(backends, &bulto).await;
    let bien: Vec<String> = resultados
        .iter()
        .filter(|(_, f)| f.is_none())
        .map(|(n, _)| n.clone())
        .collect();

    if bien.is_empty() {
        let mal: Vec<String> = resultados
            .iter()
            .filter_map(|(n, f)| f.as_ref().map(|e| format!("{n}: {e}")))
            .collect();
        return format!(
            "El volumen esta creado, pero no se pudo guardar tu configuracion en las nubes: {}.",
            mal.join("; ")
        );
    }
    format!(
        "Tu configuracion queda guardada, cifrada con tu frase de paso, en {}. En otro equipo: \
         enlaza una de estas cuentas y pulsa «Traer a este equipo».",
        bien.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Una frase vacia se para **antes** de tocar la red. El volumen que saliera de ahi no
    /// tendria llave ninguna, y para cuando se notara ya habria cabecera en cuatro nubes.
    #[test]
    fn sin_frase_no_se_crea_nada() {
        let e = crear("no/existe.conf", "   ", "espejo:2").unwrap_err();
        assert!(e.contains("frase de paso"), "{e}");
    }

    /// Una topologia que no se entiende se dice antes de mirar las cuentas: el error util es
    /// «esto no es una topologia», no «no hay cuentas».
    #[test]
    fn una_topologia_inventada_se_dice_asi() {
        let e = crear("no/existe.conf", "frase", "trenzado:7").unwrap_err();
        assert!(e.contains("topologia invalida"), "{e}");
    }

    /// Sin cuentas enlazadas no se puede crear, y lo que se dice es lo que hay que hacer.
    #[test]
    fn sin_cuentas_se_dice_que_conecte_una() {
        let e = crear("no/existe.conf", "frase", "concat").unwrap_err();
        assert!(e.contains("conecta una primero"), "{e}");
    }
}
