//! Meter en un volumen ya creado los proveedores que estan configurados y no lo forman.
//!
//! # Lo que esto gana, y lo que no
//!
//! «Anadir un proveedor para tener mas sitio» suena a una cosa y en realidad son dos, y la
//! diferencia no es un detalle:
//!
//! - En un volumen **`lineal`** cada trozo va entero a un proveedor. Llega uno nuevo, se le
//!   muda la fraccion que le toca, y el volumen **crece** lo que aporte el que llega.
//! - En los demas --espejo, paridad, union-- cada trozo se parte entre todos. Anadir uno no
//!   agranda nada: lo que se gana es **margen de caidas**. Ganar sitio ahi exige repartir otra
//!   vez todos los trozos entre mas proveedores, o sea **reescribir el volumen entero**, que
//!   puede tardar horas o dias. Eso no se hace desde un telefono.
//!
//! Esta diferencia se dice en la pantalla antes de empezar, y es la razon de que [`mirar`]
//! exista: enterarse de que lo que se gana no es sitio **despues** de esperar seria la peor
//! manera de enterarse.
//!
//! # Lo que se hace, y en que orden
//!
//! Solo las dos operaciones que **no** reescriben el volumen entero. La cabecera se publica al
//! final y solo si todo salio bien: hasta ese momento el volumen se lee exactamente como
//! antes, y si algo falla a mitad no se ha prometido nada que no este.

use std::path::Path;
use std::sync::Arc;

use multidrive_backend::Backend;
use multidrive_core::header::{self, Header};
use multidrive_core::rebalance::{self, Cambio};
use multidrive_raid::Layout;

use crate::sesion;

/// Las cuentas repartidas en dos grupos: las que el volumen ya usa y las que sobran.
///
/// Tiene nombre propio porque la pareja de vectores aparece en la firma y en quien la recibe,
/// y dos `Vec<Arc<dyn Backend>>` seguidos no dicen cual es cual.
type Bandos = (Vec<Arc<dyn Backend>>, Vec<Arc<dyn Backend>>);

/// Los del volumen y los que estan configurados pero no lo forman.
fn bandos(cabecera: &Header, configurados: &[Arc<dyn Backend>]) -> Result<Bandos, String> {
    let mut del_volumen = Vec::new();
    for nombre in &cabecera.backends {
        let Some(b) = configurados.iter().find(|b| b.name() == nombre) else {
            return Err(format!(
                "Falta «{nombre}», que forma parte del volumen. Enlázala antes de añadir otras."
            ));
        };
        del_volumen.push(Arc::clone(b));
    }
    let nuevos = configurados
        .iter()
        .filter(|b| !cabecera.backends.iter().any(|n| n == b.name()))
        .cloned()
        .collect();
    Ok((del_volumen, nuevos))
}

/// Que topologia sale de anadir los que sobran, y que se gana: `"espacio"` o `"margen"`.
fn plan(antes: Layout, cuantos: usize) -> Result<(Layout, &'static str), String> {
    let lineal = matches!(antes, Layout::Lineal { .. });
    let cambio = if lineal {
        Cambio::MasEspacio
    } else {
        Cambio::MasSeguridad
    };
    let destino = rebalance::planear(antes, cuantos, cambio).map_err(|e| e.to_string())?;

    if lineal {
        Ok((destino, "espacio"))
    } else if rebalance::conserva_lo_escrito(antes, destino) {
        Ok((destino, "margen"))
    } else {
        Err(format!(
            "Pasar de {antes} a {destino} mueve todos los fragmentos de todos los trozos: es \
             reescribir el volumen entero, que puede tardar horas. Eso no se hace desde el \
             teléfono; se hace desde el escritorio, que puede ir informando."
        ))
    }
}

/// Cuanto sitio libre suman estos proveedores, o nada si alguno no lo dice.
/// Una tras otra y no en paralelo a proposito: son las cuentas que **no** forman el volumen,
/// o sea una o dos, y pedirlas en serie cuesta un segundo de mas y ahorra una dependencia.
async fn suma_libre(backends: &[Arc<dyn Backend>]) -> Option<u64> {
    let mut total = 0u64;
    for b in backends {
        total += b.cuota().await.ok()?.libre()?;
    }
    Some(total)
}

