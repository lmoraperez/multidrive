//! El tamano que se le declara al sistema operativo, medido de fondo.
//!
//! El sistema de ficheros tiene que poder contestar «cuanto cabe aqui» **al instante**: el
//! Explorador lo pregunta cada vez que repinta una ventana y `df` cada vez que se ejecuta.
//! Preguntarselo entonces a los proveedores seria una peticion por nube en cada refresco, o
//! sea segundos de espera en una ventana que deberia pintarse sola. Por eso durante mucho
//! tiempo aqui hubo un numero fijo y enorme, que es lo que hacen los sistemas de ficheros de
//! red cuando no pueden saberlo.
//!
//! Lo que lo arregla no es preguntar mas rapido sino preguntar **en otro sitio**: una tarea
//! de fondo lo mide cada pocos minutos y deja el resultado en dos enteros atomicos. El
//! camino que atiende al sistema operativo solo los lee, sin cerrojos, sin bloquear y sin
//! red.
//!
//! Si todavia no hay medida —los primeros segundos tras montar, o porque ningun proveedor
//! declara cupo— se devuelve `None` y quien llama pone el numero grande de siempre. Eso es
//! deliberado: es mejor un tamano claramente falso que uno de cero, porque un cero significa
//! «disco lleno» y ningun programa volveria a intentar escribir.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use multidrive_core::ChunkStore;

/// Cada cuanto se vuelve a preguntar.
///
/// Cinco minutos porque la cifra se mueve despacio y preguntarla cuesta una peticion por
/// proveedor. Ademas la respuesta del proveedor va por detras de lo que uno acaba de
/// escribir —medido contra OneDrive: un cuarto de hora sin moverse tras subir 20 MiB— asi
/// que preguntar mas a menudo daria la misma cifra gastando mas.
const CADA: Duration = Duration::from_secs(300);

/// Centinela que significa "todavia no se ha medido".
///
/// Se usa `u64::MAX` en vez de un `AtomicBool` aparte porque asi el par (total, libre) se lee
/// con dos cargas independientes sin quedar nunca a medias entre dos estados: o hay medida o
/// no la hay, y se comprueba sobre el mismo entero que se va a usar.
const SIN_MEDIR: u64 = u64::MAX;

/// El tamano del volumen, refrescado de fondo.
///
/// Soltarlo **para la tarea**: el `JoinHandle` se aborta al caer, asi que un desmontaje no
/// deja detras un hilo preguntandole el cupo a unas nubes que ya no le importan a nadie.
pub(crate) struct Capacidad {
    total: Arc<AtomicU64>,
    libre: Arc<AtomicU64>,
    tarea: tokio::task::JoinHandle<()>,
}

impl Drop for Capacidad {
    fn drop(&mut self) {
        self.tarea.abort();
    }
}

impl Capacidad {
    /// Arranca la medida de fondo. La primera pasada va sin esperar, para que el tamano
    /// aparezca en cuanto se abra la unidad y no cinco minutos despues.
    pub(crate) fn arrancar(store: ChunkStore) -> Self {
        let total = Arc::new(AtomicU64::new(SIN_MEDIR));
        let libre = Arc::new(AtomicU64::new(SIN_MEDIR));
        let (t, l) = (Arc::clone(&total), Arc::clone(&libre));

        let tarea = tokio::spawn(async move {
            loop {
                let e = store.espacio().await;
                // Los dos van juntos o no va ninguno: declarar un total sin un libre, o al
                // reves, dejaria al sistema operativo calculando lo ocupado contra un
                // centinela.
                //
                // Y si no se pudo medir, se **conserva** la medida anterior en vez de
                // borrarla: una nube que no contesta durante un minuto no cambia lo que cabe
                // en el volumen, y volver al numero enorme haria bailar el tamano de la
                // unidad a ojos del usuario.
                if let (Some(a), Some(b)) = (e.total, e.libre) {
                    t.store(a, Ordering::Relaxed);
                    l.store(b, Ordering::Relaxed);
                }
                tokio::time::sleep(CADA).await;
            }
        });

        Self {
            total,
            libre,
            tarea,
        }
    }

    /// Total y libre, o `None` si todavia no hay medida.
    ///
    /// No bloquea nunca: son dos cargas atomicas. Se puede llamar desde el hilo que atiende
    /// al sistema operativo.
    pub(crate) fn leer(&self) -> Option<(u64, u64)> {
        let total = self.total.load(Ordering::Relaxed);
        let libre = self.libre.load(Ordering::Relaxed);
        (total != SIN_MEDIR).then_some((total, libre))
    }
}

/// Lo que se declara mientras no hay medida.
///
/// Es un numero deliberadamente absurdo —ocho exabytes— y no una estimacion prudente: quien
/// lo vea tiene que entender que no se sabe, no creerse una cifra inventada. Un valor
/// "razonable" pero falso es peor, porque se confunde con una medida.
pub(crate) const DESCONOCIDO: u64 = u64::MAX / 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_medida_todavia_no_hay_cifra() {
        // El centinela no se puede confundir con un volumen lleno ni con uno vacio.
        let c = Capacidad {
            total: Arc::new(AtomicU64::new(SIN_MEDIR)),
            libre: Arc::new(AtomicU64::new(SIN_MEDIR)),
            tarea: tokio::runtime::Runtime::new().unwrap().spawn(async {}),
        };
        assert_eq!(c.leer(), None);
    }

    #[test]
    fn un_volumen_lleno_se_distingue_de_uno_sin_medir() {
        // Cero libre es una medida, y de las importantes: significa que no se puede escribir.
        // Tiene que llegar como tal y no confundirse con "no se sabe".
        let c = Capacidad {
            total: Arc::new(AtomicU64::new(1000)),
            libre: Arc::new(AtomicU64::new(0)),
            tarea: tokio::runtime::Runtime::new().unwrap().spawn(async {}),
        };
        assert_eq!(c.leer(), Some((1000, 0)));
    }
}
