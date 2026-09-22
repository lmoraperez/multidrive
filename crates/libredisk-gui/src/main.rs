//! Interfaz de escritorio.
//!
//! Enseña lo que a la linea de ordenes le cuesta enseñar: si el volumen esta sano, cuanto
//! margen de caidas le queda de verdad y que hay escrito **solo en esta maquina**. Eso ultimo
//! es lo que mas se malinterpreta de un volumen con escritura diferida, y verlo en una
//! ventana abierta vale mas que un aviso al final de una orden.
//!
//! # Sin Node
//!
//! El frontend es HTML, CSS y JavaScript a mano, servido de `web/`. Tauri no obliga a usar un
//! empaquetador, y meter npm aqui traeria un segundo ecosistema de dependencias a un proyecto
//! que acaba de fijar `Cargo.lock` precisamente para no tenerlas sueltas. Tambien mantiene la
//! construccion y CI con una sola herramienta.
//!
//! Eso obliga a `withGlobalTauri` en la configuracion: sin empaquetador no hay forma de
//! importar el modulo de JavaScript, asi que la API tiene que estar en `window`. No afloja
//! nada: lo que se puede llamar lo siguen decidiendo las capacidades, y ahi solo esta lo
//! minimo para que la ventana hable con este proceso.
//!
//! # La frase de paso
//!
//! Se escribe en la ventana y viaja por el canal interno de Tauri hasta este proceso, que es
//! el mismo proceso: no sale a ninguna parte, no se guarda y no se escribe en ningun registro.
//! Al cerrar el volumen se suelta.

// Sin consola detras de la ventana en Windows. En depuracion si, que es donde hace falta.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use libredisk_backend::{Backend, live};
use libredisk_core::ChunkHealth;
use libredisk_core::header::{self, Header};
use libredisk_core::tree::Volume;
use serde::Serialize;
use tokio::sync::Mutex;

/// Cache modesta: la ventana recorre el arbol para la salud, no sirve ficheros.
const CACHE: usize = 64 * 1024 * 1024;

/// Lo que se tiene abierto, si hay algo.
struct Abierto {
    volumen: Volume,
    header: Header,
}

#[derive(Default)]
struct Estado {
    abierto: Mutex<Option<Abierto>>,
}

/// Lo primero que se enseña: que hay configurado y si hace falta la frase.
#[derive(Serialize, Default)]
struct Arranque {
    proveedores: Vec<String>,
    /// Nombres tal y como los declara la cabecera, si es que hay volumen.
    del_volumen: Vec<String>,
    layout: Option<String>,
    hay_volumen: bool,
    abierto: bool,
    problema: Option<String>,
}

#[derive(Serialize)]
struct Resumen {
    layout: String,
    proveedores: Vec<String>,
    tolera: usize,
    sobrecoste: f64,
    reparto: u32,
    /// Generacion de la raiz publicada. Sube con cada cambio del arbol.
    generacion: u64,
}

#[derive(Serialize, Default)]
struct Salud {
    trozos: usize,
    completos: usize,
    degradados: usize,
    perdidos: usize,
    /// Proveedores que no se dejaron enumerar. Con alguno, las cifras son un suelo
    /// pesimista y no una medida: hay que decirlo.
    sin_enumerar: Vec<String>,
    ilegibles: Vec<String>,
}

#[derive(Serialize, Default)]
struct Pendiente {
    trozos: usize,
    bytes: u64,
    /// `true` si todo lo escrito esta ya en los proveedores.
    a_salvo: bool,
}

fn proveedores() -> Result<Vec<Arc<dyn Backend>>, String> {
    let configurados = live::configurados().map_err(|e| e.to_string())?;
    if configurados.is_empty() {
        return Err("no hay ningun proveedor configurado".into());
    }
    Ok(configurados
        .into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect())
}

#[tauri::command]
async fn arranque(estado: tauri::State<'_, Estado>) -> Result<Arranque, String> {
    let mut out = Arranque {
        abierto: estado.abierto.lock().await.is_some(),
        ..Default::default()
    };

    let backends = match proveedores() {
        Ok(b) => b,
        Err(e) => {
            out.problema = Some(e);
            return Ok(out);
        }
    };
    out.proveedores = backends.iter().map(|b| b.name().to_string()).collect();

    match header::read(&backends).await {
        Ok(Some(h)) => {
            out.hay_volumen = true;
            out.layout = Some(h.layout.clone());
            out.del_volumen = h.backends.clone();
        }
        Ok(None) => out.problema = Some("no hay ningun volumen en estos proveedores".into()),
        Err(e) => out.problema = Some(e.to_string()),
    }
    Ok(out)
}

#[tauri::command]
async fn abrir(frase: String, estado: tauri::State<'_, Estado>) -> Result<Resumen, String> {
    let backends = proveedores()?;
    let header = header::read(&backends)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no hay ningun volumen en estos proveedores")?;

    let store = header
        .almacen(backends, &frase, CACHE)
        .map_err(|e| e.to_string())?;

    // Una frase equivocada no da un error de descifrado: da una raiz que no se abre, y por
    // tanto "no hay raiz". Decirlo tal cual pareceria corrupcion.
    let volumen = Volume::open(store).await.map_err(|e| match e {
        libredisk_core::CoreError::NoRoot => {
            "hay un volumen pero no se pudo leer su raiz: lo mas probable es que la frase de \
             paso no sea la correcta"
                .to_string()
        }
        otro => otro.to_string(),
    })?;

    let resumen = resumir(&volumen, &header).map_err(|e| e.to_string())?;
    *estado.abierto.lock().await = Some(Abierto { volumen, header });
    Ok(resumen)
}

