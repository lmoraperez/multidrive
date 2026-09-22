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
use libredisk_core::sincronizador::Sincronizador;
use libredisk_core::tree::Volume;
use libredisk_core::writeback::{self, WriteQueue};
use libredisk_raid::Layout;
use serde::Serialize;
use tokio::sync::Mutex;

/// Cada cuantos segundos sube lo pendiente mientras esta montado en escritura.
const SYNC_CADA: u64 = 30;

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
    montado: Mutex<Option<Montado>>,
    /// Lo ultimo que dijo el sincronizador, para poder ensenarlo.
    ultimo_sync: Arc<std::sync::Mutex<Option<String>>>,
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

/// Abre el almacen **con la cola de escritura diferida**.
///
/// La cola no es opcional aqui aunque la ventana no escriba por su cuenta: sin ella,
/// `pendiente` diria "nada pendiente" aunque la linea de ordenes tenga la cola llena, que es
/// la mentira mas cara que puede decir un volumen con escritura diferida. Y es lo que permite
/// montar en lectura y escritura sin volver a pedir la frase.
async fn abrir_almacen(
    header: &Header,
    backends: Vec<Arc<dyn Backend>>,
    frase: &str,
) -> Result<libredisk_core::ChunkStore, String> {
    let store = header
        .almacen(backends, frase, CACHE)
        .map_err(|e| e.to_string())?;
    let dir = writeback::dir_de_volumen(writeback::dir_base(), &header.salt);
    let cola = WriteQueue::open(&dir).await.map_err(|e| {
        format!(
            "no se pudo abrir la cola de subida en '{}': {e}",
            dir.display()
        )
    })?;
    Ok(store.with_writeback(Arc::new(cola)))
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

/// Una topologia posible para los proveedores que hay, con su precio.
///
/// Se calculan aqui y no en la ventana porque el numero que importa --cuantas caidas
/// aguanta-- sale de las mismas reglas que luego aplica el motor. Dos sitios que lo calculen
/// por su cuenta acaban discrepando, y el que discrepa es el que el usuario cree.
#[derive(Serialize)]
struct Opcion {
    spec: String,
    tolera: usize,
    sobrecoste: f64,
    /// Fraccion del espacio contratado que queda utilizable.
    aprovechado: f64,
    /// No aguanta ninguna caida. La ventana lo pinta en rojo y lo dice con todas las letras:
    /// elegir esto sin enterarse es perder el volumen entero el dia que caiga un proveedor.
    sin_red: bool,
}

/// Que topologias tienen sentido con `n` proveedores.
fn opciones(n: usize) -> Vec<Opcion> {
    let describir = |l: Layout| Opcion {
        spec: l.to_string(),
        tolera: l.tolerated_failures(),
        sobrecoste: l.storage_overhead(),
        aprovechado: l.shards_needed() as f64 / l.shard_count() as f64,
        sin_red: l.tolerated_failures() == 0,
    };

    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    // Espejo: una copia entera en cada proveedor. Aguanta todo menos uno, y desperdicia
    // tanto como aguanta.
    if n > 1 {
        out.push(describir(Layout::Mirror { n }));
    }
    // Reed-Solomon, de mas segura a mas aprovechada.
    //
    // `k = 1` se queda fuera porque **es un espejo**, no algo parecido: con un solo fragmento
    // de datos, cada fragmento de paridad acaba siendo una copia entera. Mismo margen, mismo
    // sobrecoste y misma capacidad que `mirror`, pero calculado con aritmetica de cuerpo
    // finito y sin la propiedad util del espejo —que cualquier copia suelta es el dato—.
    // Ofrecer las dos es ofrecer la misma opcion dos veces con nombres distintos.
    for m in (1..n).rev() {
        let k = n - m;
        if k < 2 {
            continue;
        }
        out.push(describir(Layout::ReedSolomon { k, m }));
    }

    // Y al final del todo, sin redundancia ninguna: toda la capacidad sumada a cambio de que
    // perder un proveedor pierda el volumen entero. Va la ultima porque la lista se lee de mas
    // segura a mas aprovechada, y porque nadie deberia toparsela antes que las demas.
    out.push(describir(if n > 1 {
        Layout::Stripe { n }
    } else {
        Layout::Concat
    }));
    out
}

/// Lo que hay montado, si hay algo.
///
/// `Montaje` desmonta al soltarlo, asi que tenerlo guardado aqui **es** lo que mantiene la
/// unidad viva. El sincronizador va al lado por lo mismo.
struct Montado {
    punto: String,
    escritura: bool,
    _montaje: libredisk_mount::Montaje,
    _sincronizador: Option<Sincronizador>,
}

#[derive(Serialize, Default)]
struct EstadoMontaje {
    montado: bool,
    punto: Option<String>,
    escritura: bool,
    /// Que hay que escribir en el campo del punto de montaje en este sistema.
    ejemplo: String,
}

fn ejemplo_de_punto() -> String {
    if cfg!(windows) {
        "Z:".into()
    } else {
        "/mnt/libredisk".into()
    }
}

#[tauri::command]
async fn montaje(estado: tauri::State<'_, Estado>) -> Result<EstadoMontaje, String> {
    let guardia = estado.montado.lock().await;
    Ok(match guardia.as_ref() {
        Some(m) => EstadoMontaje {
            montado: true,
            punto: Some(m.punto.clone()),
            escritura: m.escritura,
            ejemplo: ejemplo_de_punto(),
        },
        None => EstadoMontaje {
            ejemplo: ejemplo_de_punto(),
            ..Default::default()
        },
    })
}

#[tauri::command]
async fn montar(
    punto: String,
    escritura: bool,
    estado: tauri::State<'_, Estado>,
) -> Result<EstadoMontaje, String> {
    if estado.montado.lock().await.is_some() {
        return Err("ya hay un montaje; desmontalo antes".into());
    }
    let guardia = estado.abierto.lock().await;
    let abierto = guardia.as_ref().ok_or("el volumen no esta abierto")?;

    // El montaje trabaja sobre una copia del volumen. No es un rodeo: la ventana sigue
    // consultando salud y pendientes mientras tanto, y compartir un cerrojo entre algo que
    // puede tardar minutos y una pantalla que se refresca seria bloquear la segunda. La copia
    // comparte almacen, caches y cola, asi que ve exactamente lo mismo.
    let store = abierto.volumen.store().clone();
    let compartido = Arc::new(std::sync::RwLock::new(Volume::at(
        store.clone(),
        abierto.volumen.root().clone(),
    )));
    drop(guardia);

    let montaje = libredisk_mount::montar(compartido, &punto, !escritura).map_err(|e| {
        // Es lo primero que ve alguien sin WinFsp instalado, o que apunta a una letra
        // ocupada: el error merece llegar entero y no resumido.
        format!("{e:#}")
    })?;

    // En escritura, alguien tiene que subir lo que se escriba. Los avisos no van a ninguna
    // consola —una ventana no tiene— asi que se guardan para ensenarlos en el panel.
    let sincronizador = escritura.then(|| {
        let ultimo = Arc::clone(&estado.ultimo_sync);
        Sincronizador::arrancar(
            store,
            std::time::Duration::from_secs(SYNC_CADA),
            Box::new(move |r| {
                use libredisk_core::sincronizador::Resultado;
                let texto = match r {
                    Resultado::Hecho(i) => format!(
                        "{} subidos, {} descartados{}",
                        i.subidos,
                        i.descartados,
                        if i.raiz_publicada {
                            ", raiz publicada"
                        } else {
                            ""
                        }
                    ),
                    Resultado::Fallo(e) => format!("fallo, se reintentara: {e}"),
                };
                *ultimo.lock().unwrap_or_else(|p| p.into_inner()) = Some(texto);
            }),
        )
    });

    *estado.montado.lock().await = Some(Montado {
        punto: punto.clone(),
        escritura,
        _montaje: montaje,
        _sincronizador: sincronizador,
    });
    Ok(EstadoMontaje {
        montado: true,
        punto: Some(punto),
        escritura,
        ejemplo: ejemplo_de_punto(),
    })
}

#[tauri::command]
async fn desmontar(estado: tauri::State<'_, Estado>) -> Result<EstadoMontaje, String> {
    // Soltarlo es desmontar: el `Drop` de `Montaje` lo hace.
    *estado.montado.lock().await = None;
    Ok(EstadoMontaje {
        ejemplo: ejemplo_de_punto(),
        ..Default::default()
    })
}

#[tauri::command]
fn topologias(proveedores: usize) -> Vec<Opcion> {
    opciones(proveedores)
}

#[tauri::command]
async fn crear(
    spec: String,
    frase: String,
    estado: tauri::State<'_, Estado>,
) -> Result<Resumen, String> {
    let backends = proveedores()?;
    let layout: Layout = spec
        .parse()
        .map_err(|_| format!("topologia invalida: {spec}"))?;

    let store = header::crear(backends.clone(), layout, &frase)
        .await
        .map_err(|e| match e {
            libredisk_core::CoreError::YaHayVolumen => {
                "ya hay un volumen en estos proveedores. Crear otro encima generaria una sal \
                 nueva y dejaria inalcanzable todo lo anterior, asi que no se hace."
                    .to_string()
            }
            otro => otro.to_string(),
        })?;

    let (volumen, degradados) = Volume::create(store).await.map_err(|e| e.to_string())?;
    if !degradados.is_empty() {
        // Un volumen recien creado al que ya le falta un proveedor no es un volumen sano, y
        // callarlo aqui seria empezar mintiendo sobre la redundancia.
        return Err(format!(
            "el volumen se creo pero estos proveedores no aceptaron la escritura: {}. \
             Revisa con `libredisk scrub`.",
            degradados.join(", ")
        ));
    }

    let header = header::read(&backends)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("la cabecera no se pudo releer tras crearla")?;
    let resumen = resumir(&volumen, &header).map_err(|e| e.to_string())?;
    *estado.abierto.lock().await = Some(Abierto { volumen, header });
    Ok(resumen)
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

    let store = abrir_almacen(&header, backends, &frase).await?;

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
            arranque, abrir, resumen, cerrar, salud, pendiente, topologias, crear, montaje, montar,
            desmontar
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

    /// Dos opciones que aguantan lo mismo y cuestan lo mismo **son la misma opcion**, aunque
    /// se llamen distinto. Con tres proveedores pasaba: salian `mirror:3` y `rs:1,2`.
    ///
    /// Se comprueba por la propiedad y no por los nombres, para que cace tambien el duplicado
    /// que aparezca al anadir una topologia nueva.
    #[test]
    fn ninguna_opcion_se_repite_con_otro_nombre() {
        for n in 1..=8 {
            let mut vistas: Vec<(usize, usize)> = Vec::new();
            for o in opciones(n) {
                let l: Layout = o.spec.parse().unwrap();
                let clave = (l.shard_count(), l.shards_needed());
                assert!(
                    !vistas.contains(&clave),
                    "con {n} proveedores, '{}' aguanta y cuesta lo mismo que otra de la lista",
                    o.spec
                );
                vistas.push(clave);
            }
        }
    }

    /// Con tres, tres opciones de verdad: espejo, paridad y reparto puro.
    #[test]
    fn con_tres_proveedores_hay_tres_opciones() {
        let specs: Vec<String> = opciones(3).into_iter().map(|o| o.spec).collect();
        assert_eq!(specs, vec!["mirror:3", "rs:2,1", "stripe:3"]);
    }

    /// Con dos no cabe la paridad: `rs:1,1` seria el mismo espejo con otro nombre.
    #[test]
    fn con_dos_proveedores_solo_espejo_y_reparto() {
        let specs: Vec<String> = opciones(2).into_iter().map(|o| o.spec).collect();
        assert_eq!(specs, vec!["mirror:2", "stripe:2"]);
    }

    /// La sin redundancia va **la ultima**, y es la unica marcada como tal.
    ///
    /// El orden no es estetico: la primera es el valor por defecto, y quien no sepa elegir se
    /// queda con ella. La que puede perderlo todo tiene que estar lo mas lejos posible de ahi.
    #[test]
    fn la_que_no_aguanta_nada_va_la_ultima_y_va_marcada() {
        for n in 2..=8 {
            let ops = opciones(n);
            let marcadas: Vec<&Opcion> = ops.iter().filter(|o| o.sin_red).collect();
            assert_eq!(marcadas.len(), 1, "con {n} proveedores");
            assert_eq!(
                marcadas[0].spec,
                ops.last().unwrap().spec,
                "con {n} proveedores, la que no aguanta nada no es la ultima"
            );
            assert_eq!(marcadas[0].tolera, 0);
            assert_eq!(
                marcadas[0].sobrecoste, 1.0,
                "no ocupa de mas: es su razon de ser"
            );
        }
    }

    /// Y la lista va de mas segura a mas aprovechada, de principio a fin.
    #[test]
    fn la_lista_va_de_mas_segura_a_mas_aprovechada() {
        for n in 2..=8 {
            let ops = opciones(n);
            for par in ops.windows(2) {
                assert!(
                    par[0].tolera > par[1].tolera,
                    "con {n}: '{}' va antes que '{}' y no aguanta mas",
                    par[0].spec,
                    par[1].spec
                );
            }
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
