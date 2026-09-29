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
