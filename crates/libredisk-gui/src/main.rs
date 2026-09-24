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

use std::collections::HashMap;

use libredisk_backend::{Backend, catalogo, config, live};
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

/// Lo que se midio que ocupa el volumen en cada proveedor, con los nombres que tenian
/// cuando se midio.
///
/// Los nombres viajan con las cifras a proposito: el informe las devuelve por indice, y si
/// la configuracion cambia entre la medida y el repintado, un indice suelto se las
/// atribuiria al proveedor equivocado.
type Ocupacion = (Vec<String>, Vec<Option<u64>>);

/// Lo que se tiene abierto, si hay algo.
struct Abierto {
    volumen: Volume,
    header: Header,
}

#[derive(Default)]
struct Estado {
    abierto: Mutex<Option<Abierto>>,
    montado: Mutex<Option<Montado>>,
    /// Lo que se midio que ocupa el volumen en cada proveedor la ultima vez que se reviso,
    /// junto con los nombres que tenian entonces.
    ///
    /// Va guardado porque la cifra **cuesta una enumeracion completa** —minutos contra una
    /// nube— y no se puede pedir cada vez que se repinta una barra. Los nombres viajan con
    /// ella para no atribuirle a un proveedor lo que midio otro si la configuracion cambia
    /// entre medias.
    ocupado: Mutex<Option<Ocupacion>>,
    /// Cuentas conectadas por navegador y todavia sin guardar, por tipo de proveedor.
    ///
    /// Viven aqui y no en la pagina porque un token de refresco es acceso permanente a la
    /// cuenta. Que la pagina sea nuestra y corra en este mismo proceso no es motivo para
    /// pasearlo por ella.
    conectadas: Mutex<HashMap<String, String>>,
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
    /// Coloca cada trozo entero en un proveedor, en vez de partirlo entre todos.
    ///
    /// Cambia lo que significa la cifra de espacio: en las demas es el mas apretado por un
    /// factor --un **techo**-- y aqui es la suma de lo que declara cada uno, que con alguno
    /// mudo se queda **corta**. Lo contrario.
    suma: bool,
    /// Lo que quedaria libre en el volumen, si se pudo preguntar.
    ///
    /// Es la cifra con la que de verdad se elige: «aprovechas el 50 %» no dice si te quedan
    /// tres gigas o trescientos.
    libre: Option<u64>,
    /// Proveedores que no declararon cupo. Con alguno, `libre` es un techo y no una medida.
    sin_medir: Vec<String>,
}