fn resumir(volumen: &Volume, header: &Header) -> Result<Resumen, libredisk_core::CoreError> {
    let layout = header.layout()?;
    Ok(Resumen {
        layout: layout.to_string(),
        proveedores: header.backends.clone(),
        tolera: layout.shard_count() - layout.shards_needed(),
        sobrecoste: layout.shard_count() as f64 / layout.shards_needed() as f64,
        reparto: header.reparto,
        generacion: volumen.root().generation,
    })
}

#[tauri::command]
async fn resumen(estado: tauri::State<'_, Estado>) -> Result<Resumen, String> {
    let guardia = estado.abierto.lock().await;
    let abierto = guardia.as_ref().ok_or("el volumen no esta abierto")?;
    resumir(&abierto.volumen, &abierto.header).map_err(|e| e.to_string())
}

#[tauri::command]
async fn cerrar(estado: tauri::State<'_, Estado>) -> Result<(), String> {
    // Soltar el volumen suelta tambien la clave derivada, que es lo que importa.
    *estado.abierto.lock().await = None;
    Ok(())
}

#[tauri::command]
async fn salud(estado: tauri::State<'_, Estado>) -> Result<Salud, String> {
    let guardia = estado.abierto.lock().await;
    let abierto = guardia.as_ref().ok_or("el volumen no esta abierto")?;

    let (trozos, ilegibles) = abierto.volumen.walk().await;
    let informe = abierto.volumen.store().scrub_many(&trozos).await;

    let mut out = Salud {
        trozos: informe.health.len(),
        ilegibles: ilegibles
            .into_iter()
            .map(|(ruta, e)| format!("{ruta}: {e}"))
            .collect(),
        sin_enumerar: informe
            .unlistable
            .iter()
            .map(|(i, e)| format!("proveedor {i}: {e}"))
            .collect(),
        ..Default::default()
    };
    let minimo = abierto
        .header
        .layout()
        .map(|l| l.shards_needed())
        .unwrap_or(1);
    (out.completos, out.degradados, out.perdidos) = clasificar(&informe.health, minimo);
    Ok(out)
}

#[tauri::command]
async fn pendiente(estado: tauri::State<'_, Estado>) -> Result<Pendiente, String> {
    let guardia = estado.abierto.lock().await;
    let abierto = guardia.as_ref().ok_or("el volumen no esta abierto")?;
    let p = abierto.volumen.store().pendiente();
    Ok(Pendiente {
        trozos: p.trozos,
        bytes: p.bytes,
        a_salvo: p.a_salvo(),
    })
}

/// Reparte los trozos en tres categorias **excluyentes**, y el orden importa.
///
/// Un trozo del que no quedan fragmentos suficientes ya no se lee. Llamarlo degradado seria
/// el peor error que puede cometer esta pantalla, porque «degradado» invita a esperar y
/// «perdido» a actuar. Por eso se mira primero si se puede leer y solo despues si esta entero.
fn clasificar(health: &[ChunkHealth], minimo: usize) -> (usize, usize, usize) {
    let (mut completos, mut degradados, mut perdidos) = (0, 0, 0);
    for h in health {
        if h.present.len() < minimo {
            perdidos += 1;
        } else if h.missing.is_empty() {
            completos += 1;
        } else {
            degradados += 1;
        }
    }
    (completos, degradados, perdidos)
}

fn main() {
    tauri::Builder::default()
        .manage(Estado::default())
        .invoke_handler(tauri::generate_handler![
            arranque, abrir, resumen, cerrar, salud, pendiente
        ])
        .run(tauri::generate_context!())
        .expect("no se pudo arrancar la ventana");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn salud(presentes: &[usize], ausentes: &[usize]) -> ChunkHealth {
        ChunkHealth {
            id: libredisk_core::ChunkId::of(b"da igual"),
            present: presentes.to_vec(),
            missing: ausentes.to_vec(),
            remaining_tolerance: 0,
        }
    }

    #[test]
    fn entero_es_completo() {
        let h = [salud(&[0, 1, 2, 3], &[])];
        assert_eq!(clasificar(&h, 2), (1, 0, 0));
    }

    #[test]
    fn le_falta_alguno_pero_se_lee_es_degradado() {
        let h = [salud(&[0, 1, 2], &[3])];
        assert_eq!(clasificar(&h, 2), (0, 1, 0));
    }

    /// El caso que no se puede confundir: sin fragmentos suficientes ya no se lee.
    #[test]
    fn sin_los_suficientes_es_perdido_y_no_degradado() {
        let h = [salud(&[0], &[1, 2, 3])];
        assert_eq!(clasificar(&h, 2), (0, 0, 1));
    }

    /// Justo en el limite se lee, asi que es degradado y no perdido.
    #[test]
    fn el_minimo_exacto_todavia_se_lee() {
        let h = [salud(&[0, 1], &[2, 3])];
        assert_eq!(clasificar(&h, 2), (0, 1, 0));
    }

    #[test]
    fn las_tres_categorias_suman_el_total() {
        let h = [
            salud(&[0, 1, 2, 3], &[]),
            salud(&[0, 1, 2], &[3]),
            salud(&[0], &[1, 2, 3]),
            salud(&[], &[0, 1, 2, 3]),
        ];
        let (c, d, p) = clasificar(&h, 2);
        assert_eq!((c, d, p), (1, 1, 2));
        assert_eq!(c + d + p, h.len());
    }
}
