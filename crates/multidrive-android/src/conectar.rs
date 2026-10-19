//! Conectar una cuenta de nube desde el movil.
//!
//! El flujo de OAuth **no se reescribe aqui**: es el mismo `multidrive_backend::oauth` que usan
//! la ventana y la linea de ordenes, con PKCE, el `state` comprobado y el servidor en el bucle
//! local. Lo unico que cambia entre un escritorio y un telefono es **quien abre el navegador**,
//! y eso son tres lineas.
//!
//! Que sea el mismo codigo importa mas de lo que parece: un segundo flujo de OAuth seria un
//! segundo sitio donde olvidarse de comprobar el `state`, que es lo que impide que otro programa
//! de la maquina empuje su propio codigo a nuestro puerto y acabe conectada **su** cuenta.
//!
//! # Como se le pide el navegador a Android
//!
//! Rust no puede abrir una pantalla. Lo que hace es llamar hacia atras a Kotlin, a
//! `Nucleo.abrirNavegador(url)`, que lanza un `Intent`. Para eso hace falta la `JavaVM`, que se
//! guarda al inicializar, y **atar el hilo**: el flujo corre en un hilo del runtime, y un hilo
//! que la JVM no conoce no puede llamar a nada suyo.

use std::sync::{Mutex, OnceLock};

use jni::{Env, JavaVM};

/// La maquina virtual, para poder volver a Kotlin desde un hilo de Rust.
static VM: OnceLock<JavaVM> = OnceLock::new();

/// Cuantas veces ha vuelto a estar delante una pantalla de MultiDrive.
///
/// # Para que sirve contar esto
///
/// Para no canjear el codigo de autorizacion con la aplicacion en segundo plano. A una
/// aplicacion de fondo Android le puede cortar la red --ahorro de bateria, ahorro de datos,
/// una aplicacion marcada como restringida-- y cuando lo hace el sintoma es un fallo de DNS
/// que parece del proveedor. Un tester se comio exactamente eso.
///
/// Antes esto se resolvia con una pausa de mil doscientos milisegundos, confiando en que para
/// entonces el navegador habria devuelto al usuario solo. **No lo hace**: Chrome bloquea el
/// salto a otra aplicacion sin un gesto de por medio, asi que la pausa terminaba con la
/// aplicacion todavia detras y el canje salia de fondo igual que antes.
///
/// Un contador y no una señal suelta porque lo que hay que preguntar es «¿ha vuelto alguien
/// **desde que empece a mirar**?». Un receptor de `watch` se queda con el valor que habia al
/// clonarlo, asi que clonarlo marca el instante de partida sin perderse nada de lo que venga
/// detras, incluso si la vuelta ocurre antes de ponerse a esperar.
static DELANTE: std::sync::LazyLock<(
    tokio::sync::watch::Sender<u64>,
    tokio::sync::watch::Receiver<u64>,
)> = std::sync::LazyLock::new(|| tokio::sync::watch::channel(0));

/// Cuanto se espera a que alguien traiga la aplicacion al frente antes de canjear igual.
///
/// Hay plazo porque quien cierra la pestana sin volver no deberia perder la cuenta que acaba
/// de autorizar: un canje tardio todavia puede salir bien, y de fondo sale bien casi siempre
/// --el corte de red es el caso malo, no el normal--. Lo que no puede haber es una espera sin
/// fondo dejando la pantalla pensando para siempre.
const ESPERA_DELANTE: std::time::Duration = std::time::Duration::from_secs(90);

/// Avisa de que una pantalla de MultiDrive ha vuelto a estar delante.
///
/// Lo llama Kotlin en cada `onActivityResumed`. No bloquea, no falla y no le importa que no
/// haya nadie esperando: si no hay ninguna conexion en curso, esto es un contador que sube.
pub fn delante() {
    DELANTE.0.send_modify(|v| *v += 1);
}

