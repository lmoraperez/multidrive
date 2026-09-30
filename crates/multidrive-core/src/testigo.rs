//! El testigo de escritura: quien puede escribir en el volumen ahora mismo.
//!
//! # Lo que protege, y lo que no
//!
//! Esto **no es un cerrojo distribuido**, y decirlo importa mas que cualquier detalle de abajo.
//! Un cerrojo de verdad necesita que alguien arbitre, y aqui no hay arbitro: hay N cuentas de
//! nube independientes, sin operacion de «escribe esto solo si nadie lo ha escrito antes». Dos
//! programas pueden leer «libre» a la vez y escribir cada uno el suyo.
//!
//! Lo que si hace, y es lo que hacia falta:
//!
//! - **Evita el accidente normal**, que es el 99 % de los casos: el escritorio tenia el volumen
//!   montado y te habias olvidado, y el movil te lo dice en vez de escribir encima.
//! - **Convierte una perdida silenciosa en un mensaje.** Sin esto, dos escritores publican cada
//!   uno su raiz, gana el ultimo y los ficheros del otro quedan subidos pero sin que nadie los
//!   apunte: se pierden sin error y sin aviso. Eso es lo peor que puede hacer un programa de
//!   copias.
//!
//! Lo que **no** hace: proteger contra dos maquinas que arranquen a escribir en el mismo
//! segundo, ni contra una particion de red, ni contra quien decida ignorarlo a proposito.
//!
//! # Los relojes
//!
//! El testigo caduca por tiempo, y cada maquina mira **su** reloj. Si dos relojes discrepan mas
//! que la duracion del testigo, uno puede creer libre lo que el otro tiene cogido. Por eso la
//! duracion es de minutos y no de segundos: con la hora puesta por red --lo normal- la
//! diferencia es de milisegundos. Cuando se rechaza, se dicen las dos horas, para que un reloj
//! desviado se vea en vez de parecer otra cosa.
//!
//! # Donde vive
//!
//! En **todos** los proveedores del volumen, al lado de la cabecera. Se escribe en todos para
//! que sobreviva a que uno no responda, y se lee de todos: si **cualquiera** dice que lo tiene
//! otro, se respeta. Es la direccion conservadora, que es la que corresponde cuando lo que esta
//! en juego son los datos de alguien.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::join_all;
use multidrive_backend::{Backend, BackendError};
use serde::{Deserialize, Serialize};

use crate::CoreError;

/// La clave donde vive, al lado de la cabecera.
pub const CLAVE: &str = "multidrive-escritor.json";

/// Cuanto dura sin renovar.
///
/// Dos minutos es el equilibrio: lo bastante largo para que una diferencia de relojes normal no
/// cuente, y lo bastante corto para que un programa que muere de golpe no deje el volumen
/// bloqueado mas que un rato.
pub const DURACION: u64 = 120;

/// Cada cuanto conviene renovarlo mientras se escribe.
///
/// Un tercio de la duracion: da margen para dos intentos fallidos antes de que caduque.
pub const RENOVAR_CADA: u64 = DURACION / 3;

/// Quien lo tiene y hasta cuando.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Testigo {
    pub version: u32,
    /// Identificador de **esta sesion**, no de la maquina.
    ///
    /// Se sortea al coger el testigo. No identifica a nadie ni se reutiliza: sirve solo para
    /// que quien lo cogio se reconozca al renovarlo o soltarlo.
    pub quien: String,
    /// Como llamarlo al contarlo: «la ventana», «el terminal», «la aplicacion de Android».
    ///
    /// Se escribe en la nube del propio usuario, asi que no lleva ni el nombre de la maquina ni
    /// nada que identifique a una persona: solo lo justo para que el mensaje sea util.
    pub que: String,
    /// Cuando se cogio, en segundos desde la epoca.
    pub desde: u64,
    /// Hasta cuando vale, en segundos desde la epoca.
    pub hasta: u64,
}

impl Testigo {
    /// Si ya caduco segun **este** reloj.
    pub fn caducado(&self, ahora: u64) -> bool {
        ahora >= self.hasta
    }

    /// Cuanto le queda, en segundos.
    pub fn le_queda(&self, ahora: u64) -> u64 {
        self.hasta.saturating_sub(ahora)
    }
}

/// Por que no se puede escribir.
#[derive(Debug, Clone)]
pub struct Ocupado {
    pub por: Testigo,
    /// El reloj de esta maquina cuando se miro, para poder ver una desviacion.
    pub ahora: u64,
}

impl std::fmt::Display for Ocupado {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "el volumen lo esta escribiendo {} y le quedan {} s.\n\
             Cierra ahi antes de escribir aqui: dos escritores a la vez no dan un error, dan \
             ficheros que se pierden sin avisar.\n\
             (su testigo vale hasta {}; aqui son las {})",
            self.por.que,
            self.por.le_queda(self.ahora),
            self.por.hasta,
            self.ahora
        )
    }
}