/// Que pasaria si se ampliara, **sin tocar nada**.
///
/// Cuesta una peticion por proveedor nuevo, no una enumeracion del arbol.
pub fn mirar(id: i64, config: &str) -> Result<String, String> {
    let abierto = sesion::sesion(id)?;
    let configurados = configurados(config)?;
    let antes: Layout = abierto.cabecera.layout().map_err(|e| e.to_string())?;
    let (_, nuevos) = bandos(&abierto.cabecera, &configurados)?;

    let nombres: Vec<String> = nuevos.iter().map(|b| b.name().to_string()).collect();
    if nuevos.is_empty() {
        return Ok(serde_json::json!({
            "nuevos": nombres,
            "antes": antes.to_string(),
            "problema": "No hay ninguna cuenta enlazada que el volumen no use ya. Enlaza otra \
                         y vuelve aquí.",
        })
        .to_string());
    }

    let (destino, gana) = match plan(antes, nuevos.len()) {
        Ok(p) => p,
        Err(e) => {
            return Ok(serde_json::json!({
                "nuevos": nombres,
                "antes": antes.to_string(),
                "problema": e,
            })
            .to_string());
        }
    };

    let suma = sesion::en_el_runtime(async {
        Ok(if gana == "espacio" {
            suma_libre(&nuevos).await
        } else {
            // Una copia o una paridad mas no anaden ni un byte, y decir lo contrario seria lo
            // que la gente espera oir en vez de lo que pasa.
            Some(0)
        })
    })?;

    Ok(serde_json::json!({
        "nuevos": nombres,
        "antes": antes.to_string(),
        "despues": destino.to_string(),
        "gana": gana,
        "suma": suma,
    })
    .to_string())
}

/// Amplia de verdad. Tarda: recorre el arbol y escribe en las cuentas.
///
/// Despues de esto la sesion abierta se queda con la cabecera **vieja** en la mano, asi que
/// quien llame tiene que cerrar y volver a abrir el volumen. Se dice en la respuesta.
pub fn hacer(id: i64, config: &str) -> Result<String, String> {
    let abierto = sesion::sesion(id)?;
    if !abierto.escribe() {
        return Err("El volumen está abierto solo para leer.".into());
    }
    let configurados = configurados(config)?;
    let antes: Layout = abierto.cabecera.layout().map_err(|e| e.to_string())?;
    let (del_volumen, nuevos) = bandos(&abierto.cabecera, &configurados)?;
    if nuevos.is_empty() {
        return Err("No hay ninguna cuenta enlazada que el volumen no use ya.".into());
    }
    let (destino, gana) = plan(antes, nuevos.len())?;

    let todos: Vec<Arc<dyn Backend>> = del_volumen.iter().chain(&nuevos).cloned().collect();
    let nombres: Vec<String> = todos.iter().map(|b| b.name().to_string()).collect();
    let llegan: Vec<String> = nuevos.iter().map(|b| b.name().to_string()).collect();

    sesion::en_el_runtime(async move {
        // Lo que suman las que llegan, **antes** de moverlas: es lo que se va a anadir, y
        // decirlo al terminar es lo unico que convierte «ahora es lineal:3» en una cifra.
        // En margen es cero y se dice cero: una copia mas no anade ni un byte.
        let suma = if gana == "espacio" {
            suma_libre(&nuevos).await
        } else {
            Some(0)
        };

        let v = abierto.volumen.lock().await;

        let (trozos, ilegibles) = v.walk().await;
        if !ilegibles.is_empty() {
            // Un trozo que no se puede leer tampoco se puede mudar, y lo que no se muda se
            // queda donde la cabecera nueva ya no lo busca.
            return Err(format!(
                "Hay {} ruta(s) que no se pueden leer, empezando por «{}». Revisa la salud del \
                 volumen antes de ampliarlo.",
                ilegibles.len(),
                ilegibles[0].0
            ));
        }

        let pesos = if gana == "espacio" {
            multidrive_core::pesos::pesos_ampliados(&v.store().pesos(), &nuevos).await
        } else {
            abierto.cabecera.pesos.clone()
        };

        let fallidos = if gana == "espacio" {
            v.store()
                .mudar_reparto(destino, &pesos, &todos, &trozos)
                .await
                .map_err(|e| e.to_string())?
                .fallidos
        } else {
            v.store()
                .ampliar_redundancia(destino, &nuevos, &trozos)
                .await
                .map_err(|e| e.to_string())?
                .fallidos
        };
        if !fallidos.is_empty() {
            // La cabecera es la que promete donde esta cada cosa. Publicarla con trozos a
            // medias prometeria algo que no esta, que es peor que no haber ampliado.
            return Err(format!(
                "{} trozo(s) no se pudieron escribir, así que no se cambia nada: el volumen \
                 sigue como estaba. Vuelve a intentarlo, que lo que sí se subió no se repite.",
                fallidos.len()
            ));
        }

        // El unico punto de compromiso. Hasta aqui el volumen se leia con lo de antes.
        let cabecera = Header {
            version: abierto.cabecera.version,
            layout: destino.to_string(),
            salt: abierto.cabecera.salt.clone(),
            reparto: abierto.cabecera.reparto,
            backends: nombres,
            pesos,
        };
        header::write(&todos, &cabecera)
            .await
            .map_err(|e| e.to_string())?;

        Ok(serde_json::json!({
            "antes": antes.to_string(),
            "despues": destino.to_string(),
            "gana": gana,
            "anadidas": llegan,
            "suma": suma,
        })
        .to_string())
    })
}