/// Arma la espera y devuelve el futuro que termina cuando la aplicacion este delante.
///
/// Clonar el receptor **aqui**, y no dentro del futuro, es lo que fija el punto de partida en
/// el momento de armar y no en el de empezar a esperar.
fn esperar_delante() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    let mut rx = DELANTE.1.clone();
    Box::pin(async move {
        let _ = tokio::time::timeout(ESPERA_DELANTE, rx.changed()).await;
    })
}

/// Con que cortar la espera del navegador, si hay una en curso.
///
/// # Por que hace falta poder cancelar
///
/// Porque sin ello no hay salida. El flujo espera hasta cinco minutos a que vuelva el
/// permiso, y durante esos minutos la pantalla dice «abriendo el navegador» y nada mas. Quien
/// se encuentre con que algo se tuerce --y en una pantalla de permisos ajena se tuerce-- no
/// puede hacer nada util: volver a pulsar «conectar» no sirve mientras el intento viejo siga
/// vivo, porque el puerto y la peticion siguen siendo los de antes.
///
/// Y hay algo peor que la espera: lo que la gente hace para salir de ella. Pulsar «atras» en
/// el navegador **reenvia la misma peticion de autorizacion**, que Google ya consumio, y
/// responde con un 401 que dice literalmente «no se debe volver a intentar». A partir de ahi
/// todos los intentos fallan por la misma razon, y parece que la aplicacion esta rota.
///
/// Cancelar suelta el puerto y deja el camino limpio: el siguiente intento abre una peticion
/// nueva, que es la unica que Google va a aceptar.
static CANCELAR: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);

/// Si hay una conexion en curso ahora mismo.
///
/// # Por que no puede haber dos
///
/// Porque cada una abre su propio puerto y manda al navegador por su cuenta, y el navegador es
/// uno: la segunda tapa a la primera y deja un puerto escuchando una respuesta que ya no va a
/// llegar. Pero lo que lo hizo evidente fue el cancelador. Al armar el de la segunda se
/// descartaba el de la primera, y **descartar un emisor despierta a su receptor igual que
/// enviarle algo**: la primera se cancelaba sola, en silencio, en el instante en que alguien
/// empezaba otra.
///
/// Se vio en el telefono de quien la estaba probando: dos «conexion cancelada» con 41 ms de
/// diferencia, una desde la pantalla de abrir y otra desde la de crear.
///
/// Negarse es mejor que hacerlas convivir. Dos autorizaciones a la vez no le sirven a nadie, y
/// quien tiene una a medias lo que necesita es terminarla o cancelarla, no empezar otra.
static EN_CURSO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Deja el sitio libre al salir, **por cualquier camino**: bien, mal o cancelada.
///
/// Si esto fuera una linea al final en vez de un `Drop`, el primer `?` que saliera por en medio
/// dejaria la aplicacion creyendo para siempre que hay una conexion en curso, y ningun intento
/// posterior volveria a arrancar.
struct Suelta;

