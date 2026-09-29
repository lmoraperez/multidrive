//! Traerse la configuracion al movil sin enlazar cuenta por cuenta.
//!
//! La maleta es el bulto sellado que MultiDrive guarda en las propias nubes del usuario, cifrado
//! con la frase de paso del volumen. Con **una sola cuenta** enlazada se abre y de ella salen
//! las demas.
//!
//! En un escritorio eso ahorra unos minutos. En un movil es otra cosa: enlazar cuatro cuentas
//! significa cuatro vueltas al navegador, cuatro pantallas de permisos y elegir la cuenta buena
//! cada vez, con el teclado del telefono. Con la maleta es una vuelta y una frase de paso.
//!
//! # Lo que no se pisa
//!
//! Una clave que aqui ya vale otra cosa **no se sobrescribe**. Puede ser una cuenta enlazada
//! desde este mismo telefono hace un rato, y cambiarla por una de una maleta vieja es la forma
//! mas rapida de quedarse sin acceso a ella. Se cuenta cuantas hubo y se dice.

use std::path::Path;
use std::sync::Arc;

use multidrive_backend::Backend;
use multidrive_core::maleta;

/// Lo que paso al traer la maleta, para poder contarlo en la pantalla.
pub struct Traida {
    /// En que cuenta se encontro.
    pub donde: String,
    /// Claves escritas que no estaban.
    pub nuevas: usize,
    /// Claves que ya estaban con el mismo valor.
    pub iguales: usize,
    /// Claves que aqui valen otra cosa y **no se han tocado**.
    pub distintas: usize,
}

/// Busca la maleta en las cuentas ya configuradas y escribe lo que falte.
///
/// **Bloquea**: son tantas peticiones como cuentas haya. Nunca desde el hilo de la interfaz.
pub fn recuperar(config: &str, frase: &str) -> Result<Traida, String> {
    let ruta = Path::new(config);
    let backends = multidrive_backend::live::configurados_de(ruta)
        .map_err(|e| format!("configuracion invalida: {e}"))?;
    if backends.is_empty() {
        return Err("no hay ninguna cuenta enlazada todavia: conecta una primero".into());
    }
    let backends: Vec<Arc<dyn Backend>> = backends
        .into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect();

    let (donde, m) = crate::sesion::en_el_runtime(async move {
        maleta::recoger(&backends, frase)
            .await
            .map_err(|e| e.to_string())
    })?
    .ok_or(
        "no hay ninguna maleta en esas cuentas.\nEn el equipo que si tiene la configuracion, \
         abre el volumen una vez: se guarda sola.",
    )?;

    // Lo que ya hay en **este** fichero, que es lo unico que cuenta aqui: en Android no existen
    // ni las variables de entorno ni los sitios de siempre.
    let actuales =
        multidrive_backend::config::Config::desde(std::slice::from_ref(&ruta.to_path_buf()))
            .map_err(|e| e.to_string())?;

    let mut nuevas = Vec::new();
    let (mut iguales, mut distintas) = (0, 0);
    for (clave, valor) in &m.claves {
        match actuales.get(clave) {
            None => nuevas.push((clave.clone(), valor.clone())),
            Some(v) if &v == valor => iguales += 1,
            Some(_) => distintas += 1,
        }
    }

    if !nuevas.is_empty() {
        multidrive_backend::config::guardar(ruta, &nuevas).map_err(|e| e.to_string())?;
    }

    Ok(Traida {
        donde,
        nuevas: nuevas.len(),
        iguales,
        distintas,
    })
}
