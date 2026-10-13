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
    external fun abrir(config: String, frase: String, escritura: Boolean): Long

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

    /**
     * Sube un fichero al volumen, leyendolo de una ruta local del telefono.
     *
     * Se pasa una ruta y no los bytes: un fichero puede no caber en memoria, y lo que el
     * sistema entrega al proveedor de documentos llega a trozos y por offsets. Se junta en un
     * temporal y se sube de una vez, que es la unica forma en que el troceado y el reparto ven
     * el fichero entero.
     */
    external fun escribir(sesion: Long, ruta: String, local: String)

    /** Crea un directorio, con los que falten por encima. */
    external fun crearDirectorio(sesion: Long, ruta: String)

    /** Borra un fichero o un directorio vacio. */
    external fun borrar(sesion: Long, ruta: String)

    /** Mueve o renombra. No pisa lo que ya exista en el destino. */
    external fun mover(sesion: Long, origen: String, destino: String)

    /**
     * Busca por nombre en todo el volumen. Devuelve `{incompleto, hallazgos:[...]}`.
     *
     * **Cuesta y puede quedarse corta**: el arbol no tiene indice, asi que buscar es recorrerlo,
     * y recorrerlo es bajar un nodo por directorio. Por eso trae `incompleto`: hay un tope, y
     * quien pinte la lista tiene que poder decir que puede faltar algo.
     *
     * Nunca desde el hilo de la interfaz.
     */
    external fun buscar(sesion: Long, texto: String): String

    /** Lo ultimo que se escribio, en el mismo formato. Cuesta lo mismo, por lo mismo. */
    external fun recientes(sesion: Long, cuantos: Int): String

    /**
     * Lo que se borraria al retirar el volumen, en JSON. **No borra nada.**
     *
     * **Bloquea y tarda**: enumera cada cuenta entera. Nunca desde el hilo de la interfaz.
     */
    external fun mirarRetirada(config: String): String

    /**
     * Retira el volumen de las cuentas: fragmentos, raiz, cabecera y la maleta.
     *
     * `confirmacion` es el nombre de cuenta que dijo [mirarRetirada]. **No se puede deshacer.**
     */
    external fun retirar(config: String, confirmacion: String): String

    /**
     * Corta la espera del navegador que haya en curso. `true` si habia alguna.
     *
     * **No bloquea**: se puede llamar desde el hilo de la interfaz mientras otro hilo esta
     * dentro de [conectar]. Suelta el puerto local, que es lo que deja limpio el camino para
     * el intento siguiente.
     */
    external fun cancelarConexion(): Boolean

    /**
     * Quita de este telefono todo lo de una cuenta. Devuelve una frase para ensenar.
     *
     * **No la saca del volumen**: solo borra unas claves de aqui. Si esa cuenta formaba parte
     * de un volumen, lo que se consigue es que ese volumen ya no abra.
     */
    external fun olvidar(config: String, nombre: String): String

    /**
     * Cuanto cabe en el volumen abierto y cuanto queda, en JSON:
     * `{ total, libre, sin_medir: [...], es_medida }`.
     *
     * `total` y `libre` pueden venir nulos, y **eso no es cero**: significa que ningun
     * proveedor declara cupo, como un bucket S3, que se factura por lo que ocupa y no se
     * llena. `sin_medir` nombra a los que no contestaron o no declaran nada, y con esa lista
     * no vacia las dos cifras son un **techo** y no una medida.
     *
     * Cuesta una peticion por proveedor, en paralelo. Es red: nunca en el hilo de la interfaz,
     * y guardandose la respuesta en vez de pedirla en cada pintada.
     */
    external fun espacio(id: Long): String

    /**
     * Lo que declara cada cuenta enlazada sobre su espacio, en JSON:
     * `[{ nombre, total, usado }]`, o `{ nombre, fallo }` para la que no conteste.
     *
     * `total` y `usado` pueden venir nulos: hay proveedores **sin cupo que declarar**, y eso
     * no es cero, es que no hay barra que pintar.
     *
     * **`usado` es de la cuenta entera, no de MultiDrive.** A quien mira le queda menos sitio
     * por sus fotos tanto como por sus fragmentos, y la barra tiene que decirlo o se lee como
     * «esto ocupa MultiDrive», que es falso y por mucho.
     *
     * No hace falta el volumen abierto. Cuesta una peticion por cuenta, todas a la vez.
     */
    external fun cuotas(config: String): String

    /**
     * Que pasaria si se metieran en el volumen las cuentas enlazadas que no lo forman, **sin
     * tocar nada**. JSON: `{ nuevos: [...], antes, despues, gana, suma }`, o
     * `{ nuevos, antes, problema }` si no se puede.
     *
     * `gana` es `"espacio"` o `"margen"`, y la diferencia no es un matiz: solo un volumen
     * `lineal` crece al anadir una cuenta. En los demas cada trozo se parte entre todos, asi
     * que una cuenta mas da **margen de caidas** y ni un byte de sitio. Quien pinte esto tiene
     * que decirlo con esas palabras: es lo contrario de lo que se espera.
     *
     * Cuesta una peticion por cuenta nueva. Nunca en el hilo de la interfaz.
     */
    external fun mirarAmpliacion(id: Long, config: String): String

    /**
     * Amplia el volumen de verdad. **Tarda**: recorre el arbol y escribe en las cuentas.
     *
     * Al volver, la sesion abierta se queda con la cabecera vieja en la mano: hay que cerrar y
     * volver a abrir el volumen para usarlo con la topologia nueva.
     */
    external fun ampliar(id: Long, config: String): String

    /**
     * Si alguna de las cuentas enlazadas guarda una maleta. No hace falta la frase.
     *
     * **Bloquea**: una peticion por cuenta. Nunca desde el hilo de la interfaz.
     */
    external fun hayMaleta(config: String): Boolean

    /**
     * Las topologias posibles con las cuentas enlazadas, en JSON, con el espacio de cada una.
     *
     * **Bloquea**: pregunta el cupo a cada cuenta. Nunca desde el hilo de la interfaz.
     */
    external fun topologias(config: String): String

    /**
     * Crea el volumen. Devuelve en JSON lo que hay que contar de el.
     *
     * **Bloquea** y cuesta segundos. Nunca desde el hilo de la interfaz.
     */
    external fun crear(config: String, frase: String, spec: String): String

    /** Si esta sesion puede escribir de verdad: se abrio para ello y conserva el permiso. */
    external fun escribe(sesion: Long): Boolean

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
    external fun conectar(tipo: String, config: String, volverA: String): String

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