impl Drop for Suelta {
    fn drop(&mut self) {
        *cancelador() = None;
        EN_CURSO.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// El candado, recuperandose de uno envenenado: lo que guarda es un emisor, y un panico ajeno
/// no lo convierte en algo peor que un emisor.
fn cancelador() -> std::sync::MutexGuard<'static, Option<tokio::sync::oneshot::Sender<()>>> {
    CANCELAR.lock().unwrap_or_else(|e| e.into_inner())
}

/// Corta la espera que haya en curso. `true` si habia alguna.
///
/// No bloquea: lo llama el hilo de la interfaz desde el boton de cancelar, mientras otro hilo
/// esta dentro de `conectar` esperando.
pub fn cancelar() -> bool {
    match cancelador().take() {
        Some(tx) => {
            let _ = tx.send(());
            true
        }
        None => false,
    }
}

/// Se guarda al inicializar. Sin esto, conectar una cuenta no puede abrir el navegador.
pub fn recordar_vm(env: &Env<'_>) {
    if let Ok(vm) = env.get_java_vm() {
        let _ = VM.set(vm);
    }
}

/// Le pide a Kotlin que abra el navegador en esa direccion.
///
/// Devuelve `Err` con algo que se pueda leer si no se puede: el flujo de arriba lo convierte en
/// «no se pudo abrir el navegador, abre esta direccion a mano», que es una salida de verdad y no
/// un callejon.
fn abrir_navegador(url: &str) -> Result<(), String> {
    let vm = VM.get().ok_or("el nucleo no esta inicializado")?;
    vm.attach_current_thread(|env: &mut Env| -> Result<(), jni::errors::Error> {
        let clase = env.find_class(jni::jni_str!("net/multidrive/nucleo/Nucleo"))?;
        let url = env.new_string(url)?;
        env.call_static_method(
            &clase,
            jni::jni_str!("abrirNavegador"),
            jni::jni_sig!((java.lang.String) -> void),
            &[(&url).into()],
        )?;
        Ok(())
    })
    .map_err(|e: jni::errors::Error| format!("no se pudo abrir el navegador: {e}"))
}

/// Conecta una cuenta y la deja escrita en el fichero de configuracion que se le diga.
///
/// Devuelve el nombre con el que quedo --`gdrive`, `gdrive1`...-- que es lo que el usuario va a
/// ver. Bloquea hasta cinco minutos esperando a que se de permiso en el navegador: **nunca desde
/// el hilo de la interfaz**.
pub fn conectar(tipo: &str, config: &str, volver_a: &str) -> Result<String, String> {
    use multidrive_backend::{aplicacion, catalogo, conformance, live};

    let tipo = tipo.to_uppercase();
    let t = catalogo::tipo(&tipo).ok_or_else(|| format!("no conozco el tipo '{tipo}'"))?;
    let oauth = t
        .oauth
        .ok_or_else(|| format!("'{}' no se conecta por navegador", t.etiqueta))?;
    let app = aplicacion::configurada(&tipo).ok_or_else(|| {
        format!(
            "esta copia no trae aplicacion registrada para {}",
            t.etiqueta
        )
    })?;

    let ruta = std::path::Path::new(config);
    let ranura = live::hueco_libre_de(t, ruta)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no quedan ranuras libres de tipo {}", t.clave))?;
    let nombre = live::nombre_de_ranura(&ranura);

    // Una sola a la vez. Se reserva el sitio **antes** de tocar nada, y el guardia lo libera
    // salga por donde salga.
    if EN_CURSO.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(
            "ya hay una conexión en curso.\n\nTermínala en el navegador, o pulsa «Cancelar» \
             y vuelve a intentarlo."
                .into(),
        );
    }
    let _suelta = Suelta;

    let (tx, rx) = tokio::sync::oneshot::channel();
    *cancelador() = Some(tx);

    let conectada = crate::sesion::en_el_runtime(async move {
        let flujo = multidrive_backend::oauth::conectar(
            oauth,
            &app.client_id,
            &app.client_secret,
            &|url| abrir_navegador(url),
            // Vacio significa que no hay a donde volver: asi es como el escritorio lo pide,
            // y asi tiene que poder pedirlo tambien quien llame desde Kotlin sin enlace.
            if volver_a.is_empty() {
                None
            } else {
                Some(multidrive_backend::oauth::Vuelta {
                    enlace: volver_a,
                    delante: &esperar_delante,
                })
            },
        );
        tokio::select! {
            r = flujo => r.map_err(|e| e.to_string()),
            // Al salir por aqui se suelta `flujo`, y con el el puerto local. Eso es lo que
            // deja el camino limpio para el intento siguiente.
            _ = rx => Err("conexión cancelada".to_string()),
        }
    });
    let conectada = conectada?;

    let valores = |c: &str| match c {
        "REFRESH_TOKEN" => Some(conectada.refresh_token.clone()),
        _ => None,
    };

