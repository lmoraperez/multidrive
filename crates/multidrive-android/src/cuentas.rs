//! Las cuentas enlazadas en este telefono: mirarlas, y quitar una.
//!
//! # Quitar una cuenta no la saca del volumen
//!
//! Son dos cosas distintas y se parecen lo bastante como para confundirlas. Esto solo borra
//! unas claves del fichero de este telefono: la cuenta sigue en la nube, sus datos siguen ahi,
//! y **el volumen sigue contando con ella**. Si la cuenta formaba parte de un volumen, lo que
//! se consigue quitandola es que ese volumen ya no abra.
//!
//! Sacar un proveedor de verdad --moviendo sus trozos a los demas-- es otra operacion, cuesta
//! mover datos y hoy solo esta en el escritorio.

use std::path::Path;

use multidrive_backend::{Backend, config, live};

/// Quita del fichero todo lo de una cuenta. Devuelve lo que hay que contar.
///
/// Se niega si esa cuenta forma parte de un volumen **abierto ahora mismo**: ahi si se sabe
/// con certeza que quitarla lo dejaria sin poder abrirse, y una certeza que se calla es una
/// trampa.
///
/// Con el volumen cerrado no se puede saber, porque para saberlo habria que abrirlo. Entonces
/// se quita, y quien llama se encarga de avisar antes.
pub fn olvidar(config_ruta: &str, nombre: &str) -> Result<String, String> {
    if nombre.trim().is_empty() {
        return Err("no se dijo que cuenta".into());
    }
    if crate::sesion::nombres_en_uso().iter().any(|n| n == nombre) {
        return Err(format!(
            "«{nombre}» forma parte del volumen que tienes abierto. Quitarla de aqui no la \
             saca del volumen: lo dejaria sin poder abrirse. Cierra el volumen primero si aun \
             asi quieres quitarla."
        ));
    }

    // Todo lo de una cuenta son claves `MULTIDRIVE_<RANURA>_...`. El guion bajo final importa:
    // sin el, quitar `gdrive` se llevaria tambien a `gdrive1`, que es otra cuenta.
    let prefijo = format!("MULTIDRIVE_{}_", nombre.to_uppercase());
    let ruta = Path::new(config_ruta);
    let quitadas = config::quitar(ruta, &prefijo).map_err(|e| e.to_string())?;
    if quitadas == 0 {
        return Err(format!(
            "«{nombre}» no estaba en la configuracion de este telefono."
        ));
    }

    // Se comprueba que de verdad se fue. En el escritorio una cuenta puede seguir viniendo del
    // entorno o del fichero de la maquina; aqui no deberia pasar, y justo por eso conviene
    // enterarse si pasa en vez de decir que se borro algo que sigue estando.
    let sigue = live::configurados_de(ruta)
        .map(|v| v.iter().any(|b| b.name() == nombre))
        .unwrap_or(false);
    if sigue {
        return Err(format!(
            "Se quitaron {quitadas} clave(s), pero «{nombre}» sigue apareciendo. Eso no deberia \
             pasar en un telefono: dilo, porque es un fallo."
        ));
    }
    Ok(format!("«{nombre}» ya no está en este teléfono."))
}

/// Si alguna de las cuentas enlazadas guarda una maleta.
///
/// **No hace falta la frase**: solo se pregunta si el bulto esta ahi, no se abre. Sirve para
/// el momento justo despues de enlazar una cuenta en un telefono nuevo: si esa cuenta ya trae
/// maleta, lo que toca es pedir la frase y traerse el resto, y no dejar a nadie buscando un
/// boton que no sabe que existe.
///
/// **Bloquea**: una peticion por cuenta. Nunca desde el hilo de la interfaz.
pub fn hay_maleta(config_ruta: &str) -> Result<bool, String> {
    let backends = multidrive_backend::live::configurados_de(Path::new(config_ruta))
        .map_err(|e| format!("configuracion invalida: {e}"))?;
    if backends.is_empty() {
        return Ok(false);
    }
    let backends: Vec<std::sync::Arc<dyn Backend>> = backends
        .into_iter()
        .map(|b| std::sync::Arc::new(b) as std::sync::Arc<dyn Backend>)
        .collect();

    crate::sesion::en_el_runtime(async move { Ok(multidrive_core::maleta::hay(&backends).await) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quitar algo que no esta se dice, no se calla. Devolver «hecho» sobre una cuenta que
    /// nunca estuvo deja a quien mira la lista preguntandose que paso.
    #[test]
    fn quitar_lo_que_no_hay_se_dice() {
        let e = olvidar("no/existe.conf", "gdrive").unwrap_err();
        assert!(e.contains("no estaba"), "{e}");
    }

    /// Sin nombre no se hace nada. Un prefijo vacio se llevaria por delante el fichero entero.
    #[test]
    fn sin_nombre_no_se_borra_nada() {
        let e = olvidar("no/existe.conf", "  ").unwrap_err();
        assert!(e.contains("que cuenta"), "{e}");
    }

    /// Sin cuentas no hay maleta que buscar, y eso no es un error: es el telefono el primer
    /// dia, que es cuando mas se mira esta pantalla.
    #[test]
    fn sin_cuentas_no_hay_maleta_y_no_falla() {
        assert!(!hay_maleta("no/existe.conf").unwrap());
    }
}