fn ahora() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Un identificador de sesion al azar.
///
/// Se reutiliza el generador de sales del cifrado en vez de traer otra caja de numeros al
/// azar: es el mismo generador del sistema y ya esta en el arbol. Aqui no hace falta que sea
/// impredecible, solo que no se repita, pero usar el bueno no cuesta nada.
fn sortear() -> String {
    multidrive_crypto::generate_salt()
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Lee el testigo que haya, quedandose con el que mas tarde caduca.
///
/// Un proveedor que no responde o trae basura se ignora: puede ser una escritura a medias. Lo
/// que no se hace es dar por libre el volumen porque **uno** no conteste.
pub async fn mirar(backends: &[Arc<dyn Backend>]) -> Option<Testigo> {
    let lecturas = backends.iter().map(|b| async move {
        match b.get(CLAVE).await {
            Ok(bytes) => serde_json::from_slice::<Testigo>(&bytes).ok(),
            Err(BackendError::NotFound(_)) => None,
            Err(_) => None,
        }
    });
    join_all(lecturas)
        .await
        .into_iter()
        .flatten()
        .max_by_key(|t| t.hasta)
}

/// Coge el testigo, o dice quien lo tiene.
///
/// `que` es como llamarlo al contarlo: «la ventana», «el terminal», «la aplicacion de Android».
pub async fn coger(backends: &[Arc<dyn Backend>], que: &str) -> Result<Testigo, Ocupado> {
    let n = ahora();
    if let Some(t) = mirar(backends).await
        && !t.caducado(n)
    {
        return Err(Ocupado { por: t, ahora: n });
    }

    let nuestro = Testigo {
        version: 1,
        quien: sortear(),
        que: que.to_string(),
        desde: n,
        hasta: n + DURACION,
    };
    escribir(backends, &nuestro).await;
    Ok(nuestro)
}

/// Lo alarga. Devuelve `false` si **ya no es nuestro**, que es la senal de parar.
///
/// Que deje de serlo pasa de verdad: si esta maquina se queda sin red mas de lo que dura el
/// testigo, otra puede cogerlo legitimamente. Seguir escribiendo entonces es justo lo que esto
/// existe para evitar.
pub async fn renovar(backends: &[Arc<dyn Backend>], nuestro: &mut Testigo) -> bool {
    match mirar(backends).await {
        Some(t) if t.quien != nuestro.quien && !t.caducado(ahora()) => return false,
        _ => {}
    }
    nuestro.hasta = ahora() + DURACION;
    escribir(backends, nuestro).await;
    true
}

/// Lo suelta, para que el siguiente no tenga que esperar a que caduque.
///
/// No falla: si no se puede borrar, caducara solo. Convertir esto en un error dejaria una
/// escritura que fue bien terminando en rojo.
pub async fn soltar(backends: &[Arc<dyn Backend>], nuestro: &Testigo) {
    // Solo si sigue siendo nuestro: borrar el de otro seria dejarle sin proteccion sin que se
    // entere.
    if let Some(t) = mirar(backends).await
        && t.quien != nuestro.quien
    {
        return;
    }
    let borrados = backends
        .iter()
        .map(|b| async move { b.delete(CLAVE).await });
    let _ = join_all(borrados).await;
}

async fn escribir(backends: &[Arc<dyn Backend>], t: &Testigo) {
    let Ok(datos) = serde_json::to_vec(t) else {
        return;
    };
    let escrituras = backends.iter().map(|b| {
        let d = datos.clone();
        async move { b.put(CLAVE, d).await }
    });
    let _ = join_all(escrituras).await;
}

/// Comprueba que la raiz sigue siendo la que creemos **justo antes** de publicar la siguiente.
///
/// Es la red de seguridad, y hace falta aunque haya testigo: el testigo evita el accidente, pero
/// no puede impedir que dos maquinas arranquen a la vez o que alguien lo ignore. Sin esto, el
/// segundo en publicar pisa al primero y los ficheros del primero quedan subidos sin que nadie
/// los apunte: perdidos, sin error.
///
/// Con esto, ese caso da un error que se puede leer y el volumen se queda como estaba.
pub async fn raiz_sin_cambiar(store: &crate::ChunkStore, esperada: u64) -> Result<(), CoreError> {
    let actual = crate::tree::raiz_actual(store).await?;
    match actual {
        Some(r) if r.generation != esperada => Err(CoreError::Rebalanceo(format!(
            "otro escritor publico mientras tanto: el volumen iba por la generacion {esperada} \
             y ahora va por la {}.\nNo se ha publicado nada. Vuelve a abrir el volumen para ver \
             lo que hay ahora.",
            r.generation
        ))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::backends;

    /// Cogerlo dos veces desde sitios distintos no vale. Es la razon de ser de todo esto.
    #[tokio::test]
    async fn dos_no_lo_cogen_a_la_vez() {
        let bs = backends(3);
        let uno = coger(&bs, "el terminal").await.unwrap();

        let otro = coger(&bs, "la aplicacion de Android").await;
        let e = otro.unwrap_err();
        assert_eq!(e.por.quien, uno.quien);
        let dicho = e.to_string();
        assert!(dicho.contains("el terminal"), "{dicho}");
        // El mensaje tiene que decir que hacer, no solo que no se puede.
        assert!(dicho.contains("Cierra ahi"), "{dicho}");
    }

    /// **El caso que de verdad importa**: dos escritores, y el segundo no pisa al primero.
    ///
    /// Sin la comprobacion de antes de publicar, esto termina bien y los ficheros del primero
    /// quedan subidos sin que nadie los apunte: perdidos, sin error. Con ella, el segundo se
    /// encuentra un mensaje y el volumen queda como estaba.
    #[tokio::test]
    async fn el_segundo_en_publicar_no_pisa_al_primero() {
        use multidrive_raid::Layout;

        use crate::ChunkStore;
        use crate::testutil::clave;
        use crate::tree::Volume;

        let bs = backends(2);
        let hacer = || ChunkStore::new(Layout::Mirror { n: 2 }, bs.clone(), clave()).unwrap();

        let (mut primero, _) = Volume::create(hacer()).await.unwrap();

        // Dos programas abren el mismo volumen. Hasta aqui no pasa nada: leer no molesta.
        let mut segundo = Volume::open(hacer()).await.unwrap();

        primero
            .write_file("/del-primero.txt", &b"lo mio"[..])
            .await
            .unwrap();

        // El segundo sigue creyendo que el volumen va por donde lo dejo, y escribe.
        let r = segundo
            .write_file("/del-segundo.txt", &b"lo mio tambien"[..])
            .await;
        let e = r.unwrap_err().to_string();
        assert!(e.contains("otro escritor publico"), "{e}");

        // Y lo del primero sigue estando: no se ha pisado nada.
        let ahora = Volume::open(hacer()).await.unwrap();
        assert!(
            ahora.lookup("/del-primero.txt").await.unwrap().is_some(),
            "se perdio lo del primero"
        );
    }

    /// Soltarlo deja pasar al siguiente sin esperar a que caduque.
    #[tokio::test]
    async fn soltarlo_deja_pasar_al_siguiente() {
        let bs = backends(2);
        let uno = coger(&bs, "la ventana").await.unwrap();
        soltar(&bs, &uno).await;

        let otro = coger(&bs, "el terminal").await;
        assert!(otro.is_ok(), "{:?}", otro.err().map(|e| e.to_string()));
    }

    /// Uno caducado no bloquea: es lo que pasa cuando un programa muere sin soltarlo.
    #[tokio::test]
    async fn uno_caducado_no_bloquea() {
        let bs = backends(2);
        let viejo = Testigo {
            version: 1,
            quien: "otro".into(),
            que: "algo que murio".into(),
            desde: 0,
            hasta: 1,
        };
        escribir(&bs, &viejo).await;

        let nuestro = coger(&bs, "el terminal").await;
        assert!(nuestro.is_ok());
    }

    /// Renovar dice que no cuando otro se lo llevo legitimamente.
    ///
    /// Pasa de verdad: esta maquina se queda sin red mas de lo que dura el testigo, otra lo coge
    /// y esta vuelve creyendo que sigue escribiendo. Seguir entonces es justo lo que hay que
    /// evitar.
    #[tokio::test]
    async fn renovar_avisa_si_ya_no_es_nuestro() {
        let bs = backends(2);
        let mut nuestro = coger(&bs, "la ventana").await.unwrap();

        // Otro lo coge, como si el nuestro hubiera caducado por el camino.
        let del_otro = Testigo {
            version: 1,
            quien: "otro".into(),
            que: "el terminal".into(),
            desde: ahora(),
            hasta: ahora() + DURACION,
        };
        escribir(&bs, &del_otro).await;

        assert!(!renovar(&bs, &mut nuestro).await, "no deberia renovarse");
    }

    /// Soltar el de otro no lo borra: dejarle sin proteccion sin que se entere seria peor que
    /// no soltar nada.
    #[tokio::test]
    async fn no_se_suelta_el_de_otro() {
        let bs = backends(2);
        let del_otro = coger(&bs, "el terminal").await.unwrap();

        let ajeno = Testigo {
            version: 1,
            quien: "no-soy-yo".into(),
            que: "la ventana".into(),
            desde: 0,
            hasta: 0,
        };
        soltar(&bs, &ajeno).await;

        assert_eq!(mirar(&bs).await.map(|t| t.quien), Some(del_otro.quien));
    }
}
