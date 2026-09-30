//! Retirar el volumen de las cuentas: borrar sus fragmentos, su raiz y su cabecera.
//!
//! **No se puede deshacer y no hay papelera.** Por eso va en dos llamadas y no en una: primero
//! se mira lo que hay --cuantos fragmentos, cuanto ocupan, si alguna cuenta guarda ademas la
//! configuracion sellada-- y solo despues, con esa cuenta delante, se confirma.
//!
//! # Lo que se pide para confirmar
//!
//! El nombre de **un** proveedor, el que le toque por posicion. No la lista entera: con muchas
//! cuentas esa lista se copia y se pega sin leerla, que es exactamente lo contrario de lo que
//! una confirmacion tiene que conseguir. El numero sale de la sal, asi que es el mismo en la
//! mirada y en la confirmacion.
//!
//! # Lo que no se toca
//!
//! Solo lo que es del volumen: el prefijo de fragmentos, el puntero de raiz y la cabecera. Si
//! hay otras cosas guardadas en esa misma carpeta se quedan donde estan. Borrar todo lo que
//! hubiera debajo seria mas facil de escribir y mucho peor de explicar.

use std::path::Path;
use std::sync::Arc;

use multidrive_backend::Backend;
use multidrive_core::{header, retirar};

/// Los proveedores del fichero, ya envueltos.
fn proveedores(config: &str) -> Result<Vec<Arc<dyn Backend>>, String> {
    let backends = multidrive_backend::live::configurados_de(Path::new(config))
        .map_err(|e| format!("configuracion invalida: {e}"))?;
    if backends.is_empty() {
        return Err("no hay ninguna cuenta enlazada".into());
    }
    Ok(backends
        .into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect())
}

/// Lo que se va a borrar, en JSON, sin borrar nada.
///
/// **Bloquea y tarda**: enumera cada cuenta entera. Es lo que convierte «se va a borrar el
/// volumen» en una cifra que se puede mirar antes de decir que si.
pub fn mirar(config: &str) -> Result<String, String> {
    let backends = proveedores(config)?;

    crate::sesion::en_el_runtime(async move {
        let cabecera = header::read(&backends)
            .await
            .map_err(|e| format!("no se pudo leer la cabecera: {e}"))?
            .ok_or("no hay ningun volumen en estas cuentas")?;

        // Los configurados tienen que ser los del volumen. Sin esto se podria retirar media
        // mitad --si una cuenta se quedo fuera de la configuracion-- y diriamos que termino,
        // dejando la otra mitad como basura que ya nadie sabe de quien era.
        cabecera.check_backends(&backends).map_err(|_| {
            "las cuentas enlazadas en este telefono no son las del volumen. No se borra nada: \
             retirarlo a medias dejaria fragmentos sueltos que ya nadie podria identificar."
                .to_string()
        })?;

        let inventario = retirar::inventariar(&backends).await.map_err(|e| {
            format!("no se borra nada: sin poder mirarlo entero el recuento seria falso ({e})")
        })?;

        let nombres: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();
        let cual = retirar::a_quien_nombrar(&cabecera.salt, nombres.len());

        serde_json::to_string(&serde_json::json!({
            "topologia": cabecera.layout().map(|l| l.to_string()).unwrap_or_default(),
            "proveedores": nombres,
            "fragmentos": inventario.fragmentos(),
            "bytes": inventario.bytes(),
            // Se nombra aparte porque es lo unico de la lista que sigue sirviendo para algo
            // despues, y quien borra tiene que saber que tambien se va.
            "con_maleta": inventario.con_maleta,
            "cual": cual,
            "esperado": nombres[cual].clone(),
        }))
        .map_err(|e| e.to_string())
    })
}

/// Retira el volumen. `confirmacion` tiene que ser el nombre que dijo [`mirar`].
///
/// **Bloquea y tarda.** No se puede deshacer.
pub fn retirar_volumen(config: &str, confirmacion: &str) -> Result<String, String> {
    let backends = proveedores(config)?;
    let confirmacion = confirmacion.trim().to_string();

    crate::sesion::en_el_runtime(async move {
        let cabecera = header::read(&backends)
            .await
            .map_err(|e| format!("no se pudo leer la cabecera: {e}"))?
            .ok_or("no hay ningun volumen en estas cuentas")?;
        cabecera.check_backends(&backends).map_err(|_| {
            "las cuentas enlazadas en este telefono no son las del volumen. No se borra nada."
                .to_string()
        })?;

        // Se vuelve a calcular en vez de fiarse de lo que llegue de la pantalla: el numero sale
        // de la sal, asi que es el mismo, y comprobarlo aqui es lo que hace que la confirmacion
        // sea una comprobacion y no una formalidad.
        let nombres: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();
        let cual = retirar::a_quien_nombrar(&cabecera.salt, nombres.len());
        if confirmacion != nombres[cual] {
            return Err(format!(
                "«{confirmacion}» no es la cuenta número {}. No se ha borrado nada.",
                cual + 1
            ));
        }

        let hecho = retirar::retirar(&backends)
            .await
            .map_err(|e| e.to_string())?;

        let mut dicho = String::new();
        for (nombre, n, _) in &hecho.por_proveedor {
            let mas = if hecho.con_maleta.iter().any(|m| m == nombre) {
                ", raíz, cabecera y la configuración sellada"
            } else {
                ", raíz y cabecera"
            };
            dicho.push_str(&format!("{nombre}: {n} fragmento(s){mas}\n"));
        }
        dicho.push_str(&format!(
            "\nRetirados {} fragmento(s) de {} cuenta(s).\n\nLa carpeta sigue existiendo en cada \
             una, vacía de lo que era del volumen. Si guardaste ahí otras cosas, siguen donde \
             estaban.",
            hecho.fragmentos(),
            backends.len()
        ));
        Ok(dicho)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sin cuentas no hay nada que retirar, y se dice antes de tocar la red.
    #[test]
    fn sin_cuentas_no_hay_nada_que_retirar() {
        let e = mirar("no/existe.conf").unwrap_err();
        assert!(e.contains("ninguna cuenta"), "{e}");
    }

    /// Y confirmar tampoco pasa de ahi: el camino que borra no puede tener menos guardas que
    /// el que solo mira.
    #[test]
    fn confirmar_sin_cuentas_tampoco_hace_nada() {
        let e = retirar_volumen("no/existe.conf", "gdrive").unwrap_err();
        assert!(e.contains("ninguna cuenta"), "{e}");
    }
}
