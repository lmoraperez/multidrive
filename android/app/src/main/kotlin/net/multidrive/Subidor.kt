package net.multidrive

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import android.util.Log
import net.multidrive.nucleo.Nucleo
import java.io.File
import java.util.Calendar
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/**
 * Sube sola la carpeta elegida, cuando se cumplen las condiciones.
 *
 * # Por que `JobScheduler` y no un hilo nuestro
 *
 * Porque las condiciones que la gente espera --«solo con Wi-Fi», «solo cargando»-- las sabe el
 * sistema y nosotros no, y porque un hilo nuestro muere en cuanto Android decide que la
 * aplicacion sobra. Lo comprobamos en dos telefonos prestados: el proceso no dura.
 *
 * Se usa `JobScheduler` y no `WorkManager` porque este ultimo es androidx, que esta aplicacion
 * no usa, y lo unico que anade por encima es exactamente lo que aqui ya se hace a mano.
 *
 * # Por que de una tanda y no de uno en uno
 *
 * Porque **cada escritura publica una raiz nueva**, que es una vuelta a todas las cuentas. Mil
 * fotos de una en una serian mil publicaciones. Se sube de una en una porque el fichero hay que
 * juntarlo entero antes de trocearlo, pero se avanza con tiempo limitado y se vuelve a programar
 * en vez de intentar vaciar la carpeta en una sola pasada.
 */
class Subidor : JobService() {

    @Volatile
    private var parar = false

