//! Volúmenes abiertos, y el runtime donde viven.
//!
//! # Por que hay sesiones
//!
//! Abrir un volumen desde un movil cuesta **seis segundos**, medidos: tres de leer la cabecera
//! a cuatro cuentas, uno y medio de abrir y algo mas de un segundo de listar la raiz. De esos
//! seis, 156 ms son CPU; el resto es red. Abrir en cada llamada convertiria cualquier pantalla
//! en algo inusable, asi que se abre una vez y se guarda.
//!
//! # Por que un identificador y no un puntero
//!
//! Kotlin recibe un `long` que es solo un numero. Podria devolverse un puntero envuelto, que es
//! lo que suele hacerse, pero entonces un `long` equivocado desde Kotlin --un cero, uno viejo,
//! uno inventado-- seria un fallo de memoria. Asi, lo peor que pasa es que no se encuentre la
//! sesion y se devuelva un error.
//!
//! # Hilos
//!
//! Los metodos de un `DocumentsProvider` llegan **desde cualquier hilo** y son sincronos. El
//! runtime es uno solo para todo el proceso y se bloquea contra el. Es correcto por lo mismo
//! que en los hilos del kernel de FUSE: **no son hilos del runtime**, asi que bloquearlos no
//! puede bloquear a nadie mas.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use multidrive_backend::Backend;
use multidrive_core::header;
use multidrive_core::tree::Volume;

/// 64 MiB de cache en memoria. Con la red mandando sobre todo lo demas, cada trozo que no haya
/// que volver a pedir vale mas que cualquier otra optimizacion.
const CACHE: usize = 64 * 1024 * 1024;

fn runtime() -> &'static tokio::runtime::Runtime {
    static R: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    R.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            // Cuatro: es lo que hace falta para preguntarle a cuatro cuentas a la vez, que es
            // de donde salen los tres segundos de la cabecera. Mas no ayudarian, porque lo que
            // se espera es la red y no el procesador.
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("no se pudo arrancar el runtime")
    })
}

/// Un volumen abierto, y con que permiso.
pub struct Abierto {
    /// El volumen. Va tras un candado porque escribir necesita `&mut`, y porque escribir y leer
    /// a la vez sobre el mismo arbol no tendria sentido.
    ///
    /// Serializar las lecturas no cuesta nada aqui: ya lo estaban. El descriptor que entrega el
    /// proveedor de documentos las reparte por **un solo** hilo, asi que el candado no anade
    /// espera que no hubiera.
    pub volumen: Arc<tokio::sync::Mutex<Volume>>,
    /// El permiso de escritura mientras dure la sesion. `None` si se abrio para leer.
    ///
    /// Vive aqui y no en una variable suelta porque asi se suelta **con la sesion**: cerrar el
    /// volumen libera el testigo, y que Android mate el proceso tambien, porque entonces caduca.
    testigo: Option<multidrive_core::testigo::Guardia>,
    /// Si esta sesion se abrio para escribir.
    ///
    /// Va **aparte del testigo**, y eso es el arreglo de un fallo real: desde que el testigo
    /// dejo de bloquear, no tenerlo solo significa que hay otro equipo escribiendo tambien, no
    /// que aqui no se pueda. Mientras esto miraba el testigo, abrir con escritura mientras el
    /// escritorio tenia el volumen montado dejaba la aplicacion creyendose de solo lectura: sin
    /// borrar, sin compartir y sin decir por que.
    escritura: bool,
}

impl Abierto {
    /// Si esta sesion puede escribir.
    pub fn escribe(&self) -> bool {
        self.escritura
    }

    /// Si el permiso se perdio por el camino.
    pub fn testigo_perdido(&self) -> bool {
        self.testigo.as_ref().is_some_and(|g| g.perdido())
    }
}

fn abiertos() -> &'static Mutex<HashMap<i64, Arc<Abierto>>> {
    static A: OnceLock<Mutex<HashMap<i64, Arc<Abierto>>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bloquea el mapa, recuperandose de un candado envenenado.
///
/// Un panico mientras se tenia el candado envenena el `Mutex` para siempre. Aqui lo que hay
/// dentro es un mapa de volumenes abiertos: si alguien reventó sosteniendolo, el mapa sigue
/// siendo un mapa. Negarse a seguir por eso convertiria un fallo de una operacion en una
/// aplicacion que ya no abre nada hasta reiniciarla.
fn mapa() -> std::sync::MutexGuard<'static, HashMap<i64, Arc<Abierto>>> {
    abiertos().lock().unwrap_or_else(|e| e.into_inner())
}

