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

use multidrive_backend::{Backend, catalogo, config, live};
use multidrive_core::header::{self, Header};
use multidrive_core::maleta;
use multidrive_core::pesos::{pesos_ampliados, pesos_para};
use multidrive_core::rebalance::{self, Cambio};
use multidrive_core::sincronizador::Sincronizador;
use multidrive_core::topologias::Opcion;
use multidrive_core::tree::Volume;
use multidrive_core::writeback::{self, WriteQueue};
use multidrive_core::{ChunkHealth, RutaPendiente};
use multidrive_raid::Layout;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WindowEvent};
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
    /// Quien mas estaba escribiendo cuando se monto, si habia alguien.
    ///
    /// No impide nada --escribir se reaplica si otro se adelanta-- pero hay que decirlo: si dos
    /// tocan el mismo fichero, uno vera un error, y conviene que no sea una sorpresa.
    otro_escribiendo: Arc<std::sync::Mutex<Option<String>>>,
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
    /// Que paso con la maleta al crear el volumen. Vacio en los demas casos, que no la tocan.
    #[serde(skip_serializing_if = "Option::is_none")]
    maleta: Option<String>,
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
    /// Que ficheros concretos son los que no han salido de esta maquina.
    ///
    /// Un total de trozos no responde a lo que se pregunta quien va a apagar el equipo, y
    /// encima mezcla datos con nodos del arbol: un fichero de un byte deja tres trozos.
    rutas: Vec<RutaPendiente>,
    /// `true` si la raiz esta sin publicar: desde otro equipo todavia no se ve nada.
    raiz_sin_publicar: bool,
    /// Quien mas esta escribiendo en el volumen, si hay alguien.
    otro_escribiendo: Option<String>,
    /// `true` si se perdio el testigo de escritura mientras esto estaba montado.
    ///
    /// Pasa cuando esta maquina se queda sin red mas de lo que dura el testigo y otra lo coge
    /// legitimamente. A partir de ahi, lo que se escriba aqui puede chocar con lo de alla, y
    /// eso hay que decirlo: es la diferencia entre un aviso y una perdida silenciosa.
    testigo_perdido: bool,
    /// Lo ultimo que hizo el sincronizador de fondo, si dijo algo.
    ///
    /// Se guardaba y **no lo leia nadie**: el hilo que sube en segundo plano dejaba aqui lo
    /// que iba haciendo y ese texto no llegaba a la pantalla. Un sincronizador que falla en
    /// silencio es justo el que no hay que tener.
    ultimo_sync: Option<String>,
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
) -> Result<multidrive_core::ChunkStore, String> {
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

/// Lo que hay montado, si hay algo.
///
/// `Montaje` desmonta al soltarlo, asi que tenerlo guardado aqui **es** lo que mantiene la
/// unidad viva. El sincronizador va al lado por lo mismo.
struct Montado {
    punto: String,
    escritura: bool,
    _montaje: multidrive_mount::Montaje,
    _sincronizador: Option<Sincronizador>,
    /// Mira si otro equipo publico algo y se pasa a ello. Sin esto, una unidad montada ensena
    /// el volumen de cuando se monto, y lo que suba otro no aparece nunca.
    _refrescador: multidrive_core::refrescador::Refrescador,
    /// El testigo de escritura. Se suelta solo al desmontar, porque desmontar suelta esto.
    testigo: Option<multidrive_core::testigo::Guardia>,
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
        "/mnt/multidrive".into()
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
    app: tauri::AppHandle,
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

    // El testigo, **antes de montar**. Montar y descubrir despues que otro esta escribiendo
    // dejaria una unidad puesta que no sirve, y quitarla es mas molesto que no haberla puesto.
    // En solo lectura no se coge: leer no molesta a nadie.
    // Donde se deja lo que cuente el refrescador, para que el panel lo pueda ensenar.
    let nuevo = Arc::clone(&estado.ultimo_sync);

    let (testigo, otro_escribiendo) = if escritura {
        // No se niega: escribir se reaplica si otro se adelanta, asi que dos equipos en
        // ficheros distintos conviven. Lo que hace falta es saberlo, y se dice en el panel.
        multidrive_core::testigo::Guardia::coger_o_compartir(
            store.backends().to_vec(),
            "la ventana, montada en escritura",
        )
        .await
    } else {
        (None, None)
    };
    *estado
        .otro_escribiendo
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = otro_escribiendo.map(|t| t.que);

    let montaje =
        multidrive_mount::montar(Arc::clone(&compartido), &punto, !escritura).map_err(|e| {
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
                use multidrive_core::sincronizador::Resultado;
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

    // Va **siempre**, no solo en escritura: una unidad en solo lectura es justo la que mas
    // tiempo pasa abierta, y es la que mas necesita enterarse de lo que sube otro.
    let refrescador = multidrive_core::refrescador::Refrescador::arrancar(
        compartido,
        std::time::Duration::from_secs(SYNC_CADA),
        Box::new(move |r| {
            let g = r.generation;
            *nuevo.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(format!("otro equipo publico algo (generacion {g})"));
        }),
    );

    *estado.montado.lock().await = Some(Montado {
        punto: punto.clone(),
        escritura,
        _montaje: montaje,
        _sincronizador: sincronizador,
        _refrescador: refrescador,
        testigo,
    });

    // Con la ventana cerrada, el rotulo de la bandeja es lo unico que queda diciendo que hay
    // una unidad montada y donde.
    rotulo_de_bandeja(
        &app,
        &format!(
            "MultiDrive — montado en {punto}{}",
            if escritura { "" } else { " (solo lectura)" }
        ),
    );

    Ok(EstadoMontaje {
        montado: true,
        punto: Some(punto),
        escritura,
        ejemplo: ejemplo_de_punto(),
    })
}

#[tauri::command]
async fn desmontar(
    app: tauri::AppHandle,
    estado: tauri::State<'_, Estado>,
) -> Result<EstadoMontaje, String> {
    // Soltarlo es desmontar: el `Drop` de `Montaje` lo hace. Va **primero**, para que no
    // entren escrituras nuevas mientras se sube lo que ya hay.
    //
    // Y con el se va el sincronizador de fondo, que vive dentro: a partir de aqui no sube
    // nadie solo. Por eso la ultima subida se hace aqui y no se deja para luego.
    *estado.montado.lock().await = None;
    rotulo_de_bandeja(&app, "MultiDrive");

    let guardia = estado.abierto.lock().await;
    if let Some(abierto) = guardia.as_ref() {
        // Si falla no se dice aqui: lo que quede sin subir lo cuenta `pendiente`, con los
        // nombres de los ficheros, que es mas util que el motivo tecnico de una subida.
        let _ = abierto.volumen.store().sync().await;
    }

    Ok(EstadoMontaje {
        ejemplo: ejemplo_de_punto(),
        ..Default::default()
    })
}

/// Que topologias tienen sentido con los proveedores que hay, **y cuanto daria cada una**.
///
/// La lista y las cifras las pone el nucleo. Estuvieron aqui dentro, y se subieron cuando el
/// telefono tuvo que ofrecer lo mismo: dos copias de estas reglas empiezan iguales y acaban
/// diciendo cosas distintas sobre lo mismo, y la que discrepa es la que alguien cree.
#[tauri::command]
async fn topologias(cuantos: usize) -> Vec<Opcion> {
    // Si no se puede preguntar, se devuelven las opciones sin cifra en vez de fallar: elegir
    // topologia sigue siendo posible sin saber cuanto queda, y negarse a ensenar la lista
    // por no poder medir seria peor que ensenarla sin la medida.
    match proveedores() {
        Ok(backends) => multidrive_core::topologias::medir(&backends).await,
        Err(_) => multidrive_core::topologias::opciones(cuantos),
    }
}

/// Guarda la maleta en las nubes del volumen, y dice donde quedo.
///
/// Se hace **al crear y en cada apertura**, sin preguntar. Son los dos momentos en que la
/// frase de paso esta en la mano, y abrir tiene ademas una propiedad que lo hace seguro: para
/// abrir un volumen hay que tener configurados **todos** sus proveedores, asi que lo que se
/// guarda es siempre al menos lo que el volumen necesita. Sin esto, la maleta se quedaria con
/// los proveedores que habia el dia que se creo, y quien anadiera una cuenta despues se
/// encontraria en el otro equipo con una configuracion incompleta que no abre nada.
///
/// Por eso no hay ningun boton de «guardar»: no hace falta, y un boton que no hace falta es
/// un boton que alguien pulsa sin saber para que.
///
/// Devuelve `Err` solo con el texto: **no puede tumbar ni la creacion ni la apertura**. El
/// volumen ya existe y ya esta abierto cuando esto corre; perderlo porque no se pudo subir un
/// fichero de configuracion seria cambiar un incordio por un desastre.
async fn guardar_maleta(backends: &[Arc<dyn Backend>], frase: &str) -> Result<String, String> {
    let claves = maleta::claves_de_este_equipo().map_err(|e| e.to_string())?;
    let bulto = maleta::empaquetar(&claves, frase).map_err(|e| e.to_string())?;

    let resultados = maleta::guardar(backends, &bulto).await;
    let (bien, mal): (Vec<_>, Vec<_>) = resultados.iter().partition(|(_, f)| f.is_none());
    if bien.is_empty() {
        return Err(mal
            .iter()
            .filter_map(|(n, f)| f.as_ref().map(|e| format!("{n}: {e}")))
            .collect::<Vec<_>>()
            .join("; "));
    }
    Ok(bien
        .iter()
        .map(|(n, _)| n.clone())
        .collect::<Vec<_>>()
        .join(", "))
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
            multidrive_core::CoreError::YaHayVolumen => {
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
             Revisa con `multidrive scrub`.",
            degradados.join(", ")
        ));
    }

    let header = header::read(&backends)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("la cabecera no se pudo releer tras crearla")?;
    let mut resumen = resumir(&volumen, &header).map_err(|e| e.to_string())?;

    resumen.maleta = Some(match guardar_maleta(&backends, &frase).await {
        Ok(donde) => format!(
            "Tu configuracion queda guardada, cifrada con tu frase de paso, en {donde}. \
             En otro equipo: enlaza una de estas cuentas y pulsa «Traer a este equipo»."
        ),
        // El volumen ya esta creado cuando esto corre. Es un aviso, no un fallo.
        Err(e) => format!(
            "El volumen esta creado, pero no se pudo guardar tu configuracion en las nubes: \
             {e}. Puedes hacerlo cuando quieras desde «Llevarme esta configuracion a otro \
             equipo»."
        ),
    });

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

    let store = abrir_almacen(&header, backends.clone(), &frase).await?;

    // Una frase equivocada no da un error de descifrado: da una raiz que no se abre, y por
    // tanto "no hay raiz". Decirlo tal cual pareceria corrupcion.
    let volumen = Volume::open(store).await.map_err(|e| match e {
        multidrive_core::CoreError::NoRoot => {
            "hay un volumen pero no se pudo leer su raiz: lo mas probable es que la frase de \
             paso no sea la correcta"
                .to_string()
        }
        otro => otro.to_string(),
    })?;

    // La maleta se refresca en cada apertura, y por eso no hace falta ningun boton para
    // guardarla: abrir un volumen exige tener configurados **todos** sus proveedores, asi que
    // si se llego hasta aqui, lo que hay en este equipo es al menos lo que el volumen
    // necesita. Guardarlo ahora es lo unico que evita que la maleta envejezca al anadir una
    // cuenta mas tarde.
    //
    // Sin ruido y sin poder tumbar la apertura: el volumen ya esta abierto.
    let _ = guardar_maleta(&backends, &frase).await;

    let resumen = resumir(&volumen, &header).map_err(|e| e.to_string())?;
    *estado.abierto.lock().await = Some(Abierto { volumen, header });
    Ok(resumen)
}

fn resumir(volumen: &Volume, header: &Header) -> Result<Resumen, multidrive_core::CoreError> {
    let layout = header.layout()?;
    Ok(Resumen {
        layout: layout.to_string(),
        proveedores: header.backends.clone(),
        tolera: layout.tolerated_failures(),
        sobrecoste: layout.storage_overhead(),
        reparto: header.reparto,
        generacion: volumen.root().generation,
        maleta: None,
    })
}

#[tauri::command]
async fn resumen(estado: tauri::State<'_, Estado>) -> Result<Resumen, String> {
    let guardia = estado.abierto.lock().await;
    let abierto = guardia.as_ref().ok_or("el volumen no esta abierto")?;
    resumir(&abierto.volumen, &abierto.header).map_err(|e| e.to_string())
}

#[tauri::command]
async fn cerrar(forzar: bool, estado: tauri::State<'_, Estado>) -> Result<(), String> {
    // Cerrar con algo sin subir se niega, y no por gusto: quien cierra da por terminado. Lo
    // pendiente no se pierde --el diario vive en el disco y se retoma al volver a abrir-- pero
    // hasta entonces solo esta aqui, y nadie lo va a subir mientras el volumen este cerrado.
    //
    // Se intenta subirlo antes de negarse: negarse sin haberlo intentado seria pedirle al
    // usuario que resuelva algo que podiamos resolver nosotros.
    if !forzar {
        let guardia = estado.abierto.lock().await;
        if let Some(abierto) = guardia.as_ref() {
            let _ = abierto.volumen.store().sync().await;
            let p = abierto.volumen.store().pendiente();
            if !p.a_salvo() {
                let rutas = abierto
                    .volumen
                    .store()
                    .rutas_pendientes()
                    .await
                    .unwrap_or_default();
                return Err(sin_subir(&rutas, p.trozos));
            }
        }
    }
    // Soltar el volumen suelta tambien la clave derivada, que es lo que importa.
    *estado.abierto.lock().await = None;
    Ok(())
}

/// Bytes en algo que se pueda leer de un vistazo. La ventana lo hace en JavaScript; esto es
/// para los mensajes que se arman aqui.
fn tamano(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut n = bytes as f64;
    let mut i = 0;
    while n >= 1024.0 && i < U.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{n:.1} {}", U[i])
    }
}

