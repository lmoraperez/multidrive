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

fn abiertos() -> &'static Mutex<HashMap<i64, Arc<Volume>>> {
    static A: OnceLock<Mutex<HashMap<i64, Arc<Volume>>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bloquea el mapa, recuperandose de un candado envenenado.
///
/// Un panico mientras se tenia el candado envenena el `Mutex` para siempre. Aqui lo que hay
/// dentro es un mapa de volumenes abiertos: si alguien reventó sosteniendolo, el mapa sigue
/// siendo un mapa. Negarse a seguir por eso convertiria un fallo de una operacion en una
/// aplicacion que ya no abre nada hasta reiniciarla.
fn mapa() -> std::sync::MutexGuard<'static, HashMap<i64, Arc<Volume>>> {
    abiertos().lock().unwrap_or_else(|e| e.into_inner())
}

/// Abre un volumen y devuelve su identificador. **Cuesta segundos**: nunca en el hilo de la
/// interfaz.
pub fn abrir(config: &str, frase: &str) -> Result<i64, String> {
    let backends = multidrive_backend::live::configurados_de(Path::new(config))
        .map_err(|e| format!("configuracion invalida: {e}"))?;
    if backends.is_empty() {
        return Err("no hay ningun proveedor en esa configuracion".into());
    }
    let backends: Vec<Arc<dyn Backend>> = backends
        .into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect();

    let volumen = runtime().block_on(async {
        let cabecera = header::read(&backends)
            .await
            .map_err(|e| format!("no se pudo leer la cabecera: {e}"))?
            .ok_or("no hay ningun volumen en esas cuentas")?;
        let almacen = cabecera
            .almacen(backends, frase, CACHE)
            .map_err(|e| format!("no se pudo derivar la clave: {e}"))?;
        // Una frase equivocada no da error de descifrado: da un puntero de raiz que no se abre,
        // o sea «no hay raiz». Sin decirlo, parece corrupcion.
        Volume::open(almacen)
            .await
            .map_err(|e| format!("no se pudo abrir: {e}\n¿Es esa la frase de paso?"))
    })?;

    static SIGUIENTE: AtomicI64 = AtomicI64::new(1);
    let id = SIGUIENTE.fetch_add(1, Ordering::Relaxed);
    mapa().insert(id, Arc::new(volumen));
    Ok(id)
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
    F: FnOnce(Arc<Volume>) -> std::pin::Pin<Box<dyn Future<Output = Result<T, String>> + Send>>,
{
    let volumen = mapa()
        .get(&id)
        .cloned()
        .ok_or("esa sesion no existe: puede que la aplicacion se cerrara y haya que volver a abrir el volumen")?;
    runtime().block_on(f(volumen))
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