/// Corre algo en el runtime del proceso, sin volumen de por medio.
///
/// Lo usa conectar una cuenta, que es asincrono pero no tiene nada abierto todavia. Se comparte
/// el runtime en vez de crear otro: dos runtimes en el mismo proceso son dos grupos de hilos y
/// dos pilas de conexiones, y ninguna de las dos cosas hace falta.
pub fn en_el_runtime<T>(f: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    runtime().block_on(f)
}

/// Abre un volumen y devuelve su identificador. **Cuesta segundos**: nunca en el hilo de la
/// interfaz.
pub fn abrir(config: &str, frase: &str, escritura: bool) -> Result<i64, String> {
    let backends = multidrive_backend::live::configurados_de(Path::new(config))
        .map_err(|e| format!("configuracion invalida: {e}"))?;
    if backends.is_empty() {
        return Err("no hay ningun proveedor en esa configuracion".into());
    }
    let backends: Vec<Arc<dyn Backend>> = backends
        .into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect();

    let (volumen, testigo) = runtime().block_on(async {
        let cabecera = header::read(&backends)
            .await
            .map_err(|e| format!("no se pudo leer la cabecera: {e}"))?
            .ok_or("no hay ningun volumen en esas cuentas")?;

        // El testigo **antes de abrir**, no despues. Abrir cuesta segundos; descubrir al final
        // que otro esta escribiendo seria haber hecho esperar para nada.
        // No se niega si otro lo tiene: escribir se reaplica si alguien se adelanta, asi que
        // dos equipos en ficheros distintos conviven. Negarse ademas seria caro de verdad aqui:
        // con el volumen montado como servicio en el escritorio, el movil no podria escribir
        // nunca.
        let (testigo, _otro) = if escritura {
            multidrive_core::testigo::Guardia::coger_o_compartir(
                backends.clone(),
                "la aplicacion de Android",
            )
            .await
        } else {
            (None, None)
        };

        let almacen = cabecera
            .almacen(backends, frase, CACHE)
            .map_err(|e| format!("no se pudo derivar la clave: {e}"))?;
        // Una frase equivocada no da error de descifrado: da un puntero de raiz que no se abre,
        // o sea «no hay raiz». Sin decirlo, parece corrupcion.
        let v = Volume::open(almacen)
            .await
            .map_err(|e| format!("no se pudo abrir: {e}\n¿Es esa la frase de paso?"))?;
        Ok::<_, String>((v, testigo))
    })?;

    static SIGUIENTE: AtomicI64 = AtomicI64::new(1);
    let id = SIGUIENTE.fetch_add(1, Ordering::Relaxed);
    mapa().insert(
        id,
        Arc::new(Abierto {
            volumen: Arc::new(tokio::sync::Mutex::new(volumen)),
            testigo,
            escritura,
        }),
    );
    Ok(id)
}

/// Vuelve a mirar si hay una raiz mas nueva, y se pasa a ella.
///
/// Devuelve `true` si algo cambio, que es lo que le dice a la pantalla si merece la pena tirar
/// lo que tenia listado.
///
/// Un `Volume` guarda la raiz que leyo al abrirse. Para el escritorio eso basta --el que abre
/// es el que escribe-- pero aqui no: el movil es un lector, y mientras mira, un escritorio
/// puede estar escribiendo. Sin esto, lo subido desde otro equipo **no aparece nunca**, y
/// cerrar y volver a abrir era la unica salida. Paso de verdad la primera vez que se uso.
///
/// No se vuelve a derivar la clave, que es lo caro: se clona el `ChunkStore`, que ya la lleva
/// dentro, y solo se paga una lectura del puntero de raiz.
pub fn refrescar(id: i64) -> Result<bool, String> {
    let abierto = mapa().get(&id).cloned().ok_or("esa sesion no existe")?;

    runtime().block_on(async {
        // El candado se coge y se suelta dentro: quien estuviera leyendo termina su lectura
        // antes de que el arbol cambie debajo, y los nodos son inmutables, asi que lo que ya
        // tenia en la mano sigue siendo coherente.
        let mut v = abierto.volumen.lock().await;

        let nueva = multidrive_core::tree::raiz_actual(v.store())
            .await
            .map_err(|e| e.to_string())?;
        let Some(nueva) = nueva else {
            return Ok(false);
        };
        if nueva.generation <= v.root().generation {
            return Ok(false);
        }
        let store = v.store().clone();
        *v = Volume::at(store, nueva);
        Ok(true)
    })
}

/// Suelta un volumen. Con el se suelta la clave derivada, que es lo que importa.
pub fn cerrar(id: i64) {
    mapa().remove(&id);
}

