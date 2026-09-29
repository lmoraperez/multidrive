//! La frontera entre Kotlin y el nucleo.
//!
//! Vive aqui y no repartida por el resto porque es la unica parte del proyecto que se mantiene
//! en dos lenguajes a la vez: cada funcion que cruza hay que escribirla dos veces y romperla
//! dos veces. Por eso son pocas y por eso lo que puede quedarse dentro, se queda dentro. El
//! contrato completo y por que es asi esta en `docs/ANDROID.md`.
//!
//! # Los nombres no son libres
//!
//! JNI encuentra las funciones **por el nombre del simbolo**, que se compone de `Java_`, el
//! paquete con los puntos convertidos en barras bajas, la clase y el metodo. Cambiar el paquete
//! en Kotlin sin cambiarlo aqui no da un error de compilacion: da un `UnsatisfiedLinkError` en
//! tiempo de ejecucion, y solo cuando se llame. De ahi la constante de abajo, que no se usa
//! para nada mas que para dejarlo escrito al lado.
//!
//! # Errores
//!
//! Nada de esto puede entrar en panico hacia la JVM: un panico que cruza la frontera es
//! comportamiento indefinido, no una excepcion. `with_env(...).resolve::<ThrowRuntimeExAndDefault>()`
//! recoge tanto los `Err` como los panicos y los convierte en una excepcion de Java, que es lo
//! que Kotlin sabe manejar.

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::JString;
use jni::{EnvUnowned, objects::JObject};

/// El paquete de la clase Kotlin que declara estos metodos, escrito aqui **a proposito**.
///
/// No lo usa el codigo: esta para que quien cambie el paquete en Kotlin vea, al buscarlo, que
/// hay simbolos que cambiar tambien. Es la clase de error que no avisa hasta que un usuario
/// abre la aplicacion.
pub const CLASE: &str = "net.multidrive.nucleo.Nucleo";

/// Prepara el proceso. **Hay que llamarla antes que a nada que use la red.**
///
/// Lo unico que hace es darle al verificador de certificados el `Context` de la aplicacion,
/// que es como llega al almacen de confianza del sistema. Sin esto, la primera peticion HTTPS
/// **no devuelve un error: entra en panico** --`Expect rustls-platform-verifier to be
/// initialized`-- y se lleva el proceso por delante. Comprobado en un telefono antes de
/// escribir esta funcion.
///
/// Se llama desde `onCreate`, no al primer uso, porque un fallo aqui es algo que hay que
/// ensenar entero y no en mitad de una descarga.
///
/// En los demas sistemas no hace nada y devuelve bien: asi el mismo Kotlin vale, y el mismo
/// codigo compila para el escritorio.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_iniciar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    contexto: JObject<'local>,
) {
    entorno
        .with_env(|env| iniciar(env, contexto))
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[cfg(target_os = "android")]
fn iniciar(env: &mut jni::Env<'_>, contexto: JObject<'_>) -> Result<(), jni::errors::Error> {
    rustls_platform_verifier::android::init_with_env(env, contexto)
}

#[cfg(not(target_os = "android"))]
fn iniciar(_env: &mut jni::Env<'_>, _contexto: JObject<'_>) -> Result<(), jni::errors::Error> {
    Ok(())
}

/// Version de MultiDrive, como cadena.
///
/// Es la primera funcion a proposito, y no es de mentira: si esto vuelve con la version
/// correcta, entonces el `.so` se empaqueto, se cargo, se encontro el simbolo y el paso de
/// cadenas funciona. Son cuatro cosas que pueden fallar por separado, y las cuatro fallan de
/// formas que se parecen.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_version<'local>(
    mut entorno: EnvUnowned<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| env.new_string(env!("CARGO_PKG_VERSION")))
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Una peticion HTTPS a un sitio publico, para saber si el verificador ya funciona.
///
/// Sin credenciales de nadie: si el apreton de manos se completa contra un sitio cualquiera,
/// se completara contra una cuenta de nube. Preguntarselo a una cuenta de verdad exigiria
/// tener la configuracion en el telefono para averiguar exactamente lo mismo.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_https<'local>(
    mut entorno: EnvUnowned<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| env.new_string(https()))
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Lo mismo sin JNI. Una linea por sitio.
///
/// Varios sitios y no uno: el verificador de Android comprueba la revocacion del certificado
/// del servidor, y **un certificado sin OCSP ni CRL no se puede comprobar**, lo que sale como
/// `Revoked` aunque no lo este. Let's Encrypt ya no emite ninguna de las dos cosas. Asi que un
/// solo sitio no distingue «el verificador no funciona» de «ese certificado concreto no se
/// puede comprobar», y son dos conclusiones muy distintas.
pub fn https() -> String {
    const SITIOS: &[(&str, &str)] = &[
        ("propio ", "https://multidrive.net/"),
        ("google ", "https://oauth2.googleapis.com/"),
        ("msft   ", "https://login.microsoftonline.com/"),
    ];

    let arranque = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => return format!("no se pudo arrancar el runtime: {e}"),
    };

    arranque.block_on(async {
        // **El del backend**, no uno de aqui. Es el que en Android lleva `webpki-roots` en vez
        // del verificador de la plataforma, y es el que usaran las nubes de verdad: un sondeo
        // con un cliente distinto no probaria lo que hace falta probar.
        let cliente = multidrive_backend::http::construir(std::time::Duration::from_secs(20));
        let mut lineas = Vec::new();
        for (nombre, donde) in SITIOS {
            let t = std::time::Instant::now();
            let dicho = match cliente.get(*donde).send().await {
                Ok(r) => format!("{} en {} ms", r.status(), t.elapsed().as_millis()),
                Err(e) => format!("FALLO: {}", porque(&e)),
            };
            lineas.push(format!("{nombre} {dicho}"));
        }
        lineas.join("\n")
    })
}

