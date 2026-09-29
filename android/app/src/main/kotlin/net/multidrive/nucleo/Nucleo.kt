package net.multidrive.nucleo

import android.content.Context
import android.content.Intent
import android.net.Uri

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
    fun iniciar(contexto: Context) {
        this.contexto = contexto.applicationContext
        iniciarNativo(contexto.applicationContext)
    }

    private external fun iniciarNativo(contexto: Context)

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

    /**
     * Mira si hay algo publicado mas nuevo y se pasa a ello. `true` si cambio.
     *
     * Hace falta porque el movil es un **lector**: mientras mira, otro equipo puede estar
     * escribiendo, y sin esto lo subido desde fuera no aparece nunca.
     *
     * Cuesta una vuelta a la red. Nunca desde el hilo de la interfaz.
     */
    external fun refrescar(sesion: Long): Boolean

    /**
     * Trae la configuracion guardada en las nubes: con **una** cuenta enlazada salen las demas.
     *
     * En un escritorio ahorra unos minutos. En un movil es otra cosa: enlazar cuatro cuentas
     * son cuatro vueltas al navegador y cuatro pantallas de permisos, con el teclado del
     * telefono. Asi es una vuelta y una frase de paso.
     *
     * Bloquea. Nunca desde el hilo de la interfaz.
     */
    external fun recuperar(config: String, frase: String): String

    /**
     * Las cuentas configuradas, en JSON: `["gdrive","onedrive"]`.
     *
     * No toca la red: solo lee el fichero. La pantalla lo pide en cada repintado, asi que tiene
     * que ser barato.
     */
    external fun cuentas(config: String): String

    /** Las entradas de un directorio, en JSON: `[{"nombre":..,"dir":..,"bytes":..}]`. */
    external fun listar(sesion: Long, ruta: String): String

    /** Lo que se sabe de una ruta, en JSON, o la cadena `null` si no existe. */
    external fun mirar(sesion: Long, ruta: String): String

    /**
     * Un trozo de un fichero, por rango.
     *
     * Baja **solo los trozos que el rango toca**, que es lo que permite abrir un video de 2 GiB
     * sin bajarse 2 GiB. Puede devolver menos bytes de los pedidos al llegar al final, igual
     * que `read(2)`.
     */
    external fun leer(sesion: Long, ruta: String, desde: Long, cuanto: Int): ByteArray

    /**
     * Conecta una cuenta de nube por el navegador y la guarda. **Bloquea hasta cinco minutos.**
     *
     * Devuelve el nombre con el que quedo: `gdrive`, `gdrive1`... Nunca desde el hilo de la
     * interfaz: se queda esperando a que alguien de permiso en otra pantalla.
     */
    external fun conectar(tipo: String, config: String): String

    /**
     * El contexto de la aplicacion, para poder abrir el navegador desde Rust.
     *
     * Lo guarda `iniciar`. No es una referencia a una actividad a proposito: guardar una
     * actividad en un objeto que vive todo el proceso es como se filtran pantallas enteras.
     */
    @Volatile
    private var contexto: Context? = null

    /**
     * Abre el navegador. **La llama Rust**, no Kotlin.
     *
     * El flujo de OAuth vive en Rust --el mismo que usan la ventana y el terminal, con PKCE y
     * el `state` comprobado-- y lo unico que no puede hacer es abrir una pantalla. Asi que pide
     * esto.
     *
     * `@JvmStatic` no vale: JNI busca el metodo en la clase que le decimos, y en un `object` de
     * Kotlin los metodos viven en la instancia `INSTANCE` salvo que se marquen asi. Como aqui
     * se llama con `CallStaticVoidMethod`, tiene que ser estatico de verdad.
     */
    @JvmStatic
    fun abrirNavegador(url: String) {
        val c = contexto ?: error("el nucleo no esta inicializado")
        val i = Intent(Intent.ACTION_VIEW, Uri.parse(url))
        // Sin actividad detras hace falta decir que empiece una tarea propia.
        i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        c.startActivity(i)
    }
}