/// Corre algo sobre un volumen abierto.
///
/// El `Arc` se saca del mapa y el candado se suelta **antes** de empezar: si no, una lectura de
/// varios segundos contra la red dejaria bloqueado a todo el que quisiera abrir o cerrar otra
/// cosa mientras tanto.
pub fn con<T, F>(id: i64, f: F) -> Result<T, String>
where
    F: FnOnce(Arc<Abierto>) -> std::pin::Pin<Box<dyn Future<Output = Result<T, String>> + Send>>,
{
    let abierto = sesion(id)?;
    runtime().block_on(f(abierto))
}

/// La sesion, o un error que dice que hacer.
///
/// Que no exista es el caso normal, no el raro: Android mata el proceso cuando le conviene, y
/// Kotlin puede volver con un identificador de antes de que lo matara.
pub fn sesion(id: i64) -> Result<Arc<Abierto>, String> {
    mapa().get(&id).cloned().ok_or_else(|| {
        "esa sesion no existe: puede que la aplicacion se cerrara y haya que volver a abrir el \
         volumen"
            .to_string()
    })
}

/// Escribe un fichero en el volumen, leyendolo de una ruta local del telefono.
///
/// Se pasa una ruta y no los bytes a proposito: un fichero puede no caber en memoria, y lo que
/// el proveedor de documentos recibe del sistema llega a trozos y por offsets. Lo junta el lado
/// de Kotlin en un temporal y aqui se sube de una vez, que es la unica forma en que el troceado
/// y el reparto ven el fichero entero.
pub fn escribir(id: i64, ruta: &str, local: &str) -> Result<(), String> {
    let abierto = sesion(id)?;
    if !abierto.escribe() {
        return Err("este volumen esta abierto solo para leer".into());
    }
    let ruta = ruta.to_string();
    let local = local.to_string();
    runtime().block_on(async move {
        let fichero = tokio::fs::File::open(&local)
            .await
            .map_err(|e| format!("no se pudo leer lo que se iba a subir: {e}"))?;
        let mut v = abierto.volumen.lock().await;
        v.write_file(&ruta, fichero)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// Crea un directorio, con los que falten por encima.
pub fn crear_directorio(id: i64, ruta: &str) -> Result<(), String> {
    let abierto = sesion(id)?;
    if !abierto.escribe() {
        return Err("este volumen esta abierto solo para leer".into());
    }
    let ruta = ruta.to_string();
    runtime().block_on(async move {
        let mut v = abierto.volumen.lock().await;
        v.mkdir_p(&ruta)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// Borra un fichero o un directorio vacio.
pub fn borrar(id: i64, ruta: &str) -> Result<(), String> {
    let abierto = sesion(id)?;
    if !abierto.escribe() {
        return Err("este volumen esta abierto solo para leer".into());
    }
    let ruta = ruta.to_string();
    runtime().block_on(async move {
        let mut v = abierto.volumen.lock().await;
        v.remove(&ruta).await.map(|_| ()).map_err(|e| e.to_string())
    })
}

/// Mueve o renombra. Es la misma operacion: renombrar es mover dentro del mismo directorio.
///
/// El arbol lo hace en **una sola raiz**, asi que para cualquiera que este leyendo el fichero
/// nunca deja de estar en alguno de los dos sitios. Eso importa mas aqui que en el escritorio:
/// el movil renombra mientras otro equipo puede estar leyendo el mismo volumen.
///
/// No se pisa lo que ya exista en el destino. La app de Archivos pregunta antes de renombrar a
/// un nombre ocupado, pero eso es cosa suya y no de todas; perder un fichero por confiar en que
/// alguien de fuera pregunte no es algo que se pueda deshacer.
pub fn mover(id: i64, origen: &str, destino: &str) -> Result<(), String> {
    let abierto = sesion(id)?;
    if !abierto.escribe() {
        return Err("este volumen esta abierto solo para leer".into());
    }
    let (origen, destino) = (origen.to_string(), destino.to_string());
    runtime().block_on(async move {
        let mut v = abierto.volumen.lock().await;
        v.rename(&origen, &destino, false)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Una sesion que no existe se dice, no revienta.
    ///
    /// Es el caso normal, no el raro: Android mata el proceso de un `DocumentsProvider` cuando
    /// le conviene, y Kotlin puede volver con un identificador de antes de que lo matara.
    #[test]
    fn una_sesion_inventada_da_error_y_no_panico() {
        let r = con(987_654, |_v| Box::pin(async { Ok::<(), String>(()) }));
        let e = r.unwrap_err();
        assert!(e.contains("no existe"), "{e}");
    }

    /// Cerrar algo que no esta abierto tampoco es un error: es lo que Android hara al
    /// recuperarse de haber matado el proceso.
    #[test]
    fn cerrar_dos_veces_no_pasa_nada() {
        cerrar(987_654);
        cerrar(987_654);
    }
}