/// Que topologias tienen sentido con `n` proveedores.
fn opciones(n: usize) -> Vec<Opcion> {
    let describir = |l: Layout| Opcion {
        spec: l.to_string(),
        tolera: l.tolerated_failures(),
        sobrecoste: l.storage_overhead(),
        aprovechado: l.shards_needed() as f64 / l.shard_count() as f64,
        sin_red: l.tolerated_failures() == 0,
        suma: l.coloca_por_trozo(),
        libre: None,
        sin_medir: Vec::new(),
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

    // Y al final del todo, las que no tienen redundancia ninguna. Van las ultimas porque la
    // lista se lee de mas segura a mas aprovechada, y porque nadie deberia toparselas antes
    // que las demas.
    if n > 1 {
        // `lineal` antes que `union` porque de las dos es la que mas da: con proveedores
        // desiguales suma lo que cada uno aporta, mientras que `union` se queda en n veces
        // el mas pequeno. Con proveedores iguales dan lo mismo y el orden da igual.
        out.push(describir(Layout::Lineal { n }));
        out.push(describir(Layout::Stripe { n }));
    } else {
        out.push(describir(Layout::Concat));
    }
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

/// Que topologias tienen sentido con los proveedores que hay, **y cuanto daria cada una**.
///
/// El porcentaje de aprovechamiento no basta para decidir: «aprovechas el 50 %» no dice si
/// te quedan tres gigas o trescientos. Y con proveedores desiguales la cifra sorprende, que
/// es justo cuando hay que verla antes y no despues: manda el mas apretado, asi que dos
/// cuentas de 15 GiB y 3,4 GiB dan 3,4 en espejo y 6,8 en union, no 18,4.
///
/// Cuesta **una peticion por proveedor**, una sola vez al abrir la pantalla.
#[tauri::command]
async fn topologias(cuantos: usize) -> Vec<Opcion> {
    let mut out = opciones(cuantos);

    // Si no se puede preguntar, se devuelven las opciones sin cifra en vez de fallar: elegir
    // topologia sigue siendo posible sin saber cuanto queda, y negarse a ensenar la lista
    // por no poder medir seria peor que ensenarla sin la medida.
    let Ok(backends) = proveedores() else {
        return out;
    };
    let cuotas: Vec<Result<libredisk_backend::Cuota, String>> = futures::future::join_all(
        backends
            .iter()
            .map(|b| async move { b.cuota().await.map_err(|e| e.to_string()) }),
    )
    .await;
    let nombres: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();

    for o in &mut out {
        let Ok(layout) = o.spec.parse::<Layout>() else {
            continue;
        };
        let e = libredisk_core::espacio::combinar(layout, &nombres, &cuotas);
        o.libre = e.libre;
        o.sin_medir = e.sin_respuesta;
    }
    out
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

    let pesos = pesos_para(layout, &backends).await;
    let store = header::crear(backends.clone(), layout, &frase, pesos)
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
            // Se ensena la topologia **interpretada**, no la cadena que haya guardada: un
            // volumen creado antes del cambio de nombre tiene `stripe:2` escrito, y
            // enseñarlo tal cual seria usar dos vocabularios a la vez.
            out.layout = Some(
                h.layout()
                    .map_or_else(|_| h.layout.clone(), |l| l.to_string()),
            );
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

    // La enumeracion ya se ha pagado: de ella sale tambien lo que ocupa el volumen en cada
    // proveedor, y se guarda para que las barras puedan pintarlo sin volver a enumerar.
    let nombres = abierto.volumen.store().nombres();
    *estado.ocupado.lock().await = Some((nombres, informe.ocupado));

    Ok(out)
}

/// El espacio de un proveedor, con las tres cifras separadas a proposito.
///
/// Son tres medidas distintas y mezclarlas seria mentir:
///
/// - `total` y `usado` los declara el proveedor, y `usado` es **la cuenta entera**: las
///   fotos del usuario cuentan tanto como los fragmentos. Es lo que de verdad determina
///   cuanto le queda libre.
/// - `del_volumen` es lo que ocupa LibreDisk ahi, y solo se sabe **despues de enumerar**.
///   Hasta entonces va a `null`, que la ventana pinta como "sin medir" y no como cero.
///
/// Un `fallo` no se convierte en ceros: se ensena. Un proveedor que no contesta no es un
/// proveedor vacio.
#[derive(Serialize, Default)]
struct Espacio {
    proveedor: String,
    total: Option<u64>,
    usado: Option<u64>,
    del_volumen: Option<u64>,
    /// Si este proveedor forma parte del volumen, cuando hay uno abierto para saberlo.
    ///
    /// Hace falta para no decir una tonteria: un proveedor configurado que no esta en el
    /// volumen nunca va a tener una cifra de ocupacion, y animar a pulsar «Revisar» seria
    /// mandar a enumerar durante minutos para que no cambie nada.
    en_el_volumen: Option<bool>,
    fallo: Option<String>,
}

/// Cuanto espacio hay en cada proveedor configurado.
///
/// Funciona **con el volumen cerrado**: el cupo es de la cuenta, no del volumen, y se puede
/// mirar antes de escribir la frase de paso. Lo que no se sabe sin abrir y enumerar es
/// cuanto de eso ocupa LibreDisk, y por eso ese campo llega vacio hasta que se pulsa
/// «Revisar».
///
/// Se pregunta a todos a la vez. En serie, cuatro nubes lentas son cuatro esperas seguidas
/// delante de una ventana que todavia no ha pintado nada.
#[tauri::command]
async fn espacio(estado: tauri::State<'_, Estado>) -> Result<Vec<Espacio>, String> {
    let backends = proveedores()?;
    let medido = estado.ocupado.lock().await.clone();
    // Con el volumen abierto se sabe ademas cuales de los configurados lo forman, que no
    // tienen por que ser todos.
    let del_volumen: Option<Vec<String>> = estado
        .abierto
        .lock()
        .await
        .as_ref()
        .map(|a| a.volumen.store().nombres());

    let cuotas = futures::future::join_all(backends.iter().map(|b| async move {
        let nombre = b.name().to_string();
        (nombre, b.cuota().await)
    }))
    .await;

    Ok(cuotas
        .into_iter()
        .enumerate()
        .map(|(i, (proveedor, r))| {
            // La ocupacion se guarda por indice, que es como la devuelve el informe. Si la
            // configuracion ha cambiado desde la ultima revision, sobra el dato antes que
            // arriesgarse a atribuirselo al proveedor equivocado.
            let ocupa = medido
                .as_ref()
                .filter(|(nombres, _)| nombres.get(i).is_some_and(|n| *n == proveedor))
                .and_then(|(_, bytes)| bytes.get(i).copied().flatten());
            let en_el_volumen = del_volumen.as_ref().map(|ns| ns.contains(&proveedor));
            match r {
                Ok(c) => Espacio {
                    proveedor,
                    total: c.total,
                    usado: c.usado,
                    del_volumen: ocupa,
                    en_el_volumen,
                    fallo: None,
                },
                Err(e) => Espacio {
                    proveedor,
                    del_volumen: ocupa,
                    en_el_volumen,
                    fallo: Some(e.to_string()),
                    ..Default::default()
                },
            }
        })
        .collect())
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

// ---------------------------------------------------------------------------------------
// Anadir proveedores
// ---------------------------------------------------------------------------------------

#[derive(Serialize)]
struct CampoJson {
    clave: String,
    etiqueta: String,
    ayuda: String,
    ejemplo: String,
    secreto: bool,
    obligatorio: bool,
    /// No se teclea: lo trae el boton de conectar.
    navegador: bool,
    /// Identifica a la aplicacion y no al usuario: puede venir dentro del programa.
    aplicacion: bool,
    /// Tiene un valor por defecto que sirve: no se pregunta.
    avanzado: bool,
    /// Los de un mismo grupo van todos o ninguno. La ventana lo dice antes de enviar.
    grupo: Option<String>,
}

#[derive(Serialize)]
struct TipoJson {
    clave: String,
    etiqueta: String,
    explicacion: String,
    campos: Vec<CampoJson>,
    /// `true` si esta copia de LibreDisk trae dentro las credenciales de la aplicacion.
    ///
    /// Cuando es asi, la ventana no pide el ID ni el secreto: identifican a LibreDisk y no
    /// al usuario, y pedirle a alguien el ID de una aplicacion que no es suya es pedirle un
    /// dato que no tiene.
    aplicacion_propia: bool,
    /// Si se conecta por navegador. La ventana lo usa para decir que caben varias cuentas.
    oauth: bool,
    /// Las que ya hay configuradas de este tipo, por nombre.
    ///
    /// Se ensenan al anadir otra porque es lo que hace evidente que no se pisan: quien ve
    /// «aparte de gdrive» entiende que la primera sigue donde estaba.
    nombres: Vec<String>,
    ocupadas: usize,
    /// La ranura que le tocaria si se anadiera ahora: `gdrive`, `gdrive2`...
    ///
    /// Se ensena antes de guardar porque **ese es el nombre con el que el volumen lo va a
    /// conocer para siempre**, y porque ver `gdrive2` es lo que le dice a alguien que su
    /// primera cuenta de Drive sigue ahi y no la va a pisar.
    hueco: Option<String>,
}

/// Que clases de proveedor se pueden anadir y que pide cada una.
///
/// Sale del catalogo de `libredisk-backend`, que es tambien de donde sale la ayuda de la
/// linea de ordenes y la comprobacion al guardar. Un formulario que se invente sus propios
/// campos es un formulario que acaba pidiendo algo que el motor ya no lee.
#[tauri::command]
fn tipos_de_proveedor() -> Vec<TipoJson> {
    // Los ya configurados, una vez, para no preguntar por la configuracion una vez por tipo.
    let configurados: Vec<String> = live::configurados()
        .map(|v| v.iter().map(|b| Backend::name(b).to_string()).collect())
        .unwrap_or_default();

    catalogo::TIPOS
        .iter()
        .map(|t| {
            let mios: Vec<String> = configurados
                .iter()
                .filter(|n| live::es_ranura_de(n, t.clave))
                .cloned()
                .collect();
            TipoJson {
                clave: t.clave.to_string(),
                etiqueta: t.etiqueta.to_string(),
                explicacion: t.explicacion.to_string(),
                aplicacion_propia: libredisk_backend::aplicacion::configurada(t.clave).is_some(),
                oauth: t.oauth.is_some(),
                ocupadas: mios.len(),
                nombres: mios,
                hueco: live::hueco_libre_configurado(t)
                    .ok()
                    .flatten()
                    .map(|r| live::nombre_de_ranura(&r)),
                campos: t
                    .campos
                    .iter()
                    .map(|c| CampoJson {
                        clave: c.clave.to_string(),
                        etiqueta: c.etiqueta.to_string(),
                        ayuda: c.ayuda.to_string(),
                        ejemplo: c.ejemplo.to_string(),
                        secreto: c.secreto,
                        obligatorio: c.obligatorio,
                        navegador: c.navegador,
                        aplicacion: c.aplicacion,
                        avanzado: c.avanzado,
                        grupo: c.grupo.map(str::to_string),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Abre el navegador, espera a que el usuario de permiso y **se queda el token aqui**.
///
/// Lo que devuelve es un texto para ensenar, no la credencial. El token de refresco es
/// acceso permanente a la cuenta: no tiene por que pasar por la pagina, aunque la pagina sea
/// nuestra y viva en este mismo proceso. Se queda en el estado y lo recoge `guardar_proveedor`
/// cuando toca escribirlo.
#[tauri::command]
async fn conectar_nube(
    tipo: String,
    campos: HashMap<String, String>,
    estado: tauri::State<'_, Estado>,
) -> Result<String, String> {
    let t = catalogo::tipo(&tipo).ok_or_else(|| format!("no conozco el tipo «{tipo}»"))?;
    let oauth = t
        .oauth
        .ok_or("este tipo de proveedor no se conecta por navegador")?;

    // La aplicacion que trae el programa, si la trae; lo tecleado gana, porque quien lo
    // escribe esta diciendo "usa la mia".
    let puesto = |c: &str| {
        campos
            .get(c)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let de_la_app = libredisk_backend::aplicacion::configurada(&tipo);
    let (Some(client_id), Some(client_secret)) = (
        puesto("CLIENT_ID").or_else(|| de_la_app.as_ref().map(|a| a.client_id.clone())),
        puesto("CLIENT_SECRET").or_else(|| de_la_app.as_ref().map(|a| a.client_secret.clone())),
    ) else {
        // Hacen falta antes de abrir el navegador: sin ellos el proveedor rechaza la
        // peticion, y el usuario leeria el error en una pestana y no aqui.
        return Err(
            "esta copia de LibreDisk no trae aplicacion registrada, asi que hace falta el ID \
             y el secreto de una. Son de la aplicacion, no de tu cuenta"
                .into(),
        );
    };

    let conectada = libredisk_backend::oauth::conectar(
        oauth,
        &client_id,
        &client_secret,
        &libredisk_backend::oauth::abrir_en_navegador,
    )
    .await
    .map_err(|e| e.to_string())?;

    estado
        .conectadas
        .lock()
        .await
        .insert(tipo, conectada.refresh_token);
    Ok("Cuenta conectada. Ya puedes guardar el proveedor.".into())
}

/// Valida los campos y construye el proveedor. Comun a probar y a guardar.
fn preparar(
    tipo: &str,
    campos: &HashMap<String, String>,
) -> Result<
    (
        &'static catalogo::Tipo,
        String,
        libredisk_backend::OpendalBackend,
    ),
    String,
> {
    let t = catalogo::tipo(tipo).ok_or_else(|| format!("no conozco el tipo «{tipo}»"))?;
    catalogo::validar(t, &|c| campos.get(c).cloned()).map_err(|fallos| fallos.join("; "))?;

    let ranura = live::hueco_libre_configurado(t)
        .map_err(|e| e.to_string())?
        .ok_or("no quedan ranuras libres de este tipo")?;
    let backend =
        live::construir_uno(t, &ranura, &|c| campos.get(c).cloned()).map_err(|e| e.to_string())?;
    Ok((t, ranura, backend))
}

/// Junta lo tecleado con lo que trajo el navegador.
///
/// Los campos marcados como `navegador` en el catalogo no vienen de la pagina —un token de
/// refresco no se teclea ni se pasea por ella— sino del estado, donde los dejo
/// [`conectar_nube`].
async fn con_lo_conectado(
    tipo: &str,
    mut campos: HashMap<String, String>,
    estado: &tauri::State<'_, Estado>,
) -> HashMap<String, String> {
    if let Some(token) = estado.conectadas.lock().await.get(tipo) {
        campos.insert("REFRESH_TOKEN".into(), token.clone());
    }
    campos
}

#[derive(Serialize)]
struct Guardado {
    nombre: String,
    fichero: String,
    /// `true` si ya hay un volumen creado. Entonces esto **no** lo mete en el volumen.
    hay_volumen: bool,
}

/// Prueba el proveedor y, solo si responde, lo escribe en la configuracion del usuario.
///
/// Lo que **no** hace: meterlo en un volumen que ya exista. Son dos cosas distintas y la
/// segunda cuesta ordenes de magnitud mas —hay que mover fragmentos, o reescribir el volumen
/// entero segun para que se quiera— asi que la elige el usuario con `providers add`. Decir
/// aqui que ya esta dentro seria la mentira mas cara de esta pantalla.
#[tauri::command]
async fn guardar_proveedor(
    tipo: String,
    campos: HashMap<String, String>,
    estado: tauri::State<'_, Estado>,
) -> Result<Guardado, String> {
    let campos = con_lo_conectado(&tipo, campos, &estado).await;
    let (t, ranura, backend) = preparar(&tipo, &campos)?;
    libredisk_backend::conformance::prueba_rapida(&backend)
        .await
        .map_err(|e| format!("no se guarda nada porque el proveedor no responde: {e}"))?;

    // Y que no sea una cuenta que ya esta. Dos proveedores sobre la misma cuenta se ven como
    // dos, cuentan como dos al elegir topologia y se anuncian como «aguanta perder uno»;
    // pero el dia que se pierda esa cuenta se pierden las dos copias a la vez.
    if let Ok(Some(otro)) = libredisk_backend::live::ya_configurada(t, &backend).await {
        return Err(format!(
            "esa cuenta ya esta configurada como «{otro}». Dos proveedores sobre la misma \
             cuenta no son redundancia: se verian como dos, pero el dia que se pierda la \
             cuenta se pierden los dos a la vez. Conecta una cuenta distinta."
        ));
    }

    let fichero = config::fichero_de_usuario()
        .ok_or("no se sabe donde vive la configuracion de este usuario")?;
    let claves = live::claves_de(t, &ranura, &|c| campos.get(c).cloned());
    config::guardar(&fichero, &claves).map_err(|e| e.to_string())?;

    // Guardada ya en el fichero, la copia en memoria sobra: cuanto menos tiempo viva un
    // token de refresco en el proceso, mejor.
    estado.conectadas.lock().await.remove(&tipo);

    // El cerrojo se suelta **antes** de preguntar por la cabecera, que es red y puede tardar
    // segundos. Sostener un cerrojo a traves de un `await` es como nacen los bloqueos que
    // solo aparecen cuando dos cosas coinciden.
    let abierto_ahora = estado.abierto.lock().await.is_some();

    Ok(Guardado {
        nombre: live::nombre_de_ranura(&ranura),
        fichero: fichero.display().to_string(),
        hay_volumen: abierto_ahora || hay_volumen().await,
    })
}

/// Si existe una cabecera en los proveedores configurados.
async fn hay_volumen() -> bool {
    let Ok(backends) = proveedores() else {
        return false;
    };
    matches!(header::read(&backends).await, Ok(Some(_)))
}

// ---------------------------------------------------------------------------------------
// Retirar el volumen
// ---------------------------------------------------------------------------------------

/// Lo que se va a borrar, para poder mirarlo antes de decir que si.
#[derive(Serialize)]
struct ParaBorrar {
    layout: String,
    proveedores: Vec<String>,
    fragmentos: usize,
    bytes: u64,
    /// Lo que hay que escribir para confirmar.
    ///
    /// Se pide escribir los nombres de los proveedores y no un «si»: obliga a mirar la lista
    /// de lo que se va a borrar. Un boton que solo hay que pulsar se pulsa sin leer.
    confirmacion: String,
}

/// Cuenta lo que hay, sin tocar nada.
///
/// Cuesta una enumeracion por proveedor —contra una nube, puede tardar— y por eso se pide
/// aparte: la pantalla no puede ir a preguntarlo cada vez que se repinta.
#[tauri::command]
async fn volumen_a_borrar() -> Result<ParaBorrar, String> {
    let backends = proveedores()?;
    let header = header::read(&backends)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no hay ningun volumen en estos proveedores")?;

    let inventario = libredisk_core::retirar::inventariar(&backends)
        .await
        .map_err(|e| {
            format!("{e}\nNo se puede borrar sin poder mirarlo entero: el recuento seria falso")
        })?;

    let nombres: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();
    Ok(ParaBorrar {
        layout: header
            .layout()
            .map_or_else(|_| header.layout.clone(), |l| l.to_string()),
        confirmacion: nombres.join(","),
        proveedores: nombres,
        fragmentos: inventario.fragmentos(),
        bytes: inventario.bytes(),
    })
}

/// Retira el volumen. **No se puede deshacer.**
#[tauri::command]
async fn borrar_volumen(
    confirmacion: String,
    estado: tauri::State<'_, Estado>,
) -> Result<String, String> {
    let backends = proveedores()?;
    let esperado = backends
        .iter()
        .map(|b| b.name().to_string())
        .collect::<Vec<_>>()
        .join(",");
    if confirmacion.trim() != esperado {
        return Err(format!(
            "para confirmar hay que escribir exactamente: {esperado}"
        ));
    }

    // Con algo montado, borrar dejaria una unidad sirviendo un volumen que ya no existe. Se
    // desmonta y se cierra antes, no despues.
    *estado.montado.lock().await = None;
    *estado.abierto.lock().await = None;
    *estado.ocupado.lock().await = None;

    let hecho = libredisk_core::retirar::retirar(&backends)
        .await
        .map_err(|e| e.to_string())?;

    Ok(format!(
        "Retirado: {} fragmento(s) en {} proveedor(es).\n\
         La carpeta sigue existiendo en cada uno, vacía de lo que era del volumen. Si \
         guardaste ahí otras cosas, siguen donde estaban.",
        hecho.fragmentos(),
        hecho.por_proveedor.len()
    ))
}

/// Cuanto aporta cada proveedor, para las topologias que colocan por trozo.
///
/// Solo `lineal` los usa; para el resto se devuelve vacio y nadie los mira.
///
/// Salen de lo que cada proveedor declara **libre** ahora mismo, y se quedan fijos en la
/// cabecera. Eso es deliberado: si se recalcularan en cada arranque, un trozo escrito ayer se
/// buscaria hoy en otro proveedor y el volumen se desmontaria solo. Cuando las cuentas cambien
/// de tamano habra que reescribir el reparto a proposito, no que cambie por su cuenta.
///
/// En gigabytes y no en bytes porque lo que importa es la proporcion, y numeros mas pequenos
/// hacen la cabecera legible a ojo. Al que no declare cupo se le da el peso del mas pequeno
/// que si lo declare: dejarlo en cero lo sacaria del volumen, y darle mucho le mandaria
/// trozos que quiza no caben.
async fn pesos_para(layout: Layout, backends: &[Arc<dyn Backend>]) -> Vec<u64> {
    if !layout.coloca_por_trozo() {
        return Vec::new();
    }
    const GIB: u64 = 1 << 30;

    let cuotas = futures::future::join_all(backends.iter().map(|b| b.cuota())).await;
    let libres: Vec<Option<u64>> = cuotas
        .iter()
        .map(|c| c.as_ref().ok().and_then(|c| c.libre()))
        .collect();

    let minimo = libres.iter().flatten().min().copied().unwrap_or(GIB);
    libres
        .iter()
        .map(|l| (l.unwrap_or(minimo) / GIB).max(1))
        .collect()
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
            arranque,
            abrir,
            resumen,
            cerrar,
            salud,
            espacio,
            pendiente,
            topologias,
            crear,
            montaje,
            montar,
            desmontar,
            volumen_a_borrar,
            borrar_volumen,
            tipos_de_proveedor,
            conectar_nube,
            guardar_proveedor
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
    ///
    /// La comparacion es por **lo que el usuario lee al elegir**, no por la forma interna:
    /// cuantas caidas aguanta, cuanto ocupa de mas, y si coloca cada trozo entero en un
    /// proveedor. Esa tercera parte hizo falta al llegar `lineal`: mirando solo fragmentos
    /// necesarios y totales, `lineal:2` y `espejo:2` parecen la misma cosa --uno de dos-- y
    /// no lo son en absoluto, porque el espejo aguanta una caida y lineal ninguna.
    #[test]
    fn ninguna_opcion_se_repite_con_otro_nombre() {
        for n in 1..=8 {
            let mut vistas: Vec<(usize, u64, bool)> = Vec::new();
            for o in opciones(n) {
                let l: Layout = o.spec.parse().unwrap();
                let clave = (
                    l.tolerated_failures(),
                    l.storage_overhead().to_bits(),
                    l.coloca_por_trozo(),
                );
                assert!(
                    !vistas.contains(&clave),
                    "con {n} proveedores, '{}' promete lo mismo que otra de la lista",
                    o.spec
                );
                vistas.push(clave);
            }
        }
    }

    /// Las que no aguantan nada van **al final** y van marcadas, todas ellas.
    ///
    /// Son dos desde que existe `lineal`, y las dos tienen que llevar el aviso: lo que hace
    /// falta no es que haya una sino que ninguna se cuele entre las que si aguantan.
    #[test]
    fn las_que_no_aguantan_nada_van_al_final_y_van_marcadas() {
        for n in 2..=8 {
            let ops = opciones(n);
            let sin_red: Vec<usize> = ops
                .iter()
                .enumerate()
                .filter(|(_, o)| o.sin_red)
                .map(|(i, _)| i)
                .collect();

            assert!(!sin_red.is_empty(), "con {n} proveedores no hay ninguna");
            // Las ultimas posiciones de la lista, sin huecos: ninguna colada en medio.
            let primera = ops.len() - sin_red.len();
            assert_eq!(
                sin_red,
                (primera..ops.len()).collect::<Vec<_>>(),
                "con {n} proveedores hay una sin redundancia en medio de la lista"
            );
            for i in &sin_red {
                assert_eq!(ops[*i].tolera, 0);
                assert_eq!(
                    ops[*i].sobrecoste, 1.0,
                    "no ocupa de mas: es su razon de ser"
                );
            }
        }
    }

    /// Con dos proveedores: espejo, y las dos formas de no tener redundancia.
    ///
    /// No cabe la paridad: `rs:1,1` seria el mismo espejo con otro nombre.
    #[test]
    fn con_dos_proveedores_hay_tres_opciones() {
        let specs: Vec<String> = opciones(2).into_iter().map(|o| o.spec).collect();
        assert_eq!(specs, vec!["espejo:2", "lineal:2", "union:2"]);
    }

    /// Con tres entra ademas la paridad.
    #[test]
    fn con_tres_proveedores_entra_la_paridad() {
        let specs: Vec<String> = opciones(3).into_iter().map(|o| o.spec).collect();
        assert_eq!(specs, vec!["espejo:3", "rs:2,1", "lineal:3", "union:3"]);
    }

    /// Con uno solo no hay nada que repartir.
    #[test]
    fn con_un_solo_proveedor_solo_cabe_concat() {
        let specs: Vec<String> = opciones(1).into_iter().map(|o| o.spec).collect();
        assert_eq!(specs, vec!["concat"]);
    }

    /// La lista va de mas segura a mas aprovechada.
    ///
    /// **No estrictamente**: `lineal` y `union` aguantan las dos cero, asi que van seguidas.
    /// Lo que no puede pasar es que una que aguanta menos vaya antes que una que aguanta
    /// mas, porque el orden es lo que hace que el valor por defecto sea el prudente.
    #[test]
    fn la_lista_va_de_mas_segura_a_mas_aprovechada() {
        for n in 2..=8 {
            let ops = opciones(n);
            for par in ops.windows(2) {
                assert!(
                    par[0].tolera >= par[1].tolera,
                    "con {n}: '{}' va antes que '{}' y aguanta menos",
                    par[0].spec,
                    par[1].spec
                );
            }
        }
    }

    /// Y entre las dos sin redundancia, **`lineal` va primera**: de las dos es la que mas da.
    ///
    /// Con proveedores desiguales suma lo que cada uno aporta, mientras que `union` se queda
    /// en n veces el mas pequeno. Quien llegue hasta el final de la lista deberia encontrarse
    /// antes la mejor de las dos.
    #[test]
    fn entre_las_dos_sin_red_lineal_va_antes_que_union() {
        for n in 2..=8 {
            let specs: Vec<String> = opciones(n).into_iter().map(|o| o.spec).collect();
            let lineal = specs.iter().position(|s| s.starts_with("lineal")).unwrap();
            let union = specs.iter().position(|s| s.starts_with("union")).unwrap();
            assert!(lineal < union, "con {n} proveedores: {specs:?}");
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
