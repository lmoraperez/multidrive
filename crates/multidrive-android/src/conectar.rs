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

use std::sync::OnceLock;

use jni::{Env, JavaVM};

/// La maquina virtual, para poder volver a Kotlin desde un hilo de Rust.
static VM: OnceLock<JavaVM> = OnceLock::new();

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
pub fn conectar(tipo: &str, config: &str) -> Result<String, String> {
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

    let conectada = crate::sesion::en_el_runtime(async move {
        multidrive_backend::oauth::conectar(oauth, &app.client_id, &app.client_secret, &|url| {
            abrir_navegador(url)
        })
        .await
        .map_err(|e| e.to_string())
    })?;

    let valores = |c: &str| match c {
        "REFRESH_TOKEN" => Some(conectada.refresh_token.clone()),
        _ => None,
    };

    // Se comprueba **antes** de guardar que la cuenta escribe, lee y borra. Guardar un
    // proveedor que no funciona deja al usuario con una configuracion que parece completa y un
    // volumen que no abre, y averiguar cual de las cuentas falla es mucho mas caro despues.
    let backend = live::construir_uno(t, &ranura, &valores).map_err(|e| e.to_string())?;
    crate::sesion::en_el_runtime(async move {
        conformance::prueba_rapida(&backend)
            .await
            .map(|_| ())
            .map_err(|e| format!("no se guarda nada porque el proveedor no responde: {e}"))
    })?;

    multidrive_backend::config::guardar(ruta, &live::claves_de(t, &ranura, &valores))
        .map_err(|e| e.to_string())?;
    Ok(nombre)
}