/// El error **y de que viene**, hasta el fondo.
///
/// `reqwest` dice «error sending request for url» para cualquier cosa que pase por debajo: un
/// nombre que no resuelve, un permiso que falta, un certificado que no valida y una red que no
/// hay dan todos el mismo texto. Lo que distingue unos de otros esta en la cadena de causas, y
/// sin recorrerla el mensaje no sirve para nada.
fn porque(e: &dyn std::error::Error) -> String {
    let mut partes = vec![e.to_string()];
    let mut causa = e.source();
    while let Some(c) = causa {
        partes.push(c.to_string());
        causa = c.source();
    }
    partes.join(" <- ")
}

/// Abre el volumen y lista la raiz, cronometrando cada paso.
///
/// Es la prueba del paso 3 y **no es la interfaz definitiva**: devuelve texto porque lo que se
/// quiere saber todavia no es como se ensena, sino cuanto tarda. La frontera de verdad
/// --`raices`, `mirar`, `listar`, `leer`-- se escribira encima de esto cuando haya numeros.
///
/// La frase de paso llega como argumento y no se guarda en ningun sitio.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_listar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
    frase: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| {
            let config = config.try_to_string(env)?;
            let frase = frase.try_to_string(env)?;
            env.new_string(listar(&config, &frase))
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Lo mismo sin JNI, para poder ejecutarlo en cualquier sistema.
pub fn listar(config: &str, frase: &str) -> String {
    let arranque = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => return format!("no se pudo arrancar el runtime: {e}"),
    };
    arranque.block_on(abrir_y_listar(config, frase))
}

