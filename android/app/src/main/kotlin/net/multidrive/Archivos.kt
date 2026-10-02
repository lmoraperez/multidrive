package net.multidrive

import android.app.Activity
import android.app.AlertDialog
import android.os.Bundle
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.TextView
import net.multidrive.nucleo.Nucleo
import org.json.JSONArray
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/**
 * Ver lo que hay en el volumen, y borrarlo.
 *
 * # Es el respaldo, no el camino normal
 *
 * Nacio como pantalla propia porque desde la app de Archivos no se podia borrar. Resulto que la
 * culpa no era de ella: se la abria como **selector**, y en ese modo no existen las acciones de
 * borrar ni renombrar. Arreglado eso, el camino normal es la app del sistema, que es donde la
 * gente ya sabe moverse.
 *
 * Esto se queda porque no todos los telefonos traen navegador de documentos. Cuando no lo hay,
 * [Principal] trae aqui: mejor un listado sencillo que un boton que no lleva a ningun sitio.
 *
 * Y hay algo que solo puede hacer esta pantalla: **decir por que** no se pudo borrar. Un gestor
 * ajeno enseña «no se pudo» y se acabo; aqui se puede explicar que el directorio no esta vacio,
 * o que el volumen se abrio solo para leer.
 */
class Archivos : Activity() {

    private lateinit var lista: ListView
    private lateinit var titulo: TextView
    private lateinit var estado: TextView

    /** Donde estamos. La raiz es `/`. */
    private var ruta = "/"
    private var entradas: List<Entrada> = emptyList()

    private data class Entrada(val nombre: String, val dir: Boolean, val bytes: Long)

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)

        val caja = LinearLayout(this).comoPantalla()
        caja.addView(cabecera("Ficheros"))
        titulo = TextView(this).apply {
            textSize = 17f
            setTextColor(getColor(R.color.texto))
            setPadding(16, 0, 16, 8)
        }
        estado = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(16, 0, 16, 12)
        }
        lista = ListView(this).apply { divider = null }

        caja.addView(titulo)
        caja.addView(estado)
        caja.addView(lista)
        setContentView(caja)

        lista.setOnItemClickListener { _, _, i, _ ->
            val e = entradas[i]
            if (e.dir) {
                ruta = hijo(e.nombre)
                cargar()
            }
        }

        cargar()
    }

    /**
     * Atras sube un directorio antes de salir.
     *
     * Sin esto, el boton de atras cierra la pantalla desde cualquier profundidad, que es lo que
     * hace que navegar por carpetas se sienta como un callejon.
     */
    @Deprecated("El reemplazo moderno exige androidx, que esta aplicacion todavia no usa")
    override fun onBackPressed() {
        if (ruta == "/") {
            @Suppress("DEPRECATION")
            super.onBackPressed()
            return
        }
        ruta = ruta.substringBeforeLast('/').ifEmpty { "/" }
        cargar()
    }

    private fun hijo(nombre: String) = if (ruta == "/") "/$nombre" else "$ruta/$nombre"

    private fun cargar() {
        val s = Sesion.id
        titulo.text = ruta
        if (s == null) {
            estado.text = "El volumen está cerrado. Ábrelo en MultiDrive."
            entradas = emptyList()
            lista.adapter = null
            return
        }
        estado.text = "Cargando…"
        // Listar cuesta más de un segundo contra cuatro cuentas: nunca en el hilo de la interfaz.
        thread {
            val (leidas, fallo) = try {
                leer(s) to null
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo listar $ruta", e)
                emptyList<Entrada>() to (e.message ?: e.toString())
            }
            runOnUiThread {
                entradas = leidas
                estado.text = when {
                    fallo != null -> "No se pudo leer: $fallo"
                    leidas.isEmpty() -> "Aquí no hay nada."
                    Sesion.escribe -> "${leidas.size} elemento(s). Pulsa la X para borrar."
                    else -> "${leidas.size} elemento(s). Abierto solo para leer."
                }
                lista.adapter = Adaptador()
            }
        }
    }

    private fun leer(s: Long): List<Entrada> {
        val json = JSONArray(Nucleo.listar(s, ruta))
        // Los directorios primero, y dentro de cada grupo por nombre: es como se espera ver un
        // listado, y el nucleo los devuelve en el orden del arbol, que no es ese.
        return (0 until json.length())
            .map { json.getJSONObject(it) }
            .map { Entrada(it.getString("nombre"), it.getBoolean("dir"), it.getLong("bytes")) }
            .sortedWith(compareByDescending<Entrada> { it.dir }.thenBy { it.nombre.lowercase() })
    }

    private fun borrar(e: Entrada) {
        val s = Sesion.id ?: return
        val destino = hijo(e.nombre)

        // Se pregunta siempre. Borrar de un volumen cifrado no tiene papelera: lo que se va, se
        // fue, y un toque de mas en una lista es facil.
        AlertDialog.Builder(this)
            .setTitle("¿Borrar «${e.nombre}»?")
            .setMessage(
                if (e.dir) "Solo se puede borrar un directorio vacío."
                else "No hay papelera: esto no se puede deshacer."
            )
            .setNegativeButton("No", null)
            .setPositiveButton("Borrar") { _, _ ->
                estado.text = "Borrando…"
                thread {
                    val fallo = try {
                        Nucleo.borrar(s, destino)
                        null
                    } catch (ex: Throwable) {
                        Log.w(ETIQUETA, "no se pudo borrar $destino", ex)
                        ex.message ?: ex.toString()
                    }
                    runOnUiThread {
                        if (fallo == null) {
                            // Lo que se acaba de borrar no puede seguir en el listado que tiene
                            // guardado la app de Archivos.
                            Cache.olvidar(applicationContext, destino)
                            cargar()
                        } else {
                            estado.text = "No se pudo borrar: $fallo"
                        }
                    }
                }
            }
            .show()
    }

    private inner class Adaptador : BaseAdapter() {
        override fun getCount() = entradas.size
        override fun getItem(i: Int) = entradas[i]
        override fun getItemId(i: Int) = i.toLong()

        override fun getView(i: Int, reusar: View?, padre: ViewGroup?): View {
            val e = entradas[i]

            val fila = LinearLayout(this@Archivos).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(16, 20, 16, 20)
            }

            val nombre = TextView(this@Archivos).apply {
                text = if (e.dir) "${e.nombre}/" else e.nombre
                textSize = 15f
                setTextColor(getColor(if (e.dir) R.color.acento else R.color.texto))
                layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
            }
            val tamano = TextView(this@Archivos).apply {
                text = if (e.dir) "" else legible(e.bytes)
                textSize = 13f
                setTextColor(getColor(R.color.apagado))
                setPadding(12, 0, 12, 0)
            }

            fila.addView(nombre)
            fila.addView(tamano)

            // La X solo con permiso de escritura: un botón que no va a funcionar es peor que no
            // tener botón.
            if (Sesion.escribe) {
                fila.addView(
                    Button(this@Archivos).apply {
                        text = "✕"
                        textSize = 14f
                        setTextColor(getColor(R.color.texto))
                        background = null
                        minWidth = 0
                        setPadding(24, 0, 8, 0)
                        setOnClickListener { borrar(e) }
                    }
                )
            }
            return fila
        }
    }

}
