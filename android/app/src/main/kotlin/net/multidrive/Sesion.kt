package net.multidrive

import android.util.Log
import net.multidrive.nucleo.Nucleo
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/**
 * La autoridad del proveedor de documentos.
 *
 * Sale del `applicationId`, igual que en el manifiesto, y no escrita a mano en los dos sitios:
 * una autoridad es **unica por dispositivo**, asi que si estos dos dejaran de coincidir la
 * aplicacion no instalaria, o peor, instalaria y el proveedor no respondria a nadie.
 */
val AUTORIDAD = "${BuildConfig.APPLICATION_ID}.documentos"

/**
 * El identificador de la raiz que publica el proveedor.
 *
 * Esta aqui y no dentro de `Proveedor` porque tambien hace falta fuera: es lo que identifica al
 * volumen en el `Uri` con el que se le pide a la app de Archivos que lo abra.
 */
const val RAIZ = "multidrive"

/**
 * Lo que se enseña mientras el navegador esta abierto dando permiso.
 *
 * # Por que dice que hay que bajar
 *
 * Porque es donde se quedaron las primeras personas que lo probaron. La pantalla de permisos de
 * Google en un movil no cabe entera: se ve el titulo, la cuenta elegida y lo que la aplicacion
 * va a poder hacer, y el boton de continuar queda **debajo del pliegue**. Quien no lo sabe lee
 * lo que hay, no ve ningun boton y se queda esperando a que pase algo, que es exactamente lo
 * que nos contaron: «seleccionó su cuenta y allí se quedó».
 *
 * No es un fallo de Google ni se puede arreglar desde aqui: esa pagina es suya. Lo unico que se
 * puede hacer es decirlo antes, que es barato y funciona.
 *
 * Esta aqui y no copiado en las tres pantallas que conectan cuentas porque ya estaba copiado en
 * las tres, y asi es como una se queda sin arreglar.
 */
const val MIENTRAS_NAVEGADOR =
    "Abriendo el navegador.\n\nElige tu cuenta y, en la pantalla de permisos, " +
        "baja hasta el final y pulsa «Continuar». El botón queda fuera de la pantalla y hay " +
        "que desplazarse para verlo.\n\n" +
        "Si algo sale mal, vuelve aquí y pulsa «Cancelar»: no uses el botón atrás del " +
        "navegador, porque reenvía la misma petición y el proveedor la rechaza."

/**
 * El mensaje de un fallo, sin la envoltura del puente con Rust.
 *
 * El `jni` envuelve lo nuestro en una excepción cuyo texto empieza por «Rust error:». Eso le
 * dice algo a quien programa y nada a quien usa la aplicación, que lo único que ve es una
 * palabra en inglés delante de un mensaje escrito con cuidado en castellano.
 */
fun dicho(e: Throwable): String =
    (dicho(e)).removePrefix("Rust error: ").trim()

/** Bytes en algo que se pueda leer de un vistazo. */
fun legible(bytes: Long): String {
    val u = listOf("B", "KiB", "MiB", "GiB", "TiB")
    var n = bytes.toDouble()
    var i = 0
    while (n >= 1024 && i < u.size - 1) {
        n /= 1024
        i++
    }
    return if (i == 0) "$bytes B" else String.format("%.1f %s", n, u[i])
}

/**
 * El volumen abierto, compartido entre la aplicacion y el proveedor de documentos.
 *
 * Los dos viven en el **mismo proceso**, asi que basta con un objeto. Que sea `object` y no algo
 * que se pasa de mano en mano es a proposito: el `DocumentsProvider` lo crea Android cuando
 * quiere y no le podemos pasar nada al construirlo.
 *
 * Abrir cuesta seis segundos medidos, asi que se abre una vez. Y **Android puede matar el
 * proceso cuando le convenga**: al volver, esto estara vacio y habra que pedir la frase otra
 * vez. No es un fallo que esconder, es como funciona; lo que no se puede es fingir que la
 * sesion es eterna.
 */
object Sesion {
    @Volatile
    var id: Long? = null
        private set

    @Volatile
    var resumen: String = ""
        private set

    /** Si se puede escribir: se abrio para ello y no se ha perdido el permiso. */
    val escribe: Boolean
        get() = id?.let { Nucleo.escribe(it) } ?: false

    /**
     * Si sabemos, **de verdad**, que en las cuentas enlazadas no hay ningun volumen.
     *
     * Vive aqui porque lo enciende una pantalla --la de eliminar-- y lo lee otra. No se puede
     * deducir sin preguntar a las nubes, y preguntar cuesta segundos, asi que solo se enciende
     * cuando algo lo demuestra: un intento de abrir que dijo que no hay ninguno, o una
     * eliminacion que termino.
     */
    @Volatile
    var sinVolumen: Boolean = false

