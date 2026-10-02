package net.multidrive

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.text.Editable
import android.text.TextWatcher
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.BaseAdapter
import android.widget.TextView
import net.multidrive.nucleo.Nucleo
import org.json.JSONArray
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/**
 * El explorador de ficheros del volumen.
 *
 * # Por que la aplicacion tiene el suyo
 *
 * Durante un tiempo esto fue un respaldo y el camino bueno era la app de Archivos del sistema.
 * Se cambio a proposito: el explorador propio es el que puede enseñar lo que solo nosotros
 * sabemos --que una busqueda quedo incompleta, que un fichero es de antes de que hubiera
 * fechas, que algo se esta subiendo-- y el ajeno no.
 *
 * **El proveedor SAF se queda igual.** Es lo que permite que otras aplicaciones abran y guarden
 * en el volumen, y eso no lo sustituye ninguna pantalla nuestra.
 *
 * # Por que el cajon esta escrito a mano
 *
 * Porque `DrawerLayout` es de androidx y esta aplicacion no usa androidx. Son dos vistas y una
 * animacion de desplazamiento; la dependencia costaria mas que las cuarenta lineas.
 */
class Explorador : Activity() {

    private enum class Vista { CARPETAS, FAVORITOS, RECIENTES, BUSQUEDA }

    private lateinit var lista: ListView
    private lateinit var titulo: TextView
    private lateinit var estado: TextView
    private lateinit var buscador: EditText
    private lateinit var cajon: LinearLayout
    private lateinit var sombra: View
    private lateinit var espacio: TextView

    private var vista = Vista.CARPETAS
    private var ruta = "/"
    private var entradas: List<Fichero> = emptyList()