/// Los proveedores enlazados en este telefono.
fn configurados(config: &str) -> Result<Vec<Arc<dyn Backend>>, String> {
    let v = multidrive_backend::live::configurados_de(Path::new(config))
        .map_err(|e| format!("Configuración inválida: {e}"))?;
    Ok(v.into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proveedores de mentira con los nombres que pida la prueba. En memoria: no tocan red ni
    /// disco, que es lo unico que les pedimos aqui --lo que se prueba es el reparto de nombres
    /// entre bandos, no lo que guarden--.
    fn backends(nombres: &[&str]) -> Vec<Arc<dyn Backend>> {
        nombres
            .iter()
            .map(|n| {
                Arc::new(multidrive_backend::OpendalBackend::memory(*n).unwrap())
                    as Arc<dyn Backend>
            })
            .collect()
    }

    /// En un volumen que no es lineal, anadir **no da sitio**: da margen. Es la distincion que
    /// esta pantalla existe para contar, asi que se fija con una prueba.
    #[test]
    fn en_espejo_se_gana_margen_y_no_espacio() {
        let (destino, gana) = plan(Layout::Mirror { n: 2 }, 1).expect("espejo admite uno mas");
        assert_eq!(gana, "margen");
        assert_eq!(destino.to_string(), "espejo:3");
    }

    /// Y en lineal si da sitio, que es toda su razon de ser.
    #[test]
    fn en_lineal_se_gana_espacio() {
        let (_, gana) = plan(Layout::Lineal { n: 2 }, 1).expect("lineal admite uno mas");
        assert_eq!(gana, "espacio");
    }

    /// Una cuenta que el volumen ya usa no cuenta como nueva.
    #[test]
    fn los_del_volumen_no_son_nuevos() {
        let cab = Header {
            version: 1,
            layout: "espejo:2".into(),
            salt: vec![0; 16],
            reparto: 0,
            backends: vec!["a".into(), "b".into()],
            pesos: vec![],
        };
        let configurados = backends(&["a", "b"]);
        let (del, nuevos) = bandos(&cab, &configurados).expect("estan las dos");
        assert_eq!(del.len(), 2);
        assert!(nuevos.is_empty());
    }

    /// Y si al volumen le falta una de las suyas, no se amplia: se dice cual falta.
    #[test]
    fn sin_una_del_volumen_no_se_amplia() {
        let cab = Header {
            version: 1,
            layout: "espejo:2".into(),
            salt: vec![0; 16],
            reparto: 0,
            backends: vec!["a".into(), "b".into()],
            pesos: vec![],
        };
        let configurados = backends(&["a", "c"]);
        // `unwrap_err` pediria `Debug` del lado Ok, y `dyn Backend` no lo tiene.
        let Err(e) = bandos(&cab, &configurados) else {
            panic!("tenia que negarse: falta «b», que forma parte del volumen");
        };
        assert!(e.contains("«b»"), "tiene que nombrar la que falta: {e}");
    }
}