    /**
     * Algo que contar en la pantalla principal cuando se vuelva a ella.
     *
     * Lo deja una pantalla que termina --crear un volumen-- y lo recoge y lo borra la
     * principal. Existe porque el resultado de crear un volumen es lo mas importante que dice
     * esta aplicacion: donde quedo la configuracion sellada, cuantas caidas aguanta. Enseñarlo
     * en una pantalla que acto seguido se cierra seria perderlo.
     */
    @Volatile
    var ultimoAviso: String? = null

    /**
     * Abre el volumen. **Tarda segundos: nunca desde el hilo de la interfaz.**
     *
     * Con `escritura` coge ademas el testigo del volumen, pero **no falla si lo tiene otro
     * equipo**: desde que escribir se reaplica sobre la raiz nueva, dos equipos en ficheros
     * distintos conviven. Negarse ademas seria caro aqui, porque con el volumen montado como
     * servicio en el escritorio el movil no podria escribir nunca.
     */
    fun abrir(config: String, frase: String, escritura: Boolean): Long {
        val s = Nucleo.abrir(config, frase, escritura)
        id = s
        resumen = if (escritura) "abierto para leer y escribir" else "abierto para leer"
        // Si se abrio, haberlo hay.
        sinVolumen = false
        Log.i(ETIQUETA, "sesion $s abierta, escritura=$escritura")
        return s
    }

    /** Suelta el volumen, y con el la clave derivada. */
    fun cerrar() {
        val s = id ?: return
        id = null
        resumen = ""
        Cache.hijos.clear()
        Nucleo.cerrar(s)
        Log.i(ETIQUETA, "sesion $s cerrada")
    }
}

/**
 * Lo ya listado, por ruta.
 *
 * No es una optimizacion: es lo que hace usable la aplicacion. Listar un directorio cuesta mas
 * de un segundo contra cuatro cuentas, y el selector de ficheros pregunta cada vez que se pinta.
 * Sin esto, moverse por el arbol seria esperar en cada paso y volver a esperar al retroceder.
 *
 * Se vacia al cerrar la sesion y al refrescar.
 *
 * Aqui decia antes que no hacia falta caducidad porque «con un solo escritor, lo que hay arriba
 * no cambia por su cuenta mientras el movil mira». **Era falso**, y se vio el primer dia que se
 * uso: un solo escritor significa uno cada vez, no que el lector sea el escritor. El escritorio
 * subio un fichero y en el movil no aparecio nunca; hubo que cerrar el volumen y volver a
 * abrirlo. De ahi salio `refrescar`.
 */
object Cache {
    val hijos = ConcurrentHashMap<String, String>()

    /**
     * Olvida el listado del directorio que contiene `ruta` y avisa al sistema.
     *
     * **Hay que llamarla despues de cada escritura**, venga de donde venga: del selector de
     * ficheros, de compartir una foto, de borrar algo. Escribir cambia el arbol, y el listado
     * que hay guardado es el de antes del cambio.
     *
     * Vive aqui y no en cada sitio que escribe porque lo que se olvida es esto. Tenerlo repetido
     * es tener un camino que se olvida de olvidar, y entonces lo que acabas de subir no aparece
     * y parece que no se subio. Paso exactamente eso con compartir.
     */
    fun olvidar(contexto: android.content.Context, ruta: String) {
        val padre = ruta.substringBeforeLast('/').ifEmpty { "/" }
        hijos.remove(padre)
        contexto.contentResolver.notifyChange(
            android.provider.DocumentsContract.buildChildDocumentsUri(AUTORIDAD, padre),
            null,
        )
    }
}

/**
 * Trae listados en segundo plano y avisa cuando llegan.
 *
 * `queryChildDocuments` tiene que volver **enseguida** --Android llama desde cualquier hilo y
 * la interfaz espera-- asi que devuelve la lista vacia marcada como «cargando» y deja el
 * trabajo aqui. Cuando termina, se notifica el `Uri` y el selector vuelve a preguntar; esta vez
 * lo encuentra en la cache.
 */
object Cargador {
    private val hilos = Executors.newFixedThreadPool(2)
    private val enCurso = ConcurrentHashMap<String, Boolean>()

    /**
     * Pide un listado si no se esta pidiendo ya.
     *
     * Lo de «si no se esta pidiendo ya» no sobra: el selector puede preguntar por el mismo
     * directorio varias veces mientras carga, y sin esto cada una lanzaria su propia tanda de
     * peticiones a cuatro nubes.
     */
    fun pedir(ruta: String, avisar: () -> Unit) {
        if (enCurso.putIfAbsent(ruta, true) != null) return
        hilos.execute {
            try {
                val s = Sesion.id
                if (s != null) {
                    Cache.hijos[ruta] = Nucleo.listar(s, ruta)
                }
            } catch (e: Throwable) {
                // Se anota y no se reintenta solo: un reintento automatico contra cuatro nubes
                // que no contestan es una forma cara de no arreglar nada. El usuario puede
                // volver a entrar en la carpeta.
                Log.w(ETIQUETA, "no se pudo listar $ruta", e)
            } finally {
                enCurso.remove(ruta)
                avisar()
            }
        }
    }
}