/// El aviso de cerrar con cosas sin subir. Nombra ficheros, no cifras.
fn sin_subir(rutas: &[RutaPendiente], trozos: usize) -> String {
    if rutas.is_empty() {
        // Puede pasar: lo unico pendiente es la raiz o algun nodo del arbol, sin datos de
        // ningun fichero detras. Se dice lo que es en vez de inventar una lista vacia.
        return format!(
            "Queda {trozos} trozo(s) sin subir, del indice del volumen. Desde otro equipo \
             todavia no se veria lo ultimo escrito."
        );
    }
    let cuantos = rutas.len();
    let lista: Vec<String> = rutas
        .iter()
        .take(5)
        .map(|r| format!("  {} ({})", r.ruta, tamano(r.bytes)))
        .collect();
    let mas = if cuantos > 5 {
        format!("\n  ...y {} mas", cuantos - 5)
    } else {
        String::new()
    };
    format!(
        "Hay {cuantos} fichero(s) que solo estan en esta maquina:\n{}{mas}\n\n\
         No se han podido subir ahora. Si cierras, siguen aqui y no los sube nadie hasta que \
         vuelvas a abrir el volumen.",
        lista.join("\n")
    )
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
/// - `del_volumen` es lo que ocupa MultiDrive ahi, y solo se sabe **despues de enumerar**.
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
/// cuanto de eso ocupa MultiDrive, y por eso ese campo llega vacio hasta que se pulsa
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

/// Olvida un proveedor: le quita sus claves al fichero de configuracion del usuario.
///
/// **Olvidar no es sacar del volumen.** Si el proveedor forma parte de uno, sus fragmentos
/// siguen donde estaban y la cabecera lo sigue nombrando; lo unico que se consigue quitandolo
/// de aqui es que el volumen deje de poder abrirse. Por eso, con el volumen abierto y el
/// proveedor dentro, esto se niega y manda a la orden que si sabe hacerlo.
///
/// Con el volumen cerrado no se puede saber sin preguntarle a las nubes, asi que ahi la
/// pregunta la hace la ventana antes de llamar.
#[tauri::command]
async fn olvidar_proveedor(
    nombre: String,
    estado: tauri::State<'_, Estado>,
) -> Result<String, String> {
    if let Some(a) = estado.abierto.lock().await.as_ref()
        && a.volumen.store().nombres().iter().any(|n| n == &nombre)
    {
        return Err(format!(
            "«{nombre}» forma parte del volumen que tienes abierto. Quitarlo de la \
             configuracion no lo saca del volumen: lo dejaria sin poder abrirse. Para sacarlo \
             de verdad, moviendo sus trozos a los demas:\n\n  multidrive providers remove {nombre}"
        ));
    }

    // Todo lo de una cuenta son claves `MULTIDRIVE_<RANURA>_...`. El guion bajo final importa:
    // sin el, quitar `gdrive` se llevaria tambien a `gdrive1`.
    let prefijo = format!("MULTIDRIVE_{}_", nombre.to_uppercase());
    let fichero = config::fichero_de_usuario()
        .ok_or("no se sabe donde vive la configuracion de este usuario")?;
    let quitadas = config::quitar(&fichero, &prefijo).map_err(|e| e.to_string())?;

    // Puede seguir configurado desde el entorno o desde el fichero de la maquina, que esto no
    // toca ni debe tocar. Callarlo dejaria la ventana diciendo que se borro algo que sigue ahi.
    let sigue = live::configurados()
        .map(|v| v.iter().any(|b| b.name() == nombre))
        .unwrap_or(false);
    if sigue {
        return Err(format!(
            "Se quitaron {quitadas} clave(s) de tu configuracion, pero «{nombre}» sigue \
             apareciendo: viene del entorno o del fichero de la maquina, y eso se edita a mano."
        ));
    }
    if quitadas == 0 {
        return Err(format!(
            "«{nombre}» no estaba en tu configuracion ({}), asi que no habia nada que quitar.",
            fichero.display()
        ));
    }
    Ok(format!(
        "«{nombre}» olvidado. Sus datos siguen en la nube; lo que se ha borrado es la forma de \
         llegar a ellos desde aqui."
    ))
}
#[tauri::command]
async fn pendiente(estado: tauri::State<'_, Estado>) -> Result<Pendiente, String> {
    let guardia = estado.abierto.lock().await;
    let abierto = guardia.as_ref().ok_or("el volumen no esta abierto")?;
    let p = abierto.volumen.store().pendiente();
    // Las rutas cuestan un paseo por el arbol pendiente, que esta en disco local: no se va a
    // la red. Si no se pudieran derivar se sigue dando el total, que es peor pero no es
    // mentira; quedarse sin decir nada porque falle el detalle si lo seria.
    let rutas = abierto
        .volumen
        .store()
        .rutas_pendientes()
        .await
        .unwrap_or_default();
    let testigo_perdido = estado
        .montado
        .lock()
        .await
        .as_ref()
        .and_then(|m| m.testigo.as_ref())
        .is_some_and(|g| g.perdido());

    // Los dos candados de aqui son de `std`, no de tokio, y su guardia no se puede tener en la
    // mano al cruzar un `await`: el futuro dejaria de ser `Send` y Tauri no lo acepta. Se leen
    // aqui, donde ya no queda ninguno por delante.
    let otro_escribiendo = estado
        .otro_escribiendo
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let ultimo_sync = estado
        .ultimo_sync
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();

    Ok(Pendiente {
        trozos: p.trozos,
        bytes: p.bytes,
        a_salvo: p.a_salvo(),
        rutas,
        raiz_sin_publicar: p.raiz,
        otro_escribiendo,
        testigo_perdido,
        ultimo_sync,
    })
}

// ---------------------------------------------------------------------------------------
// La maleta: la configuracion, cifrada, en las nubes del propio usuario
// ---------------------------------------------------------------------------------------

/// Lo que se cuenta despues de guardar o traer la maleta.
#[derive(Serialize, Default)]
struct Maleta {
    /// Proveedores donde quedo, o aquel del que salio.
    donde: Vec<String>,
    /// Los que no pudieron, con su motivo. Que uno falle no invalida al resto.
    fallos: Vec<String>,
    /// Cuantas claves lleva dentro.
    claves: usize,
    /// Cuantas se escribieron aqui al traerla.
    nuevas: usize,
    /// Cuantas estaban ya con **otro** valor y se dejaron como estaban.
    distintas: usize,
    /// Cuando se hizo, contado en palabras.
    hecha: Option<String>,
}

/// Si las cuentas configuradas ya guardan una maleta.
///
/// Se pregunta justo despues de enlazar una cuenta: si esa cuenta viene de otro equipo, lo que
/// toca es pedir la frase y traerse el resto, no dejar a nadie buscando un boton que no sabe
/// que existe.
#[tauri::command]
async fn hay_maleta() -> Result<bool, String> {
    let backends = match proveedores() {
        Ok(b) => b,
        // Sin proveedores no hay donde mirar, y eso no es un error que ensenar aqui.
        Err(_) => return Ok(false),
    };
    Ok(maleta::hay(&backends).await)
}

/// Trae la configuracion guardada en las nubes y escribe aqui lo que falte.
///
/// Lo que ya esta con otro valor **no se toca**: pisar una credencial que funciona con otra de
/// una maleta vieja es la forma mas rapida de quedarse sin acceso a una cuenta.
#[tauri::command]
async fn recuperar_maleta(frase: String) -> Result<Maleta, String> {
    if frase.trim().is_empty() {
        return Err("hace falta la frase de paso con la que se guardo".into());
    }
    let backends = proveedores()?;
    let (donde, m) = maleta::recoger(&backends, &frase)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no hay ninguna maleta en estas cuentas. Guardala primero desde el equipo que si tiene la configuracion.")?;

    let actuales = maleta::claves_de_este_equipo().map_err(|e| e.to_string())?;
    let mut nuevas = Vec::new();
    let mut distintas = 0;
    for (clave, valor) in &m.claves {
        match actuales.get(clave) {
            None => nuevas.push((clave.clone(), valor.clone())),
            Some(v) if v == valor => {}
            Some(_) => distintas += 1,
        }
    }

    let mut out = Maleta {
        donde: vec![donde],
        claves: m.claves.len(),
        nuevas: nuevas.len(),
        distintas,
        hecha: Some(antiguedad(m.hecha_en)),
        ..Default::default()
    };

    if !nuevas.is_empty() {
        let fichero = config::fichero_de_usuario()
            .ok_or("no se sabe donde guardar la configuracion de este usuario")?;
        config::guardar(&fichero, &nuevas).map_err(|e| e.to_string())?;
        out.fallos.clear();
    }
    Ok(out)
}

/// «hace 3 dias», que es lo que uno quiere saber de una maleta: si es de antes o de despues
/// del ultimo cambio.
fn antiguedad(hecha_en: u64) -> String {
    let ahora = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if hecha_en == 0 || ahora <= hecha_en {
        return "recien hecha".into();
    }
    match ahora - hecha_en {
        0..=90 => "hecha hace un momento".into(),
        s @ 91..=5400 => format!("hecha hace {} minuto(s)", s / 60),
        s @ 5401..=172_800 => format!("hecha hace {} hora(s)", s / 3600),
        s => format!("hecha hace {} dia(s)", s / 86_400),
    }
}
// ---------------------------------------------------------------------------------------
// Anadir proveedores al volumen
// ---------------------------------------------------------------------------------------

/// Lo que se ganaria metiendo en el volumen los proveedores configurados que no lo forman.
///
/// Es lo que se ensena **antes** de pulsar nada: que se gana, cuanto hay que mover y, si no
/// se puede desde aqui, por que.
#[derive(Serialize, Default)]
struct Ampliacion {
    /// Configurados que el volumen todavia no usa, en el orden en que entrarian.
    nuevos: Vec<String>,
    antes: String,
    despues: String,
    /// `espacio` o `margen`. No son lo mismo y no siempre se puede elegir: en un volumen
    /// lineal un proveedor mas es sitio, y en Reed-Solomon es una caida mas de margen.
    gana: String,
    /// Que parte del volumen cambia de proveedor, de 0 a 1.
    mueve: f64,
    /// Cuanto sitio anade. `null` si alguno de los nuevos no dice cuanto le queda libre: mas
    /// vale no decirlo que inventarlo.
    suma: Option<u64>,
    /// Por que no se puede desde aqui, si no se puede.
    problema: Option<String>,
}

/// Los proveedores que forman el volumen y los que estan configurados de mas.
type Bandos = (Vec<Arc<dyn Backend>>, Vec<Arc<dyn Backend>>);

/// Los proveedores del volumen **en el orden de la cabecera**, y los que estan de mas.
///
/// El orden importa: el indice de un proveedor es parte de la clave de cada fragmento. Por eso
/// se reconstruye desde la cabecera y no desde como esten configurados ahora.
fn separar_para_ampliar(
    header: &Header,
    configurados: &[Arc<dyn Backend>],
) -> Result<Bandos, String> {
    let mut del_volumen = Vec::new();
    for nombre in &header.backends {
        let Some(b) = configurados.iter().find(|b| b.name() == nombre) else {
            return Err(format!(
                "falta «{nombre}», que forma parte del volumen. Configuralo antes de anadir \
                 otros proveedores."
            ));
        };
        del_volumen.push(Arc::clone(b));
    }
    let nuevos = configurados
        .iter()
        .filter(|b| !header.backends.iter().any(|n| n == b.name()))
        .cloned()
        .collect();
    Ok((del_volumen, nuevos))
}

/// Cuanto sitio libre suman estos proveedores, o nada si alguno no lo dice.
async fn suma_libre(backends: &[Arc<dyn Backend>]) -> Option<u64> {
    let cuotas = futures::future::join_all(backends.iter().map(|b| b.cuota())).await;
    let mut total = 0u64;
    for c in &cuotas {
        total += c.as_ref().ok()?.libre()?;
    }
    Some(total)
}

/// Que topologia sale de anadir los que sobran, y que se gana con ello.
///
/// En un volumen lineal lo que se gana es sitio. En los demas **no se puede ganar sitio sin
/// reescribir el volumen entero**, asi que lo que se ofrece es margen de caidas; y si ni eso,
/// se dice por que.
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
            "pasar de {antes} a {destino} mueve todos los fragmentos de todos los trozos, o \
             sea, reescribe el volumen entero. Eso puede tardar horas o dias, asi que se hace \
             desde la linea de ordenes, que puede ir informando: `multidrive providers add \
             --mas-espacio`."
        ))
    }
}