    override fun onStartJob(params: JobParameters?): Boolean {
        if (!Carga.activa(this)) return false
        parar = false
        thread {
            val quedan = try {
                subir()
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "la carga automatica fallo", e)
                true
            }
            // `true` como segundo argumento vuelve a programarlo: queda trabajo, o fallo algo y
            // merece otro intento con las condiciones de nuevo cumplidas.
            jobFinished(params, quedan)
        }
        return true
    }

    /** Android nos quita el sitio: se para en cuanto se pueda y se pide volver. */
    override fun onStopJob(params: JobParameters?): Boolean {
        parar = true
        return true
    }

    /**
     * Sube lo que falte. Devuelve `true` si queda trabajo para la proxima.
     *
     * Abre su **propia** sesion y la cierra al terminar. No se reutiliza la del usuario porque
     * puede no haberla: lo normal es que esto corra con la aplicacion cerrada.
     */
    private fun subir(): Boolean {
        val origen = Carga.origen(this)?.let { Uri.parse(it) } ?: return false
        val frase = Llave.abrir(this) ?: run {
            Subidos.anotarParte(this, "Esperando a que desbloquees el teléfono.")
            // Sin llave no se puede escribir. No es un fallo que haya que gritar: pasa cada vez
            // que el telefono esta bloqueado, y lo correcto es volver a intentarlo luego.
            Log.i(ETIQUETA, "carga automatica: sin llave disponible todavia")
            return true
        }

        val conf = File(filesDir, "multidrive.conf").absolutePath
        val sesion = Nucleo.abrir(conf, frase, true)
        try {
            var quedan = false
            var subidos = 0
            var fallados = 0
            var saltados = 0
            val limite = System.currentTimeMillis() + MINUTOS_POR_PASADA * 60_000
            for (f in deLaCarpeta(origen)) {
                if (parar || System.currentTimeMillis() > limite) {
                    quedan = true
                    break
                }
                if (Subidos.ya(this, f.id)) continue
                if (!f.cabeAhora()) { saltados++; continue }
                if (subirUno(sesion, f)) {
                    Subidos.anotar(this, f.id)
                    subidos++
                } else {
                    fallados++
                    quedan = true
                }
            }
            Subidos.anotarParte(this, parteDe(subidos, fallados, saltados, quedan))
            return quedan
        } finally {
            Nucleo.cerrar(sesion)
        }
    }

    /**
     * Lo que paso, en una linea que se pueda leer en Configuracion.
     *
     * Los saltados se nombran a proposito. Son los que no suben porque son anteriores a activar
     * la carga, y es la causa numero uno de «la activé y no pasó nada»: pasa lo correcto, pero
     * desde fuera se ve igual que no hacer nada.
     */
    private fun parteDe(subidos: Int, fallados: Int, saltados: Int, quedan: Boolean): String =
        buildString {
            append(if (subidos == 0) "Nada que subir." else "Subido(s) $subidos.")
            if (fallados > 0) append(" No se pudo con $fallados.")
            if (saltados > 0) {
                append(" $saltados ya estaban en la carpeta antes de activarla: para esos, ")
                append("marca «Subir también lo que ya está».")
            }
            if (quedan) append(" Sigue en otra pasada.")
        }

    /** Un fichero de la carpeta elegida, con lo que hace falta para decidir si sube. */
    private inner class Candidato(
        val id: String,
        val nombre: String,
        val modificado: Long,
    ) {
        /**
         * Si entra en lo que se pidió subir.
         *
         * Sin «subir también lo que ya estaba», solo sube lo que aparezca **a partir de ahora**:
         * activar la carga automática no debe empezar a subir diez años de fotos sin avisar.
         */
        fun cabeAhora(): Boolean =
            Carga.tambienExistentes(this@Subidor) || modificado >= Subidos.desde(this@Subidor)
    }

    /**
     * Lo que hay en la carpeta elegida, sin bajar a subcarpetas.
     *
     * Se usa `DocumentsContract` directamente y no `DocumentFile`, que es androidx: una consulta
     * con su proyección da lo mismo y no añade una dependencia por cuatro columnas.
     */
    private fun deLaCarpeta(arbol: Uri): List<Candidato> {
        val hijos = DocumentsContract.buildChildDocumentsUriUsingTree(
            arbol,
            DocumentsContract.getTreeDocumentId(arbol),
        )
        val salida = mutableListOf<Candidato>()
        contentResolver.query(
            hijos,
            arrayOf(
                DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                DocumentsContract.Document.COLUMN_MIME_TYPE,
                DocumentsContract.Document.COLUMN_LAST_MODIFIED,
            ),
            null, null, null,
        )?.use { c ->
            while (c.moveToNext()) {
                val mime = c.getString(2) ?: ""
                if (mime == DocumentsContract.Document.MIME_TYPE_DIR) continue
                salida.add(Candidato(c.getString(0), c.getString(1) ?: "sin-nombre", c.getLong(3)))
            }
        }
        // De lo más viejo a lo más nuevo: si una pasada no da para todo, lo que lleva más tiempo
        // esperando sube primero. Lo contrario dejaría las fotos antiguas para nunca.
        return salida.sortedBy { it.modificado }
    }

    /** Sube uno. `true` si quedó guardado. */
    private fun subirUno(sesion: Long, f: Candidato): Boolean {
        val arbol = Uri.parse(Carga.origen(this) ?: return false)
        val uri = DocumentsContract.buildDocumentUriUsingTree(arbol, f.id)
        // El temporal hace falta porque el núcleo necesita el fichero entero para trocearlo, y
        // lo que da el sistema es un flujo. Se borra pase lo que pase: son fotos, y llenar el
        // teléfono de copias sería peor que no subirlas.
        val temporal = File.createTempFile("carga-", ".tmp", cacheDir)
        return try {
            contentResolver.openInputStream(uri).use { entrada ->
                if (entrada == null) return false
                temporal.outputStream().use { entrada.copyTo(it) }
            }
            val destino = destinoDe(f)
            Nucleo.escribir(sesion, destino, temporal.absolutePath)
            Cache.olvidar(applicationContext, destino)
            Log.i(ETIQUETA, "carga automatica: subido $destino")
            true
        } catch (e: Throwable) {
            Log.w(ETIQUETA, "carga automatica: no se pudo subir ${f.nombre}", e)
            false
        } finally {
            temporal.delete()
        }
    }

    /** Dónde va dentro del volumen, con subcarpeta por fecha si se pidió. */
    private fun destinoDe(f: Candidato): String {
        val base = Carga.destino(this).trimEnd('/')
        val nombre = f.nombre.replace('/', '-').replace('\\', '-')
        if (!Carga.porFecha(this)) return "$base/$nombre"
        val c = Calendar.getInstance().apply { timeInMillis = f.modificado }
        val mes = (c.get(Calendar.MONTH) + 1).toString().padStart(2, '0')
        return "$base/${c.get(Calendar.YEAR)}/$mes/$nombre"
    }

    companion object {
        /** El identificador del trabajo. Uno solo: dos cargas a la vez se pisarían. */
        private const val TRABAJO = 7401

        /** El de la pasada inmediata. Aparte del periodico: uno no puede ser las dos cosas. */
        private const val AHORA = 7402

        /**
         * Cuánto se trabaja por pasada.
         *
         * `JobScheduler` da unos diez minutos antes de cortar. Se para antes a propósito: que la
         * pasada termine sola y pida otra es mucho más limpio que la maten a mitad de una
         * subida.
         */
        private const val MINUTOS_POR_PASADA = 8L

        /**
         * Programa la carga, o la cancela si está apagada.
         *
         * `setPersisted` la mantiene tras reiniciar el teléfono, que es justo cuando más falta
         * hace: nadie va a abrir la aplicación para que suban las fotos de ayer.
         */
        fun programar(c: Context) {
            val js = c.getSystemService(JobScheduler::class.java) ?: return
            if (!Carga.activa(c)) {
                js.cancel(TRABAJO)
                js.cancel(AHORA)
                return
            }
            // **Se mira lo que devuelve.** Se ignoraba, y por eso una activación que no
            // programaba nada se veía igual que una que sí: la pantalla decía «activa, subirá
            // cuando se cumplan las condiciones» y en el sistema no había ningún trabajo.
            val puesto = try {
                js.schedule(
                    comun(c, TRABAJO)
                        .setPersisted(true)
                        // Cada cuarto de hora como pronto. Más seguido no sube nada antes --las
                        // fotos no aparecen más rápido-- y gasta batería mirando.
                        .setPeriodic(15 * 60_000L)
                        .build()
                ) == JobScheduler.RESULT_SUCCESS
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo programar la carga", e)
                false
            }
            if (!puesto) {
                Subidos.anotarParte(
                    c,
                    "El sistema no aceptó programar la subida automática. Suele ser el ahorro " +
                        "de batería del fabricante: busca MultiDrive en los ajustes de batería " +
                        "y quítale las restricciones.",
                )
                return
            }
            ahora(c)
        }

        /**
         * Una pasada **ya**, en cuanto se cumplan las condiciones.
         *
         * Hace falta porque un trabajo periódico no corre al crearlo: corre dentro de su primer
         * cuarto de hora. Quien activa la carga y mira la carpeta esperando ver algo se
         * encuentra con que no pasa nada, y concluye que no funciona. Pasó.
         *
         * Sin `setOverrideDeadline`: ese forzaría la subida **saltándose** las condiciones, y
         * «solo con Wi-Fi» tiene que seguir significando solo con Wi-Fi aunque haya prisa.
         */
        fun ahora(c: Context) {
            val js = c.getSystemService(JobScheduler::class.java) ?: return
            if (!Carga.activa(c)) return
            val puesto = try {
                js.schedule(comun(c, AHORA).setMinimumLatency(1_000).build()) ==
                    JobScheduler.RESULT_SUCCESS
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo programar la pasada inmediata", e)
                false
            }
            if (!puesto) {
                Subidos.anotarParte(c, "El sistema no aceptó la comprobación. Mira las restricciones de batería de MultiDrive.")
            }
        }

        /** Las condiciones, que son las mismas para la pasada de ahora y para las periódicas. */
        private fun comun(c: Context, id: Int): JobInfo.Builder {
            val red = if (Carga.soloWifi(c)) {
                JobInfo.NETWORK_TYPE_UNMETERED
            } else {
                JobInfo.NETWORK_TYPE_ANY
            }
            return JobInfo.Builder(id, ComponentName(c, Subidor::class.java))
                .setRequiredNetworkType(red)
                .setRequiresCharging(Carga.soloCargando(c))
        }
    }
}

