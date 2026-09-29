package net.multidrive.nucleo

import android.content.Context

/**
 * La unica puerta al nucleo escrito en Rust.
 *
 * Los nombres de estos metodos **no son libres**: JNI los busca por el nombre del simbolo, que
 * se compone del paquete, la clase y el metodo. Cambiar `net.multidrive.nucleo` o el nombre de
 * esta clase obliga a cambiar tambien las funciones de `crates/multidrive-android/src/lib.rs`.
 * No dara error de compilacion: dara `UnsatisfiedLinkError` al llamar, y solo entonces. Hay un
 * test en el lado de Rust que lo vigila.
 */
object Nucleo {
    init {
        // El nombre viene de `[lib] name = "multidrive"` en el Cargo.toml del crate. Android
        // busca `libmultidrive.so` dentro del APK, en la carpeta de la arquitectura del
        // aparato.
        System.loadLibrary("multidrive")
    }

    /**
     * Prepara el proceso. **Antes que nada que use la red.**
     *
     * Le da al verificador de certificados el `Context` de la aplicacion, que es como llega al
     * almacen de confianza del sistema. Sin esta llamada, la primera peticion HTTPS no falla:
     * **entra en panico y cierra la aplicacion**.
     */
    external fun iniciar(contexto: Context)

    /** Version de MultiDrive. */
    external fun version(): String

    /** Describe una topologia calculandola, para probar que el nucleo esta de verdad detras. */
    external fun prueba(): String

    /** Una peticion HTTPS a un sitio publico. Sin credenciales de nadie. */
    external fun https(): String

    /**
     * Abre un volumen y devuelve el identificador de la sesion. **Tarda segundos.**
     *
     * Nunca desde el hilo de la interfaz: seis segundos medidos en un movil, de los cuales
     * solo 156 ms son CPU. Si falla, lanza una excepcion con el motivo.
     *
     * La frase de paso llega como argumento y no se guarda en ningun sitio.
     */
    external fun abrir(config: String, frase: String): Long

    /** Suelta el volumen, y con el la clave derivada. */
    external fun cerrar(sesion: Long)

    /** Las entradas de un directorio, en JSON: `[{"nombre":..,"dir":..,"bytes":..}]`. */
    external fun listar(sesion: Long, ruta: String): String

    /** Lo que se sabe de una ruta, en JSON, o la cadena `null` si no existe. */
    external fun mirar(sesion: Long, ruta: String): String
}