#[tauri::command]
async fn ampliacion(estado: tauri::State<'_, Estado>) -> Result<Ampliacion, String> {
    let configurados = proveedores()?;
    let guardia = estado.abierto.lock().await;
    let abierto = guardia.as_ref().ok_or("el volumen no esta abierto")?;

    let antes = abierto.header.layout().map_err(|e| e.to_string())?;
    let (_, nuevos) = separar_para_ampliar(&abierto.header, &configurados)?;

    let mut out = Ampliacion {
        nuevos: nuevos.iter().map(|b| b.name().to_string()).collect(),
        antes: antes.to_string(),
        ..Default::default()
    };
    if nuevos.is_empty() {
        out.problema = Some("no hay ningun proveedor configurado que el volumen no use ya.".into());
        return Ok(out);
    }

    let (destino, gana) = match plan(antes, nuevos.len()) {
        Ok(p) => p,
        Err(e) => {
            out.problema = Some(e);
            return Ok(out);
        }
    };
    out.despues = destino.to_string();
    out.gana = gana.to_string();

    if gana == "espacio" {
        let base = abierto.volumen.store().pesos();
        let pesos = pesos_ampliados(&base, &nuevos).await;
        out.mueve = rebalance::fraccion_que_se_mueve(&base, &pesos);
        out.suma = suma_libre(&nuevos).await;
    } else {
        // Una copia o una paridad mas no anaden ni un byte de sitio, y decir lo contrario
        // seria lo que la gente espera oir en vez de lo que pasa.
        out.suma = Some(0);
    }
    Ok(out)
}