/**
 * Lo que ya se subió, para no volver a subirlo.
 *
 * Se guarda el identificador que da el sistema a cada documento, no el nombre: dos fotos pueden
 * llamarse igual en carpetas distintas, y una renombrada no es una foto nueva.
 */
object Subidos {
    private const val FICHERO = "subidos"
    private const val CLAVE = "ids"
    private const val DESDE = "desde"
    private const val PARTE = "parte"

    private fun p(c: Context) = c.getSharedPreferences(FICHERO, Context.MODE_PRIVATE)

    fun ya(c: Context, id: String) = (p(c).getStringSet(CLAVE, emptySet()) ?: emptySet()).contains(id)

    fun anotar(c: Context, id: String) {
        val todos = HashSet(p(c).getStringSet(CLAVE, emptySet()) ?: emptySet())
        todos.add(id)
        p(c).edit().putStringSet(CLAVE, todos).apply()
    }

    /**
     * Desde cuándo cuenta como «nuevo», en milisegundos.
     *
     * Se fija al activar la carga. Sin esto, encenderla empezaría a subir el carrete entero sin
     * que nadie lo haya pedido, que con datos móviles es una factura y con una cuenta pequeña es
     * llenarla.
     */
    fun desde(c: Context): Long = p(c).getLong(DESDE, 0L)

    fun empiezaAhora(c: Context) {
        p(c).edit().putLong(DESDE, System.currentTimeMillis()).apply()
    }

    /** Al apagar la carga se olvida lo anotado: si se vuelve a encender, se empieza limpio. */
    fun olvidar(c: Context) {
        p(c).edit().remove(CLAVE).remove(DESDE).remove(PARTE).apply()
    }

    /**
     * Que paso en la ultima pasada, para poder decirlo en Configuracion.
     *
     * Existe porque sin esto la carga automatica es invisible: sube o no sube, y desde fuera se
     * ve igual. Quien la activo y no vio nada concluyo que no funcionaba, y tenia razon en no
     * poder saberlo.
     *
     * No se usa una notificacion porque en Android 13 hace falta pedir permiso para ellas, y
     * pedir un permiso para contar algo que cabe en la pantalla donde ya esta mirando quien
     * pregunta seria cobrar de mas.
     */
    fun parte(c: Context): String? = p(c).getString(PARTE, null)

    fun anotarParte(c: Context, dicho: String) {
        p(c).edit().putString(PARTE, dicho).apply()
    }
}
