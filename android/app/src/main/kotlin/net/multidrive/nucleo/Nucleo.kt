package net.multidrive.nucleo

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

    /** Version de MultiDrive. */
    external fun version(): String

    /** Describe una topologia calculandola, para probar que el nucleo esta de verdad detras. */
    external fun prueba(): String
}