/// Mete en el volumen los proveedores configurados que no lo forman.
///
/// Hace **solo** las dos operaciones que no reescriben el volumen entero: mudar la fraccion
/// que le toca al que llega, en un volumen lineal, y escribir la redundancia que falta, en los
/// demas. La cara se queda en la linea de ordenes, que puede ir contando por donde va.
///
/// La cabecera se publica al final y solo si todo salio bien: hasta ese momento el volumen se
/// lee exactamente como antes.
#[tauri::command]
async fn ampliar(estado: tauri::State<'_, Estado>) -> Result<Resumen, String> {
    // Montado no se toca. Lo que se escribiera mientras esto corre se colocaria con el reparto
    // viejo y, al publicar la cabecera, se buscaria donde no esta.
    if estado.montado.lock().await.is_some() {
        return Err(
            "desmonta la unidad antes de ampliar el volumen: lo que se escriba \
                    mientras tanto acabaria en el proveedor equivocado."
                .into(),
        );
    }

    let configurados = proveedores()?;
    let mut guardia = estado.abierto.lock().await;
    let abierto = guardia.as_ref().ok_or("el volumen no esta abierto")?;

    // Y con cosas sin subir, tampoco, por lo mismo: subirian al sitio de antes.
    let p = abierto.volumen.store().pendiente();
    if !p.a_salvo() {
        return Err(format!(
            "quedan {} trozo(s) por subir. Espera a que esten a salvo en los proveedores \
             antes de ampliar el volumen.",
            p.trozos
        ));
    }

    let antes = abierto.header.layout().map_err(|e| e.to_string())?;
    let (del_volumen, nuevos) = separar_para_ampliar(&abierto.header, &configurados)?;
    if nuevos.is_empty() {
        return Err("no hay ningun proveedor configurado que el volumen no use ya.".into());
    }
    let (destino, gana) = plan(antes, nuevos.len())?;

    let todos: Vec<Arc<dyn Backend>> = del_volumen.iter().chain(&nuevos).cloned().collect();
    let nombres: Vec<String> = todos.iter().map(|b| b.name().to_string()).collect();

    let (trozos, ilegibles) = abierto.volumen.walk().await;
    if !ilegibles.is_empty() {
        // Un trozo que no se puede leer tampoco se puede mudar, y lo que no se muda se queda
        // donde la cabecera nueva ya no lo busca. Con el volumen a medias no se amplia.
        return Err(format!(
            "hay {} ruta(s) que no se pueden leer, empezando por «{}». Revisa la salud del \
             volumen antes de ampliarlo.",
            ilegibles.len(),
            ilegibles[0].0
        ));
    }

    let pesos = if gana == "espacio" {
        pesos_ampliados(&abierto.volumen.store().pesos(), &nuevos).await
    } else {
        abierto.header.pesos.clone()
    };

    let fallidos = if gana == "espacio" {
        let informe = abierto
            .volumen
            .store()
            .mudar_reparto(destino, &pesos, &todos, &trozos)
            .await
            .map_err(|e| e.to_string())?;
        informe.fallidos
    } else {
        let informe = abierto
            .volumen
            .store()
            .ampliar_redundancia(destino, &nuevos, &trozos)
            .await
            .map_err(|e| e.to_string())?;
        informe.fallidos
    };
    if !fallidos.is_empty() {
        // La cabecera es la que promete donde esta cada cosa. Publicarla con trozos a medias
        // prometeria algo que no esta, que es peor que no haber ampliado.
        return Err(format!(
            "{} trozo(s) no se pudieron escribir, asi que no se cambia nada: el volumen sigue \
             como estaba. Vuelve a intentarlo, que lo que si se subio no se repite. El \
             primero: {}",
            fallidos.len(),
            fallidos[0].1
        ));
    }

    // El unico punto de compromiso. Hasta aqui el volumen se leia con lo de antes.
    let cabecera = Header {
        version: abierto.header.version,
        layout: destino.to_string(),
        salt: abierto.header.salt.clone(),
        reparto: abierto.header.reparto,
        backends: nombres,
        pesos: pesos.clone(),
    };
    header::write(&todos, &cabecera)
        .await
        .map_err(|e| e.to_string())?;

    // Y solo ahora se puede tirar la copia vieja de lo que se mudo, y solo si lo nuevo esta
    // entero de verdad: que la subida no diera error dice que se escribio, no que este ahi.
    if gana == "espacio" {
        let revisor = abierto
            .volumen
            .store()
            .rehecho(destino, todos.clone(), pesos.clone())
            .map_err(|e| e.to_string())?;
        let salud = revisor.scrub_many(&trozos).await;
        if salud.is_complete() && salud.health.iter().all(|h| h.missing.is_empty()) {
            abierto
                .volumen
                .store()
                .olvidar_origen(&pesos, &trozos)
                .await;
        }
        // Si no esta entero no se borra nada. De las dos equivocaciones posibles, dejar sitio
        // ocupado de mas es la barata.
    }

    // El volumen que sigue abierto en esta ventana es el de antes: mismos proveedores, mismos
    // pesos. Se rehace sobre la cabecera nueva, con la misma clave y sin volver a pedir la
    // frase.
    let raiz = abierto.volumen.root().clone();
    let store = abierto
        .volumen
        .store()
        .rehecho(destino, todos.clone(), pesos)
        .map_err(|e| e.to_string())?
        .with_cache(CACHE);

    let dir = writeback::dir_de_volumen(writeback::dir_base(), &cabecera.salt);
    let cola = WriteQueue::open(&dir)
        .await
        .map_err(|e| format!("no se pudo reabrir la cola de subida: {e}"))?;

    let volumen = Volume::at(store.with_writeback(Arc::new(cola)), raiz);
    let resumen = resumir(&volumen, &cabecera).map_err(|e| e.to_string())?;
    *guardia = Some(Abierto {
        volumen,
        header: cabecera,
    });
    Ok(resumen)
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
    /// `true` si esta copia de MultiDrive trae dentro las credenciales de la aplicacion.
    ///
    /// Cuando es asi, la ventana no pide el ID ni el secreto: identifican a MultiDrive y no
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
/// Sale del catalogo de `multidrive-backend`, que es tambien de donde sale la ayuda de la
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
                aplicacion_propia: multidrive_backend::aplicacion::configurada(t.clave).is_some(),
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
    let de_la_app = multidrive_backend::aplicacion::configurada(&tipo);
    let (Some(client_id), Some(client_secret)) = (
        puesto("CLIENT_ID").or_else(|| de_la_app.as_ref().map(|a| a.client_id.clone())),
        puesto("CLIENT_SECRET").or_else(|| de_la_app.as_ref().map(|a| a.client_secret.clone())),
    ) else {
        // Hacen falta antes de abrir el navegador: sin ellos el proveedor rechaza la
        // peticion, y el usuario leeria el error en una pestana y no aqui.
        return Err(
            "esta copia de MultiDrive no trae aplicacion registrada, asi que hace falta el ID \
             y el secreto de una. Son de la aplicacion, no de tu cuenta"
                .into(),
        );
    };

    let conectada = multidrive_backend::oauth::conectar(
        oauth,
        &client_id,
        &client_secret,
        &multidrive_backend::oauth::abrir_en_navegador,
        // En el escritorio no hace falta volver a ningun sitio: la ventana sigue detras.
        None,
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
        multidrive_backend::OpendalBackend,
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
    multidrive_backend::conformance::prueba_rapida(&backend)
        .await
        .map_err(|e| format!("no se guarda nada porque el proveedor no responde: {e}"))?;

    // Y que no sea una cuenta que ya esta. Dos proveedores sobre la misma cuenta se ven como
    // dos, cuentan como dos al elegir topologia y se anuncian como «aguanta perder uno»;
    // pero el dia que se pierda esa cuenta se pierden las dos copias a la vez.
    if let Ok(Some(otro)) = multidrive_backend::live::ya_configurada(t, &backend).await {
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
    /// Proveedores donde ademas hay configuracion sellada, que tambien se va.
    con_maleta: Vec<String>,
    /// Lo que hay que escribir para confirmar: el nombre de **uno** de los proveedores.
    ///
    /// Se pide un nombre y no un «si» porque obliga a mirar la lista de lo que se va a borrar,
    /// y un boton que solo hay que pulsar se pulsa sin leer. Antes se pedia la lista entera,
    /// que cumplia lo mismo pero crecia con el numero de cuentas hasta volverse algo que se
    /// copia y se pega sin leer.
    confirmacion: String,
    /// Cual de la lista es, contando desde 1. Es lo unico que se le ensena al usuario.
    ///
    /// El nombre esperado viaja aparte para poder comparar mientras se escribe, pero **no se
    /// escribe en la pantalla**: si se enseñara, no habria que mirar la lista.
    posicion: usize,
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

    let inventario = multidrive_core::retirar::inventariar(&backends)
        .await
        .map_err(|e| {
            format!("{e}\nNo se puede borrar sin poder mirarlo entero: el recuento seria falso")
        })?;

    let nombres: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();
    let cual = multidrive_core::retirar::a_quien_nombrar(&header.salt, nombres.len());
    Ok(ParaBorrar {
        layout: header
            .layout()
            .map_or_else(|_| header.layout.clone(), |l| l.to_string()),
        confirmacion: nombres[cual].clone(),
        posicion: cual + 1,
        proveedores: nombres,
        fragmentos: inventario.fragmentos(),
        bytes: inventario.bytes(),
        con_maleta: inventario.con_maleta,
    })
}

/// Retira el volumen. **No se puede deshacer.**
#[tauri::command]
async fn borrar_volumen(
    confirmacion: String,
    estado: tauri::State<'_, Estado>,
) -> Result<String, String> {
    let backends = proveedores()?;
    // Se vuelve a calcular aqui en vez de fiarse de lo que mande la pantalla: es un borrado
    // irreversible, y quien confirma tiene que haber acertado de verdad.
    let header = header::read(&backends)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no hay ningun volumen en estos proveedores")?;
    let nombres: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();
    let cual = multidrive_core::retirar::a_quien_nombrar(&header.salt, nombres.len());
    if confirmacion.trim() != nombres[cual] {
        // No se dice cual era el bueno: el mensaje seria la respuesta al examen.
        return Err(format!(
            "eso no es el nombre del proveedor numero {} de la lista. No se ha borrado nada.",
            cual + 1
        ));
    }

    // Con algo montado, borrar dejaria una unidad sirviendo un volumen que ya no existe. Se
    // desmonta y se cierra antes, no despues.
    *estado.montado.lock().await = None;
    *estado.abierto.lock().await = None;
    *estado.ocupado.lock().await = None;

    let hecho = multidrive_core::retirar::retirar(&backends)
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

// ---------------------------------------------------------------------------------------
// El icono de la bandeja del sistema
// ---------------------------------------------------------------------------------------

/// Identificador del icono, para poder buscarlo y cambiarle el rotulo desde otro sitio.
const BANDEJA: &str = "principal";

/// Pone el icono en la bandeja, con su menu.
///
/// No es adorno. La unidad montada vive mientras viva **este proceso**: `Montado` la suelta al
/// destruirse, y el hilo que sube lo pendiente se para con el. Sin bandeja, cerrar la ventana
/// desmonta la unidad delante del usuario, que es lo que pasaba hasta ahora.
fn bandeja(app: &tauri::AppHandle) -> tauri::Result<()> {
    let abrir = MenuItem::with_id(app, "abrir", "Abrir MultiDrive", true, None::<&str>)?;
    let salir = MenuItem::with_id(app, "salir", "Salir y desmontar", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&abrir, &PredefinedMenuItem::separator(app)?, &salir])?;

    let icono = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("el icono de la ventana".into()))?;

    TrayIconBuilder::with_id(BANDEJA)
        .icon(icono)
        .tooltip("MultiDrive")
        .menu(&menu)
        // Con el menu en el boton izquierdo, el gesto de siempre --un click para abrir-- no
        // funcionaria: abriria un menu de dos entradas para llegar a lo mismo.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id.as_ref() {
            "abrir" => mostrar(app),
            // Sale de verdad: desmonta, porque `Montado` se suelta con el proceso. El rotulo
            // lo dice para que nadie lo pulse creyendo que solo cierra la ventana.
            "salir" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                mostrar(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Trae la ventana al frente, este oculta o minimizada.
fn mostrar(app: &tauri::AppHandle) {
    if let Some(v) = app.get_webview_window("main") {
        let _ = v.show();
        let _ = v.unminimize();
        let _ = v.set_focus();
    }
}

/// Cambia lo que dice el icono al pasar el raton por encima.
///
/// Es el unico sitio donde se puede ver que hace el programa con la ventana cerrada, asi que
/// dice lo que importa: si hay algo montado y donde.
fn rotulo_de_bandeja(app: &tauri::AppHandle, texto: &str) {
    if let Some(icono) = app.tray_by_id(BANDEJA) {
        let _ = icono.set_tooltip(Some(texto));
    }
}

/// Si cerrar la ventana dejaria algo a medias.
///
/// Con la unidad montada, cerrar la esconde en la bandeja; sin nada montado, cerrar cierra. Un
/// programa que sigue vivo sin hacer nada y sin ventana es un programa que se ha perdido.
///
/// Si el estado esta ocupado en ese instante no se arriesga: esconde. Equivocarse hacia el
/// lado de no desmontar cuesta un icono de mas; hacia el otro, una unidad que desaparece
/// mientras alguien copia ficheros.
fn sigue_trabajando(ventana: &tauri::Window) -> bool {
    use tauri::Manager;
    let estado = ventana.state::<Estado>();
    match estado.montado.try_lock() {
        Ok(g) => g.is_some(),
        Err(_) => true,
    }
}

/// Muere diciendo por que, cuando no hay ventana donde decirlo.
///
/// `windows_subsystem = "windows"` quita la consola: un panico aqui no lo ve nadie. El proceso
/// desaparece y quien pulso el acceso directo se queda mirando un icono que no hace nada, que
/// es el peor fallo posible de los que se pueden tener.
///
/// El caso que mas se va a dar es que falte el runtime de WebView2, que es lo que dibuja esta
/// interfaz: Windows 11 lo trae de fabrica y algunos Windows 10 no. Desde que el instalador
/// trae la ventana, eso le puede pasar a alguien que no somos nosotros.
#[cfg(windows)]
fn morir_diciendolo(motivo: &str) -> ! {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    let ancho = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(Some(0))
            .collect()
    };
    let texto = ancho(&format!(
        "MultiDrive no pudo abrir su ventana.\n\n{motivo}\n\n\
         Lo mas probable es que falte el runtime de WebView2, que es lo que dibuja esta \
         interfaz. Es gratuito y lo publica Microsoft:\n\n\
         https://developer.microsoft.com/microsoft-edge/webview2/\n\n\
         La orden `multidrive` funciona igual sin el."
    ));
    let titulo = ancho("MultiDrive");

    // SEGURIDAD: los dos vectores acaban en cero y viven durante toda la llamada. La ventana
    // padre va nula porque no hay ninguna: de eso se trata.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            texto.as_ptr(),
            titulo.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
    std::process::exit(1)
}

/// Lo mismo donde si hay consola detras.
#[cfg(not(windows))]
fn morir_diciendolo(motivo: &str) -> ! {
    eprintln!("MultiDrive no pudo abrir su ventana: {motivo}");
    std::process::exit(1)
}
fn main() {
    tauri::Builder::default()
        .manage(Estado::default())
        .setup(|app| {
            bandeja(app.handle())?;
            Ok(())
        })
        // Cerrar la ventana con algo montado la esconde en la bandeja en vez de tirar la
        // unidad abajo. Sin nada montado, cerrar cierra.
        .on_window_event(|ventana, evento| {
            if let WindowEvent::CloseRequested { api, .. } = evento
                && sigue_trabajando(ventana)
            {
                api.prevent_close();
                let _ = ventana.hide();
            }
        })
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
            guardar_proveedor,
            olvidar_proveedor,
            recuperar_maleta,
            hay_maleta,
            ampliacion,
            ampliar
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| morir_diciendolo(&e.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn salud(presentes: &[usize], ausentes: &[usize]) -> ChunkHealth {
        ChunkHealth {
            id: multidrive_core::ChunkId::of(b"da igual"),
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