    @Volatile
    private var ocupado = false

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)
        setContentView(montar())
        pedir()
    }

    // ---------------------------------------------------------------- interfaz

    private fun montar(): View {
        val contenido = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(getColor(R.color.fondo))
        }

        contenido.addView(barra())

        titulo = TextView(this).apply {
            textSize = 15f
            setTextColor(getColor(R.color.texto))
            setPadding(40, 24, 40, 4)
        }
        estado = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(40, 0, 40, 12)
        }
        contenido.addView(titulo)
        contenido.addView(estado)

        lista = ListView(this).apply {
            divider = null
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f,
            )
            setOnItemClickListener { _, _, i, _ -> abrir(entradas[i]) }
        }
        contenido.addView(lista)
        contenido.addView(barraInferior())

        // El botón de añadir, flotando sobre la lista y encima de la barra inferior.
        val mas = Button(this).apply {
            text = "+"
            textSize = 22f
            setOnClickListener { menuDeAnadir() }
            layoutParams = FrameLayout.LayoutParams(150, 150).apply {
                gravity = Gravity.BOTTOM or Gravity.END
                setMargins(0, 0, 48, 190)
            }
        }

        sombra = View(this).apply {
            setBackgroundColor(0x99000000.toInt())
            visibility = View.GONE
            setOnClickListener { cajon(false) }
        }
        cajon = cajonLateral()

        return FrameLayout(this).apply {
            addView(contenido)
            addView(mas)
            addView(sombra)
            addView(cajon)
        }
    }

    private fun barra(): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setPadding(24, 48, 24, 8)
        addView(
            Button(this@Explorador).apply {
                text = "☰"
                textSize = 20f
                background = null
                setTextColor(getColor(R.color.texto))
                minWidth = 0
                setOnClickListener { cajon(true) }
            }
        )
        buscador = EditText(this@Explorador).apply {
            hint = "Buscar en MultiDrive"
            textSize = 15f
            setSingleLine()
            setBackgroundResource(R.drawable.campo)
            setPadding(28, 24, 28, 24)
            setTextColor(getColor(R.color.texto))
            setHintTextColor(getColor(R.color.apagado))
            layoutParams =
                LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
            addTextChangedListener(object : TextWatcher {
                override fun afterTextChanged(s: Editable?) {
                    // Buscar recorre el volumen entero, así que **no** se busca en cada tecla:
                    // serían decenas de recorridos para una palabra. Se busca al enviar.
                }

                override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            })
            setOnEditorActionListener { _, _, _ ->
                vista = Vista.BUSQUEDA
                pedir()
                true
            }
        }
        addView(buscador)
    }

    private fun barraInferior(): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        setBackgroundColor(getColor(R.color.panel))
        for ((texto, v) in listOf(
            "Archivos" to Vista.CARPETAS,
            "Favoritos" to Vista.FAVORITOS,
            "Recientes" to Vista.RECIENTES,
        )) {
            addView(
                Button(this@Explorador).apply {
                    text = texto
                    textSize = 12f
                    background = null
                    setTextColor(getColor(R.color.texto))
                    layoutParams =
                        LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
                    setOnClickListener {
                        vista = v
                        if (v == Vista.CARPETAS) ruta = "/"
                        pedir()
                    }
                }
            )
        }
    }

    private fun cajonLateral(): LinearLayout {
        val c = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(getColor(R.color.panel))
            setPadding(40, 80, 40, 40)
            visibility = View.GONE
            layoutParams = FrameLayout.LayoutParams(820, ViewGroup.LayoutParams.MATCH_PARENT)
        }
        c.addView(
            TextView(this).apply {
                text = "MultiDrive"
                textSize = 20f
                setTextColor(getColor(R.color.texto))
                setPadding(0, 0, 0, 32)
            }
        )
        for ((texto, accion) in listOf<Pair<String, () -> Unit>>(
            "Todos los archivos" to { vista = Vista.CARPETAS; ruta = "/"; pedir() },
            "Favoritos" to { vista = Vista.FAVORITOS; pedir() },
            "Archivos recientes" to { vista = Vista.RECIENTES; pedir() },
            "Configuración" to { startActivity(Intent(this, Ajustes::class.java)) },
        )) {
            c.addView(
                Button(this).apply {
                    text = texto
                    textSize = 15f
                    background = null
                    gravity = Gravity.CENTER_VERTICAL or Gravity.START
                    setTextColor(getColor(R.color.texto))
                    setOnClickListener { cajon(false); accion() }
                }
            )
        }
        espacio = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(16, 32, 0, 0)
            text = "Volumen abierto"
        }
        c.addView(espacio)
        return c
    }

    private fun cajon(abierto: Boolean) {
        cajon.visibility = if (abierto) View.VISIBLE else View.GONE
        sombra.visibility = if (abierto) View.VISIBLE else View.GONE
    }

    /** Atrás cierra el cajón, luego sube un directorio, y solo entonces sale. */
    @Deprecated("El reemplazo moderno exige androidx, que esta aplicacion todavia no usa")
    override fun onBackPressed() {
        when {
            cajon.visibility == View.VISIBLE -> cajon(false)
            vista != Vista.CARPETAS -> { vista = Vista.CARPETAS; ruta = "/"; pedir() }
            ruta != "/" -> { ruta = ruta.substringBeforeLast('/').ifEmpty { "/" }; pedir() }
            else -> {
                @Suppress("DEPRECATION")
                super.onBackPressed()
            }
        }
    }

    // ---------------------------------------------------------------- datos

    /** Pide lo que toque según la vista. Siempre fuera del hilo de la interfaz: todo esto es red. */
    private fun pedir() {
        val s = Sesion.id
        titulo.text = when (vista) {
            Vista.CARPETAS -> ruta
            Vista.FAVORITOS -> "Favoritos"
            Vista.RECIENTES -> "Archivos recientes"
            Vista.BUSQUEDA -> "Resultados de «${buscador.text}»"
        }
        if (s == null) {
            estado.text = "El volumen está cerrado. Ábrelo en MultiDrive."
            entradas = emptyList()
            lista.adapter = null
            return
        }
        ocupado = true
        estado.text = "Cargando…"
        thread {
            val (hallado, fallo) = try {
                leer(s) to null
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo leer $vista", e)
                Hallazgos(emptyList(), false) to (e.message ?: e.toString())
            }
            runOnUiThread {
                ocupado = false
                entradas = hallado.ficheros
                estado.text = when {
                    fallo != null -> "No se pudo leer: $fallo"
                    hallado.ficheros.isEmpty() -> vacio()
                    hallado.incompleto ->
                        "${hallado.ficheros.size} resultado(s). Puede faltar alguno: el volumen " +
                            "es grande y la búsqueda se detuvo antes de recorrerlo entero."
                    else -> "${hallado.ficheros.size} elemento(s)"
                }
                lista.adapter = Adaptador()
            }
        }
    }

    private fun vacio(): String = when (vista) {
        Vista.CARPETAS -> "Aquí no hay nada."
        Vista.FAVORITOS ->
            "Sin favoritos. Márcalos desde el menú de cada fichero.\n\n" +
                "Se guardan solo en este teléfono: no los verás desde el escritorio."
        Vista.RECIENTES ->
            "Nada reciente. Los ficheros subidos antes de esta versión no tienen fecha."
        Vista.BUSQUEDA -> "No se encontró nada con ese nombre."
    }

    private fun leer(s: Long): Hallazgos = when (vista) {
        Vista.CARPETAS -> {
            val a = JSONArray(Nucleo.listar(s, ruta))
            val l = (0 until a.length()).map { Fichero.deListado(ruta, a.getJSONObject(it)) }
            Hallazgos(Fichero.ordenar(l), false)
        }
        Vista.BUSQUEDA -> Hallazgos.de(Nucleo.buscar(s, buscador.text.toString()))
        Vista.RECIENTES -> Hallazgos.de(Nucleo.recientes(s, 50))
        Vista.FAVORITOS -> {
            // Los favoritos se guardan por ruta, así que hay que preguntar por cada uno: el que
            // ya no exista se quita en vez de quedarse en una lista que no abre.
            val vivos = mutableListOf<Fichero>()
            for (r in Favoritos.todos(this)) {
                val json = try { Nucleo.mirar(s, r) } catch (e: Throwable) { "null" }
                if (json != "null") {
                    vivos.add(Fichero.deListado(r.substringBeforeLast('/', ""), org.json.JSONObject(json)))
                }
            }
            Favoritos.limpiar(this, vivos.map { it.ruta })
            Hallazgos(Fichero.ordenar(vivos), false)
        }
    }

    // ---------------------------------------------------------------- acciones

    private fun abrir(f: Fichero) {
        if (!f.dir) return
        vista = Vista.CARPETAS
        ruta = f.ruta
        pedir()
    }

    private fun menuDeAnadir() {
        if (Sesion.id == null) return
        AlertDialog.Builder(this)
            .setTitle("Añadir en ${if (vista == Vista.CARPETAS) ruta else "/"}")
            .setItems(arrayOf("Crear carpeta")) { _, _ -> crearCarpeta() }
            .show()
    }

    private fun crearCarpeta() {
        val s = Sesion.id ?: return
        val campo = EditText(this).apply {
            hint = "Nombre de la carpeta"
            setTextColor(getColor(R.color.texto))
        }
        AlertDialog.Builder(this)
            .setTitle("Nueva carpeta")
            .setView(campo)
            .setNegativeButton("Cancelar", null)
            .setPositiveButton("Crear") { _, _ ->
                val nombre = campo.text.toString().trim()
                if (nombre.isEmpty() || nombre.contains('/')) {
                    estado.text = "Un nombre no puede estar vacío ni llevar barras."
                    return@setPositiveButton
                }
                val padre = if (vista == Vista.CARPETAS) ruta else "/"
                val destino = if (padre == "/") "/$nombre" else "$padre/$nombre"
                enSegundoPlano { Nucleo.crearDirectorio(s, destino); Cache.olvidar(this, destino) }
            }
            .show()
    }

    private fun menuDe(f: Fichero) {
        val s = Sesion.id ?: return
        val favorito = Favoritos.es(this, f.ruta)
        val opciones = mutableListOf(
            if (favorito) "Quitar de favoritos" else "Añadir a favoritos",
        )
        if (Sesion.escribe) {
            opciones.add("Renombrar")
            opciones.add("Borrar")
        }
        AlertDialog.Builder(this)
            .setTitle(f.nombre)
            .setItems(opciones.toTypedArray()) { _, i ->
                when (opciones[i]) {
                    "Renombrar" -> renombrar(s, f)
                    "Borrar" -> borrar(s, f)
                    else -> {
                        Favoritos.cambiar(this, f.ruta)
                        pedir()
                    }
                }
            }
            .show()
    }

    private fun renombrar(s: Long, f: Fichero) {
        val campo = EditText(this).apply {
            setText(f.nombre)
            setTextColor(getColor(R.color.texto))
        }
        AlertDialog.Builder(this)
            .setTitle("Renombrar")
            .setView(campo)
            .setNegativeButton("Cancelar", null)
            .setPositiveButton("Renombrar") { _, _ ->
                val nuevo = campo.text.toString().trim()
                if (nuevo.isEmpty() || nuevo.contains('/')) {
                    estado.text = "Un nombre no puede estar vacío ni llevar barras."
                    return@setPositiveButton
                }
                val padre = f.ruta.substringBeforeLast('/', "")
                val destino = if (padre.isEmpty()) "/$nuevo" else "$padre/$nuevo"
                enSegundoPlano {
                    Nucleo.mover(s, f.ruta, destino)
                    Cache.olvidar(this, f.ruta)
                    Cache.olvidar(this, destino)
                }
            }
            .show()
    }

    private fun borrar(s: Long, f: Fichero) {
        AlertDialog.Builder(this)
            .setTitle("¿Borrar «${f.nombre}»?")
            .setMessage(
                if (f.dir) "Solo se puede borrar una carpeta vacía."
                else "No hay papelera: esto no se puede deshacer."
            )
            .setNegativeButton("No", null)
            .setPositiveButton("Borrar") { _, _ ->
                enSegundoPlano { Nucleo.borrar(s, f.ruta); Cache.olvidar(this, f.ruta) }
            }
            .show()
    }

    /** Hace algo que toca la red y vuelve a pedir la lista. El error se enseña, no se traga. */
    private fun enSegundoPlano(trabajo: () -> Unit) {
        ocupado = true
        estado.text = "Un momento…"
        thread {
            val fallo = try {
                trabajo(); null
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo", e)
                e.message ?: e.toString()
            }
            runOnUiThread {
                ocupado = false
                if (fallo == null) pedir() else estado.text = fallo
            }
        }
    }

    // ---------------------------------------------------------------- lista

    private inner class Adaptador : BaseAdapter() {
        override fun getCount() = entradas.size
        override fun getItem(i: Int) = entradas[i]
        override fun getItemId(i: Int) = i.toLong()

        override fun getView(i: Int, reusar: View?, padre: ViewGroup?): View {
            val f = entradas[i]
            val fila = LinearLayout(this@Explorador).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(40, 22, 24, 22)
            }

            fila.addView(
                TextView(this@Explorador).apply {
                    text = if (f.dir) "📁" else "📄"
                    textSize = 20f
                    setPadding(0, 0, 28, 0)
                }
            )

            val medio = LinearLayout(this@Explorador).apply {
                orientation = LinearLayout.VERTICAL
                layoutParams =
                    LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
            }
            medio.addView(
                TextView(this@Explorador).apply {
                    text = f.nombre
                    textSize = 15f
                    setTextColor(getColor(R.color.texto))
                }
            )
            medio.addView(
                TextView(this@Explorador).apply {
                    // El tamaño y, si la hay, la fecha. Sin fecha no se escribe nada: un fichero
                    // de antes de que el árbol las guardara no tiene, y poner una sería mentir.
                    val cuando = cuando(f.modificado)
                    text = buildString {
                        if (!f.dir) append(legible(f.bytes))
                        if (cuando.isNotEmpty()) {
                            if (isNotEmpty()) append(" · ")
                            append(cuando)
                        }
                    }
                    textSize = 12f
                    setTextColor(getColor(R.color.apagado))
                }
            )
            fila.addView(medio)

            if (Favoritos.es(this@Explorador, f.ruta)) {
                fila.addView(
                    TextView(this@Explorador).apply {
                        text = "★"
                        textSize = 14f
                        setTextColor(getColor(R.color.acento))
                        setPadding(0, 0, 16, 0)
                    }
                )
            }
            fila.addView(
                Button(this@Explorador).apply {
                    text = "⋮"
                    textSize = 16f
                    background = null
                    minWidth = 0
                    setTextColor(getColor(R.color.apagado))
                    setPadding(16, 0, 16, 0)
                    setOnClickListener { menuDe(f) }
                }
            )
            return fila
        }
    }
}