    // Se comprueba **antes** de guardar que la cuenta escribe, lee y borra. Guardar un
    // proveedor que no funciona deja al usuario con una configuracion que parece completa y un
    // volumen que no abre, y averiguar cual de las cuentas falla es mucho mas caro despues.
    let backend = live::construir_uno(t, &ranura, &valores).map_err(|e| e.to_string())?;
    let etiqueta = t.etiqueta;
    let ruta_dup = ruta.to_path_buf();
    crate::sesion::en_el_runtime(async move {
        conformance::prueba_rapida(&backend)
            .await
            .map_err(|e| format!("no se guarda nada porque el proveedor no responde: {e}"))?;

        // Y que no sea una cuenta que ya esta. **Es la comprobacion que evita prometer una
        // redundancia que no existe**: dos ranuras sobre la misma cuenta se ven como dos, se
        // cuentan como dos al elegir topologia y se anuncian como «aguanta perder una», pero
        // el dia que se pierda esa cuenta se pierden las dos a la vez.
        //
        // Faltaba aqui y estaba en el escritorio. Se vio enlazando dos veces la misma cuenta
        // de Google desde el movil, que las acepto sin decir nada.
        match live::ya_configurada_de(t, &backend, &ruta_dup).await {
            Ok(Some(otro)) => Err(format!(
                "esa cuenta de {etiqueta} ya está enlazada como «{otro}».\n\n\
                 Dos cuentas iguales no son dos: se verían como dos al repartir, pero el día \
                 que se pierda esa cuenta se pierden las dos copias a la vez. No se ha \
                 guardado nada.\n\n\
                 Enlaza otra cuenta distinta."
            )),
            Ok(None) => Ok(()),
            // No poder comprobarlo no bloquea: negarse porque otro proveedor esta caido seria
            // peor que el riesgo que se intenta evitar. Pero se dice, porque la proxima cifra
            // de «aguanta perder una» se habra calculado sin saberlo.
            Err(_) => Ok(()),
        }
    })?;

    multidrive_backend::config::guardar(ruta, &live::claves_de(t, &ranura, &valores))
        .map_err(|e| e.to_string())?;
    Ok(nombre)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cancelar sin nada en curso dice que no, y un cancelador armado se consume una sola vez.
    ///
    /// Va todo en una prueba **a proposito**: `CANCELAR` es global, y dos pruebas tocandolo a
    /// la vez --que es como las corre cargo-- se pisarian y fallarian un dia de cada diez.
    ///
    /// Lo que se comprueba importa por el boton: existe mientras la pantalla cree que se esta
    /// esperando, y esa creencia puede llegar tarde, porque el intento pudo terminar solo un
    /// instante antes. Devolver `false` es lo que permite distinguir «lo he cortado yo» de «ya
    /// habia terminado». Y que no se quede armado es lo que impide que el boton de un intento
    /// corte el siguiente, un fallo imposible de entender desde fuera: se pulsa en una
    /// pantalla y se rompe otra.
    #[test]
    fn el_cancelador_se_arma_y_se_consume_una_sola_vez() {
        assert!(!cancelar(), "sin nada en curso no hay nada que cortar");

        let (tx, _rx) = tokio::sync::oneshot::channel();
        *cancelador() = Some(tx);
        assert!(cancelar(), "lo armado se puede cancelar");
        assert!(!cancelar(), "y no se queda puesto para la vez siguiente");
    }

    /// El guardia libera el sitio salga por donde salga.
    ///
    /// Sin esto, el primer error que saliera por en medio dejaria la aplicacion creyendo para
    /// siempre que hay una conexion en curso, y ningun intento posterior volveria a arrancar:
    /// un fallo que solo se arregla cerrando la aplicacion.
    #[test]
    fn el_sitio_se_libera_aunque_se_salga_por_un_error() {
        use std::sync::atomic::Ordering;

        assert!(!EN_CURSO.swap(true, Ordering::SeqCst), "estaba libre");
        {
            let _suelta = Suelta;
            // Aqui dentro se sale como sea: por error, por panico atrapado o por el final.
        }
        assert!(!EN_CURSO.load(Ordering::SeqCst), "y vuelve a estarlo");
    }
}
