package net.multidrive

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import android.util.Log
import android.widget.TextView
import android.widget.Toast
import net.multidrive.nucleo.Nucleo
import java.io.File
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/**
 * Recibe lo que se comparte desde otra aplicacion y lo sube al volumen.
 *
 * # Por que hace falta esto aparte del `DocumentsProvider`
 *
 * Son dos mecanismos distintos de Android y es facil confundirlos, porque desde fuera parecen
 * lo mismo:
 *
 * - El **Storage Access Framework** --el `DocumentsProvider`-- aparece en el selector de
 *   ficheros del sistema: «guardar en», «abrir desde», la app de Archivos. Lo usa una
 *   aplicacion cuando quiere que el usuario **elija un sitio**.
 * - **Compartir** (`ACTION_SEND`) es una lista de *aplicaciones*, no de sitios. Un
 *   `DocumentsProvider` **nunca** sale ahi, por muy bien que funcione.
 *
 * Y compartir es como la gente mete cosas en una nube desde un movil: se abre la galeria, se
 * elige la foto y se comparte. Sin esto, MultiDrive no sale en esa lista y parece que no
 * funciona.
 */
class Compartir : Activity() {

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)

        val texto = TextView(this).apply {
            setPadding(48, 96, 48, 48)
            textSize = 15f
            setBackgroundColor(getColor(R.color.fondo))
            setTextColor(getColor(R.color.texto))
        }
        setContentView(texto)

        val cuales = recibidas()
        if (cuales.isEmpty()) {
            terminar(texto, "No llegó ningún fichero.")
            return
        }

        val s = Sesion.id
        if (s == null) {
            // Aparecer en la lista de compartir y no poder recibir es peor que no aparecer, asi
            // que se dice que hacer en vez de fallar a secas.
            terminar(texto, "El volumen está cerrado.\n\nÁbrelo en MultiDrive, marcando «También quiero escribir en él», y vuelve a compartir.")
            return
        }
        if (!Sesion.escribe) {
            terminar(texto, "El volumen está abierto solo para leer.\n\nCiérralo y vuelve a abrirlo marcando «También quiero escribir en él».")
            return
        }

        texto.text = "Subiendo ${cuales.size} fichero(s)…"
        thread {
            val fallos = mutableListOf<String>()
            var subidos = 0
            for (uri in cuales) {
                try {
                    subir(s, uri)
                    subidos++
                } catch (e: Throwable) {
                    Log.w(ETIQUETA, "no se pudo subir $uri", e)
                    fallos.add(e.message ?: e.toString())
                }
            }
            val dicho = buildString {
                append("Subido(s) $subidos de ${cuales.size}.")
                if (fallos.isNotEmpty()) {
                    append("\n\nNo se pudo con ")
                    append(fallos.size)
                    append(":\n")
                    append(fallos.take(3).joinToString("\n"))
                }
            }
            runOnUiThread { terminar(texto, dicho) }
        }
    }

    /**
     * Copia lo compartido a un temporal y lo sube.
     *
     * El temporal hace falta porque lo que llega es un `Uri` de otra aplicacion --puede ser un
     * flujo, no un fichero-- y el volumen necesita el contenido entero para trocearlo y
     * repartirlo. Se borra al terminar, salga bien o mal: si salio mal, el original sigue en la
     * aplicacion de donde vino.
     */
    private fun subir(sesion: Long, uri: Uri) {
        val nombre = nombreDe(uri)
        val temporal = File.createTempFile("compartido-", ".tmp", cacheDir)
        try {
            contentResolver.openInputStream(uri).use { entrada ->
                if (entrada == null) error("no se pudo leer lo compartido")
                temporal.outputStream().use { salida -> entrada.copyTo(salida) }
            }
            Nucleo.escribir(sesion, "/$nombre", temporal.absolutePath)
            Log.i(ETIQUETA, "subido /$nombre desde compartir")
        } finally {
            temporal.delete()
        }
    }

    /**
     * Como se llama lo que llega.
     *
     * Muchas aplicaciones comparten un `Uri` sin nombre util --`content://.../1234`-- asi que
     * se pregunta por el nombre para mostrar. Si tampoco lo hay, se inventa uno con la hora:
     * un fichero con un nombre feo es recuperable; uno que no se sube porque no sabemos como
     * llamarlo, no.
     */
    private fun nombreDe(uri: Uri): String {
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { c ->
                if (c.moveToFirst()) {
                    val i = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (i >= 0 && !c.isNull(i)) return limpiar(c.getString(i))
                }
            }
        return "compartido-" + System.currentTimeMillis()
    }

    /** Sin barras: el nombre es de un fichero, no una ruta, y una barra lo movería de sitio. */
    private fun limpiar(nombre: String): String =
        nombre.replace('/', '-').replace('\\', '-').ifBlank { "sin-nombre" }

    private fun recibidas(): List<Uri> = when (intent?.action) {
        Intent.ACTION_SEND -> listOfNotNull(
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        )
        Intent.ACTION_SEND_MULTIPLE ->
            intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java) ?: emptyList()
        else -> emptyList()
    }

    private fun terminar(texto: TextView, dicho: String) {
        texto.text = dicho
        Log.i(ETIQUETA, dicho)
        Toast.makeText(this, dicho.lineSequence().first(), Toast.LENGTH_LONG).show()
    }
}