async fn abrir_y_listar(config: &str, frase: &str) -> String {
    use std::time::Instant;

    use multidrive_backend::Backend;
    use multidrive_core::header;
    use multidrive_core::tree::Volume;

    let mut out = Vec::new();
    let mut paso = |que: &str, ms: u128| out.push(format!("{que:<10} {ms:>6} ms"));

    let backends = match multidrive_backend::live::configurados_de(std::path::Path::new(config)) {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => return "no hay ningun proveedor en esa configuracion".into(),
        Err(e) => return format!("configuracion invalida: {e}"),
    };
    let backends: Vec<std::sync::Arc<dyn Backend>> = backends
        .into_iter()
        .map(|b| std::sync::Arc::new(b) as std::sync::Arc<dyn Backend>)
        .collect();
    let nombres: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();

    let t = Instant::now();
    let cabecera = match header::read(&backends).await {
        Ok(Some(h)) => h,
        Ok(None) => return format!("no hay volumen en {}", nombres.join(", ")),
        Err(e) => return format!("no se pudo leer la cabecera: {e}"),
    };
    paso("cabecera", t.elapsed().as_millis());

    let t = Instant::now();
    let almacen = match cabecera.almacen(backends, frase, 64 * 1024 * 1024) {
        Ok(a) => a,
        Err(e) => return format!("no se pudo derivar la clave: {e}"),
    };
    paso("clave", t.elapsed().as_millis());

    let t = Instant::now();
    let volumen = match Volume::open(almacen).await {
        Ok(v) => v,
        Err(e) => return format!("no se pudo abrir: {e}\n(¿es esa la frase de paso?)"),
    };
    paso("abrir", t.elapsed().as_millis());

    let t = Instant::now();
    let entradas = match volumen.read_dir("/").await {
        Ok(e) => e,
        Err(e) => return format!("no se pudo listar: {e}"),
    };
    paso("listar /", t.elapsed().as_millis());

    out.push(String::new());
    out.push(format!("{} sobre {}", cabecera.layout, nombres.join(", ")));
    out.push(format!("{} entrada(s)", entradas.len()));
    for (nombre, e) in entradas.iter().take(20) {
        out.push(format!("  {nombre}  {}", e.len));
    }
    out.join("\n")
}

/// Que abajo hay un nucleo y no solo una cadena.
///
/// Describe una topologia **calculandola**, no escribiendola: si esto responde bien, el `.so`
/// trae dentro el motor RAID de verdad. Una funcion que devolviera un saludo no distingue
/// «se cargo el nucleo» de «se cargo una biblioteca vacia con el nombre correcto».
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_prueba<'local>(
    mut entorno: EnvUnowned<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| env.new_string(prueba()))
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Lo mismo, sin JNI de por medio, para poder probarlo en cualquier sistema.
///
/// La logica separada de la frontera es la que se comprueba con un test normal. Lo que queda al
/// otro lado --que el simbolo exista y que la JVM lo encuentre-- no se comprueba compilando: se
/// comprueba ejecutando en un telefono.
pub fn prueba() -> String {
    use multidrive_raid::Layout;

    let l: Layout = "rs:4,2".parse().expect("rs:4,2 es valida");
    format!(
        "{l}: tolera perder {}, sobrecoste {:.2}x",
        l.tolerated_failures(),
        l.storage_overhead()
    )
}

/// Marcador para que el `rlib` no quede sin uso en los sistemas donde no hay JVM.
///
/// Compilar esto en Windows y en Linux no es un accidente: es lo que hace que un cambio que
/// rompa la frontera se vea en la CI de siempre y no solo el dia que alguien compile para
/// Android.
#[allow(dead_code)]
fn _no_usado(_: JObject<'_>) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// El nucleo esta de verdad detras, y calcula.
    #[test]
    fn la_prueba_describe_una_topologia_calculandola() {
        let d = prueba();
        assert!(d.starts_with("rs:4,2"), "{d}");
        assert!(d.contains("tolera perder 2"), "{d}");
        assert!(d.contains("1.50x"), "{d}");
    }

    /// Los simbolos y el paquete de Kotlin tienen que decir lo mismo.
    ///
    /// JNI casa las funciones **por el nombre del simbolo**. Cambiar el paquete en Kotlin sin
    /// cambiarlos aqui no rompe ninguna compilacion: rompe la aplicacion en tiempo de
    /// ejecucion, con un `UnsatisfiedLinkError`, y solo cuando se llame a la funcion. Esto lo
    /// convierte en un fallo de `cargo test`, que es donde se ve enseguida.
    #[test]
    fn los_simbolos_corresponden_al_paquete_declarado() {
        let esperado = format!("Java_{}_", CLASE.replace('.', "_"));
        let fuente = include_str!("lib.rs");

        for metodo in ["version", "prueba"] {
            let simbolo = format!("{esperado}{metodo}");
            assert!(
                fuente.contains(&simbolo),
                "falta o no coincide el simbolo `{simbolo}`.\n\
                 Si se cambio el paquete de Kotlin, hay que cambiar tambien estas funciones."
            );
        }
    }
}
