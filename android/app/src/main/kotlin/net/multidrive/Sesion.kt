package net.multidrive

import android.util.Log
import net.multidrive.nucleo.Nucleo
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/** La autoridad del proveedor de documentos. Una sola, aqui, para que no haya dos que discrepen. */
const val AUTORIDAD = "net.multidrive.documentos"

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
     * Abre el volumen. **Tarda segundos: nunca desde el hilo de la interfaz.**
     *
     * Con `escritura`, coge ademas el testigo del volumen, y **falla si lo tiene otro equipo**.
     * Es deliberado que falle en vez de abrir en solo lectura por su cuenta: quien pidio
     * escribir tiene que enterarse de que no puede, no descubrirlo al intentarlo.
     */
    fun abrir(config: String, frase: String, escritura: Boolean): Long {
        val s = Nucleo.abrir(config, frase, escritura)
        id = s
        resumen = if (escritura) "abierto para leer y escribir" else "abierto para leer"
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
