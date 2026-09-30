//! Enterarse de lo que publica otro equipo, sin tener que desmontar.
//!
//! Un `Volume` guarda la raiz que leyo al abrirse y no vuelve a mirar. Mientras el que abria era
//! el unico que escribia, eso era correcto y ademas barato. Ya no lo es: desde que dos equipos
//! pueden escribir a la vez, una unidad montada ayer ensena el volumen de ayer.
//!
//! Y el sintoma es de los peores: **no hay error**. Se sube una foto desde el movil, la
//! aplicacion dice que la subio --y es verdad-- y en la unidad del escritorio no aparece. Parece
//! que se perdio algo cuando lo unico viejo era la vista.
//!
//! # Cuando se adopta la raiz nueva, y cuando no
//!
//! Solo si **no queda nada pendiente de subir**. Con escritura diferida, lo que se ha escrito
//! aqui vive en la cola y todavia no esta publicado; cambiar de raiz en ese momento tiraria esos
//! cambios por la ventana. Se espera a que la cola este vacia, que es lo que hace el
//! sincronizador cada pocos segundos.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::tree::{Root, Volume};

/// Cada cuanto se mira, como mucho, para poder parar enseguida al desmontar.
const PASO: Duration = Duration::from_millis(200);

/// Lo que se cuenta cuando hay novedades. Donde escribirlo lo sabe quien monta.
pub type Informar = Box<dyn Fn(&Root) + Send + 'static>;

/// Un hilo que mira si otro equipo publico algo, y se pasa a ello.
pub struct Refrescador {
    parar: Arc<AtomicBool>,
    hilo: Option<std::thread::JoinHandle<()>>,
}

impl Refrescador {
    /// Arranca sobre el volumen montado. Tiene que llamarse desde dentro de un runtime.
    pub fn arrancar(volumen: Arc<RwLock<Volume>>, cada: Duration, informar: Informar) -> Self {
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

                    // La consulta se hace **sin** el cerrojo de escritura: es una peticion de
                    // red por proveedor, y tenerlo cogido mientras tanto pararia el sistema de
                    // ficheros entero por algo que la mayoria de las veces no cambia nada.
                    let (store, actual, a_salvo) = {
                        let Ok(v) = volumen.read() else { return };
                        (
                            v.store().clone(),
                            v.root().generation,
                            v.store().pendiente().a_salvo(),
                        )
                    };
                    if !a_salvo {
                        // Hay cosas escritas aqui sin publicar. Cambiar de raiz ahora las
                        // tiraria; el sincronizador las subira en su proxima pasada y entonces
                        // ya se mirara.
                        continue;
                    }

                    let Ok(Some(nueva)) = rt.block_on(crate::tree::raiz_actual(&store)) else {
                        continue;
                    };
                    if nueva.generation <= actual {
                        continue;
                    }

                    let Ok(mut v) = volumen.write() else { return };
                    // Se vuelve a comprobar con el cerrojo cogido: entre la consulta y ahora
                    // pudo escribir alguien **aqui**, y su raiz seria mas nueva que la leida.
                    if nueva.generation > v.root().generation && v.store().pendiente().a_salvo() {
                        let store = v.store().clone();
                        *v = Volume::at(store, nueva.clone());
                        drop(v);
                        informar(&nueva);
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

impl Drop for Refrescador {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        if let Some(h) = self.hilo.take() {
            let _ = h.join();
        }
    }
}
