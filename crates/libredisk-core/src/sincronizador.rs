//! Subida en segundo plano mientras el volumen esta montado.
//!
//! Con escritura diferida, escribir en el montaje no sube nada: deja el trozo en la cola y
//! vuelve. Alguien tiene que llevarselo a los proveedores, y ese alguien no puede ser la
//! propia escritura, porque cada `write` de 4 KiB seria una subida sincrona a varias nubes.
//!
//! # Por que una copia del almacen y no el volumen
//!
//! No es un rodeo. Subir puede tardar minutos, y sostener el cerrojo del volumen durante ese
//! rato bloquearia todas las escrituras del sistema de ficheros. La copia comparte caches y
//! cola con el original, asi que ve exactamente lo mismo sin estorbar.
//!
//! # Por que un hilo propio y no una tarea
//!
//! Porque bloquea sobre el runtime, y hacerlo desde un hilo del runtime es un panico. Es la
//! misma razon por la que funcionan los metodos de WinFsp y de FUSE: sus hilos tampoco son
//! del runtime.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::{ChunkStore, writeback::SyncReport};

/// Cada cuanto se mira si hay que parar, mientras se espera al siguiente intento.
///
/// Se duerme a trocitos para poder parar enseguida al desmontar, en vez de tener que esperar
/// a que se cumpla el intervalo entero. Al desmontar, la espera la nota el usuario.
const PASO: Duration = Duration::from_millis(200);

/// Que hacer con lo que pase en cada pasada.
///
/// Va como devolucion de llamada y no como `println!` porque quien lo arranca sabe donde
/// escribir y el nucleo no: la linea de ordenes tiene consola, el montaje por servicio tiene
/// una tuberia cerrada donde escribir mata el proceso, y una ventana no tiene ninguna de las
/// dos.
pub type Informar = Box<dyn Fn(Resultado<'_>) + Send + 'static>;

/// Lo que paso en una pasada.
pub enum Resultado<'a> {
    /// Subio algo, o descarto algo que ya no hacia falta.
    Hecho(&'a SyncReport),
    /// No se pudo subir. No para el montaje: lo pendiente sigue en disco y el siguiente
    /// intento lo recoge. Callarselo si seria grave.
    Fallo(&'a str),
}

/// Un hilo que sube lo pendiente cada cierto tiempo. Al soltarlo, para y se espera a que
/// termine lo que estuviera haciendo.
pub struct Sincronizador {
    parar: Arc<AtomicBool>,
    hilo: Option<std::thread::JoinHandle<()>>,
}

impl Sincronizador {
    /// Arranca sobre una copia de `store`. Tiene que llamarse desde dentro de un runtime.
    pub fn arrancar(store: ChunkStore, cada: Duration, informar: Informar) -> Self {
        let parar = Arc::new(AtomicBool::new(false));
        let rt = tokio::runtime::Handle::current();

        let hilo = std::thread::spawn({
            let parar = Arc::clone(&parar);
            move || {
                while !parar.load(Ordering::Relaxed) {
                    let mut queda = cada;
                    while queda > Duration::ZERO && !parar.load(Ordering::Relaxed) {
                        let dormir = PASO.min(queda);
                        std::thread::sleep(dormir);
                        queda -= dormir;
                    }
                    if parar.load(Ordering::Relaxed) {
                        return;
                    }

                    if store.pendiente().a_salvo() {
                        continue;
                    }
                    match rt.block_on(store.sync()) {
                        Ok(i) if i.subidos > 0 || i.descartados > 0 => {
                            informar(Resultado::Hecho(&i));
                        }
                        Ok(_) => {}
                        Err(e) => informar(Resultado::Fallo(&e.to_string())),
                    }
                }
            }
        });

        Self {
            parar,
            hilo: Some(hilo),
        }
    }
}

impl Drop for Sincronizador {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        if let Some(h) = self.hilo.take() {
            let _ = h.join();
        }
    }
}
