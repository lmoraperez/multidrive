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

pub mod ampliar;
pub mod conectar;
pub mod crear;
pub mod cuentas;
pub mod maleta;
pub mod retirar;
pub mod sesion;

/// Lo que puede salir mal en la frontera, sea de JNI o nuestro.
///
/// Hace falta uno solo porque dentro de cada funcion se mezclan las dos cosas: convertir una
/// cadena de Java falla con un error de JNI, y abrir un volumen falla con un texto nuestro. Al
/// resolver con `ThrowRuntimeExAndDefault`, los dos acaban siendo la misma excepcion de Java,
/// que es lo unico que Kotlin sabe manejar.
#[derive(Debug, thiserror::Error)]
enum Fallo {
    #[error("{0}")]
    Dicho(String),
    #[error(transparent)]
    Jni(#[from] jni::errors::Error),
}

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
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_iniciarNativo<'local>(
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
    // La maquina virtual se guarda aqui porque es el unico sitio donde llega: conectar una
    // cuenta necesita volver a Kotlin para abrir el navegador, y lo hace desde un hilo del
    // runtime que la JVM no conoce.
    conectar::recordar_vm(env);
    rustls_platform_verifier::android::init_with_env(env, contexto)
}

#[cfg(not(target_os = "android"))]
fn iniciar(env: &mut jni::Env<'_>, _contexto: JObject<'_>) -> Result<(), jni::errors::Error> {
    conectar::recordar_vm(env);
    Ok(())
}

/// Conecta una cuenta de nube por el navegador y la guarda. **Bloquea hasta cinco minutos.**
///
/// Devuelve el nombre con el que quedo: `gdrive`, `gdrive1`... Nunca desde el hilo de la
/// interfaz: espera a que alguien de permiso en una pantalla que no es esta.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_conectar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    tipo: JString<'local>,
    config: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let tipo = tipo.try_to_string(env)?;
            let config = config.try_to_string(env)?;
            let nombre = conectar::conectar(&tipo, &config).map_err(Fallo::Dicho)?;
            Ok(env.new_string(nombre)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
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
        // Si ni siquiera se puede armar, eso **es** el resultado del sondeo y no hace falta
        // preguntarle a nadie: significa que este binario no puede hablar por HTTPS.
        let cliente = match multidrive_backend::http::construir(std::time::Duration::from_secs(20))
        {
            Ok(c) => c,
            Err(e) => return format!("no se pudo armar el cliente: {e}"),
        };
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

/// Abre un volumen y devuelve el identificador de la sesion. **Cuesta segundos.**
///
/// Nunca desde el hilo de la interfaz: seis segundos medidos en un movil, de los cuales solo
/// 156 ms son CPU. Si falla, lanza una excepcion con el motivo en vez de devolver cero: un cero
/// se pasa por alto y una excepcion no.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_abrir<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
    frase: JString<'local>,
    escritura: bool,
) -> jni::sys::jlong {
    entorno
        .with_env(|env| -> Result<i64, Fallo> {
            let config = config.try_to_string(env)?;
            let frase = frase.try_to_string(env)?;
            sesion::abrir(&config, &frase, escritura).map_err(Fallo::Dicho)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Las cuentas configuradas en ese fichero, en JSON: `["gdrive","onedrive"]`.
///
/// No toca la red: solo lee el fichero y mira que ranuras estan puestas. La pantalla lo pide
/// cada vez que se repinta, asi que tiene que ser barato.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_cuentas<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            Ok(env.new_string(cuentas(&config))?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Lo mismo sin JNI.
///
/// Un fichero que no existe **no es un error**: es un telefono sin nada configurado todavia, que
/// es el estado normal el primer dia. Se devuelve la lista vacia y la pantalla dice que hay que
/// conectar una cuenta, que es lo util.
pub fn cuentas(config: &str) -> String {
    use multidrive_backend::Backend;

    let nombres: Vec<String> =
        match multidrive_backend::live::configurados_de(std::path::Path::new(config)) {
            Ok(b) => b.iter().map(|b| b.name().to_string()).collect(),
            Err(_) => Vec::new(),
        };
    serde_json::Value::Array(nombres.into_iter().map(serde_json::Value::String).collect())
        .to_string()
}

/// Trae la configuracion guardada en las nubes: con una cuenta enlazada salen las demas.
///
/// Devuelve una frase para ensenar. **Bloquea**: nunca desde el hilo de la interfaz.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_recuperar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
    frase: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let frase = frase.try_to_string(env)?;
            let t = maleta::recuperar(&config, &frase).map_err(Fallo::Dicho)?;

            let mut dicho = format!("Encontrada en '{}'.", t.donde);
            if t.nuevas > 0 {
                dicho.push_str(&format!("\nSe han anadido {} cuenta(s).", t.nuevas));
            } else {
                dicho.push_str("\nNo habia nada que anadir: ya estaba todo.");
            }
            if t.distintas > 0 {
                // Pisar una credencial que funciona con otra de una maleta vieja es la forma
                // mas rapida de quedarse sin acceso a una cuenta, asi que no se toca y se dice.
                dicho.push_str(&format!(
                    "\n{} clave(s) valen aqui otra cosa y NO se han tocado.",
                    t.distintas
                ));
            }
            Ok(env.new_string(dicho)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Sube un fichero al volumen, leyendolo de una ruta local del telefono.
///
/// Se pasa una ruta y no los bytes: un fichero puede no caber en memoria, y lo que el sistema
/// entrega al proveedor de documentos llega a trozos y por offsets. Kotlin lo junta en un
/// temporal y aqui se sube de una vez.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_escribir<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    ruta: JString<'local>,
    local: JString<'local>,
) {
    entorno
        .with_env(|env| -> Result<(), Fallo> {
            let ruta = ruta.try_to_string(env)?;
            let local = local.try_to_string(env)?;
            sesion::escribir(id, &ruta, &local).map_err(Fallo::Dicho)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Crea un directorio en el volumen, con los que falten por encima.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_crearDirectorio<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    ruta: JString<'local>,
) {
    entorno
        .with_env(|env| -> Result<(), Fallo> {
            let ruta = ruta.try_to_string(env)?;
            sesion::crear_directorio(id, &ruta).map_err(Fallo::Dicho)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Borra un fichero o un directorio vacio del volumen.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_borrar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    ruta: JString<'local>,
) {
    entorno
        .with_env(|env| -> Result<(), Fallo> {
            let ruta = ruta.try_to_string(env)?;
            sesion::borrar(id, &ruta).map_err(Fallo::Dicho)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Busca por nombre en todo el volumen, en JSON.
///
/// **Cuesta y puede quedarse corta**: recorre el arbol, y el resultado trae `incompleto` para
/// que la pantalla pueda decir que puede faltar algo. Nunca desde el hilo de la interfaz.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_buscar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    texto: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let texto = texto.try_to_string(env)?;
            let json = sesion::buscar(id, &texto).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Lo ultimo que se escribio, en JSON. Cuesta lo mismo que buscar, por lo mismo.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_recientes<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    cuantos: jni::sys::jint,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let json = sesion::recientes(id, cuantos.max(0) as usize).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Cuanto cabe en el volumen abierto y cuanto queda, en JSON.
///
/// Una peticion por proveedor, no una enumeracion. Ver `sesion::espacio` para lo que significa
/// que `total` o `libre` vengan vacios.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_espacio<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let json = sesion::espacio(id).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Lo que declara cada cuenta enlazada sobre su espacio, en JSON.
///
/// No hace falta el volumen abierto. Ver `cuentas::cuotas` para lo que significa cada campo,
/// y en particular que `usado` es de la cuenta entera y no de MultiDrive.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_cuotas<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let json = cuentas::cuotas(&config).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Que pasaria si se metieran en el volumen las cuentas que no lo forman. No toca nada.
///
/// Devuelve JSON. Puede traer `problema` en vez de un plan, y entonces no se puede ampliar:
/// el motivo esta ahi escrito para ensenarlo tal cual.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_mirarAmpliacion<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    config: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let json = ampliar::mirar(id, &config).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Amplia el volumen de verdad. **Tarda**: recorre el arbol y escribe en las cuentas.
///
/// Al volver, la sesion abierta se queda con la cabecera vieja: hay que cerrar y volver a
/// abrir el volumen.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_ampliar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    config: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let json = ampliar::hacer(id, &config).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Corta la espera del navegador que haya en curso. `true` si habia alguna.
///
/// No bloquea: lo llama el hilo de la interfaz mientras otro esta dentro de `conectar`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_cancelarConexion<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
) -> jni::sys::jboolean {
    entorno
        .with_env(|_env| -> Result<bool, Fallo> { Ok(conectar::cancelar()) })
        .resolve::<ThrowRuntimeExAndDefault>() as jni::sys::jboolean
}

/// Lo que se borraria al retirar el volumen, en JSON. **No borra nada.**
///
/// **Bloquea y tarda**: enumera cada cuenta entera. Nunca desde el hilo de la interfaz.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_mirarRetirada<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let json = retirar::mirar(&config).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Retira el volumen de las cuentas. `confirmacion` es el nombre que dijo `mirarRetirada`.
///
/// **No se puede deshacer.** Bloquea y tarda: nunca desde el hilo de la interfaz.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_retirar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
    confirmacion: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let confirmacion = confirmacion.try_to_string(env)?;
            let dicho = retirar::retirar_volumen(&config, &confirmacion).map_err(Fallo::Dicho)?;
            Ok(env.new_string(dicho)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Quita del fichero todo lo de una cuenta. Devuelve una frase para ensenar.
///
/// **No la saca del volumen**: solo borra unas claves de este telefono. Quien llama tiene que
/// haberlo dicho antes de llegar aqui.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_olvidar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
    nombre: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let nombre = nombre.try_to_string(env)?;
            let dicho = cuentas::olvidar(&config, &nombre).map_err(Fallo::Dicho)?;
            Ok(env.new_string(dicho)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Si alguna de las cuentas enlazadas guarda una maleta. No hace falta la frase.
///
/// **Bloquea**: una peticion por cuenta. Nunca desde el hilo de la interfaz.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_hayMaleta<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
) -> jni::sys::jboolean {
    entorno
        .with_env(|env| -> Result<bool, Fallo> {
            let config = config.try_to_string(env)?;
            cuentas::hay_maleta(&config).map_err(Fallo::Dicho)
        })
        .resolve::<ThrowRuntimeExAndDefault>() as jni::sys::jboolean
}

/// Las topologias posibles con las cuentas enlazadas, en JSON, con el espacio de cada una.
///
/// **Bloquea**: una peticion de cupo por cuenta. Nunca desde el hilo de la interfaz.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_topologias<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let json = crear::topologias(&config).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Crea el volumen. Devuelve en JSON lo que hay que contar de el.
///
/// **Bloquea** y cuesta segundos. Nunca desde el hilo de la interfaz.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_crear<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    config: JString<'local>,
    frase: JString<'local>,
    spec: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let config = config.try_to_string(env)?;
            let frase = frase.try_to_string(env)?;
            let spec = spec.try_to_string(env)?;
            let json = crear::crear(&config, &frase, &spec).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Mueve o renombra. Es la misma operacion vista desde dos sitios.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_mover<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    origen: JString<'local>,
    destino: JString<'local>,
) {
    entorno
        .with_env(|env| -> Result<(), Fallo> {
            let origen = origen.try_to_string(env)?;
            let destino = destino.try_to_string(env)?;
            sesion::mover(id, &origen, &destino).map_err(Fallo::Dicho)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Si esta sesion puede escribir. La pantalla y el proveedor de documentos lo necesitan para
/// no ofrecer lo que no van a poder hacer.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_escribe<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
) -> jni::sys::jboolean {
    entorno
        .with_env(|_env| -> Result<bool, Fallo> {
            Ok(sesion::sesion(id)
                .map(|a| a.escribe() && !a.testigo_perdido())
                .unwrap_or(false))
        })
        .resolve::<ThrowRuntimeExAndDefault>() as jni::sys::jboolean
}

/// Mira si hay algo nuevo publicado y se pasa a ello. `true` si cambio.
///
/// **Cuesta una vuelta a la red**, asi que nunca desde el hilo de la interfaz.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_refrescar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
) -> jni::sys::jboolean {
    entorno
        .with_env(|_env| -> Result<bool, Fallo> { sesion::refrescar(id).map_err(Fallo::Dicho) })
        .resolve::<ThrowRuntimeExAndDefault>() as jni::sys::jboolean
}

/// Suelta el volumen, y con el la clave derivada.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_cerrar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
) {
    entorno
        .with_env(|_env| {
            sesion::cerrar(id);
            Ok::<(), Fallo>(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Las entradas de un directorio, en JSON.
///
/// JSON y no una estructura con campos porque cruzar objetos por JNI cuesta una clase y varios
/// `GetFieldID` por cada uno, y lo que se cruza aqui es una lista de nombres. `org.json` viene
/// con Android, asi que del otro lado no hace falta ninguna dependencia; de este se usa
/// `serde_json`, que ya estaba en el arbol, en vez de escapar cadenas a mano: los nombres de
/// fichero pueden traer comillas y saltos de linea, y escribir ese escapado a mano es la clase
/// de codigo que parece correcto hasta el dia que no lo es.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_listar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    ruta: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let ruta = ruta.try_to_string(env)?;
            let json = listar(id, &ruta).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Lo que se sabe de una ruta, en JSON. `null` si no existe.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_mirar<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    ruta: JString<'local>,
) -> JString<'local> {
    entorno
        .with_env(|env| -> Result<JString<'local>, Fallo> {
            let ruta = ruta.try_to_string(env)?;
            let json = mirar(id, &ruta).map_err(Fallo::Dicho)?;
            Ok(env.new_string(json)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Un trozo de un fichero, por rango.
///
/// Es lo que hace viable abrir un video de 2 GiB sin bajarse 2 GiB: `read_at` baja **solo los
/// trozos que el rango toca**. Del otro lado lo consume un `ProxyFileDescriptorCallback`, que
/// es la unica forma en Android de entregar un descriptor cuyas lecturas nos llegan a nosotros.
///
/// Devuelve menos bytes de los pedidos al final del fichero, que es lo que `read(2)` hace y lo
/// que el callback espera.
#[unsafe(no_mangle)]
pub extern "system" fn Java_net_multidrive_nucleo_Nucleo_leer<'local>(
    mut entorno: EnvUnowned<'local>,
    _clase: JObject<'local>,
    id: jni::sys::jlong,
    ruta: JString<'local>,
    desde: jni::sys::jlong,
    cuanto: jni::sys::jint,
) -> jni::objects::JByteArray<'local> {
    entorno
        .with_env(|env| -> Result<jni::objects::JByteArray<'local>, Fallo> {
            let ruta = ruta.try_to_string(env)?;
            let bytes =
                leer(id, &ruta, desde as u64, cuanto.max(0) as usize).map_err(Fallo::Dicho)?;
            Ok(env.byte_array_from_slice(&bytes)?)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Lo mismo sin JNI, para poder probarlo.
pub fn leer(id: i64, ruta: &str, desde: u64, cuanto: usize) -> Result<Vec<u8>, String> {
    let ruta = ruta.to_string();
    sesion::con(id, move |a| {
        Box::pin(async move {
            a.volumen
                .lock()
                .await
                .read_at(&ruta, desde, cuanto)
                .await
                .map_err(|e| e.to_string())
        })
    })
}

/// Las entradas de un directorio, ya en JSON. Sin JNI, para poder probarlo.
pub fn listar(id: i64, ruta: &str) -> Result<String, String> {
    let ruta = ruta.to_string();
    let entradas = sesion::con(id, move |a| {
        Box::pin(async move {
            a.volumen
                .lock()
                .await
                .read_dir(&ruta)
                .await
                .map_err(|e| e.to_string())
        })
    })?;
    let lista: Vec<serde_json::Value> = entradas
        .iter()
        .map(|(nombre, e)| entrada_json(nombre, e))
        .collect();
    Ok(serde_json::Value::Array(lista).to_string())
}

/// Lo que se sabe de una ruta, ya en JSON.
pub fn mirar(id: i64, ruta: &str) -> Result<String, String> {
    let nombre = ruta.rsplit('/').next().unwrap_or("").to_string();
    let ruta2 = ruta.to_string();
    let entrada = sesion::con(id, move |a| {
        Box::pin(async move {
            a.volumen
                .lock()
                .await
                .lookup(&ruta2)
                .await
                .map_err(|e| e.to_string())
        })
    })?;
    Ok(match entrada {
        Some(e) => entrada_json(&nombre, &e).to_string(),
        None => "null".into(),
    })
}

fn entrada_json(nombre: &str, e: &multidrive_core::tree::Entry) -> serde_json::Value {
    use multidrive_core::tree::Kind;
    serde_json::json!({
        "nombre": nombre,
        "dir": matches!(e.kind, Kind::Dir),
        "bytes": e.len,
        // Segundos desde 1970, o `null` si no se sabe: lo escrito antes de que el arbol
        // guardara fechas no tiene ninguna, y quien lo pinte tiene que enseñar eso en vez de
        // inventarse una.
        "modificado": e.modificado,
    })
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

        for metodo in [
            "version",
            "prueba",
            "iniciarNativo",
            "abrir",
            "cerrar",
            "listar",
            "buscar",
            "recientes",
            "mirar",
            "leer",
            "conectar",
            "cancelarConexion",
            "refrescar",
            "recuperar",
            "cuentas",
            "escribir",
            "crearDirectorio",
            "borrar",
            "mover",
            "topologias",
            "crear",
            "olvidar",
            "hayMaleta",
            "mirarRetirada",
            "retirar",
            "escribe",
        ] {
            let simbolo = format!("{esperado}{metodo}");
            assert!(
                fuente.contains(&simbolo),
                "falta o no coincide el simbolo `{simbolo}`.\n\
                 Si se cambio el paquete de Kotlin, hay que cambiar tambien estas funciones."
            );
        }
    }
}
