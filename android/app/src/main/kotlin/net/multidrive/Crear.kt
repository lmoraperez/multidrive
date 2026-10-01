package net.multidrive

import android.app.Activity
import android.app.AlertDialog
import android.os.Bundle
import android.text.InputType
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import net.multidrive.nucleo.Nucleo
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/**
 * Crear el volumen desde el telefono.
 *
 * # Por que el movil tambien crea
 *
 * Porque si no, la primera vez hay que ir a buscar un ordenador. Quien instala esto en el
 * telefono ya tiene ahi sus cuentas --son las del propio telefono-- y pedirle un escritorio
 * para empezar es pedirle que no empiece.
 *
 * # Por que la lista se mide antes de ensenarla
 *
 * Porque «aprovechas el 50 %» no dice si te quedan tres gigas o trescientos, y con cuentas
 * desiguales la cifra sorprende: manda la mas apretada, asi que 15 GiB y 3,4 GiB dan 3,4 en
 * espejo y 6,8 en union, no 18,4. Esa sorpresa hay que llevarsela **antes** de crear, porque
 * despues cambiar de topologia es mover todo el volumen.
 *
 * Cuesta una peticion por cuenta, una sola vez al abrir esta pantalla.
 */
class Crear : Activity() {

    private lateinit var estado: TextView
    private lateinit var lista: LinearLayout
    private lateinit var frase: EditText
    private lateinit var otraVez: EditText
    private lateinit var boton: Button
    private lateinit var avisoFrase: TextView
    private lateinit var pasoCuentas: LinearLayout
    private lateinit var cuentas: TextView
    private lateinit var drive: Button
    private lateinit var onedrive: Button
    private lateinit var seguir: Button
    private lateinit var cancelar: Button
    private lateinit var flotante: TextView
    private lateinit var cajaFlotante: LinearLayout
    private lateinit var conf: File

    @Volatile
    private var ocupado = false

    private var opciones: List<Opcion> = emptyList()
    private var elegida: Int = -1

    private data class Opcion(
        val spec: String,
        val tolera: Int,
        val sinRed: Boolean,
        val suma: Boolean,
        val libre: Long?,
        val sinMedir: List<String>,
    )

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)

        val caja = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 72, 48, 48)
            setBackgroundColor(getColor(R.color.fondo))
        }

        caja.addView(
            TextView(this).apply {
                text = "Crear volumen"
                textSize = 20f
                setTextColor(getColor(R.color.texto))
                setPadding(0, 0, 0, 12)
            }
        )
        estado = TextView(this).apply {
            textSize = 14f
            setTextColor(getColor(R.color.texto))
            setPadding(0, 0, 0, 16)
        }
        caja.addView(estado)

        // --- Paso 1: las cuentas. Un volumen es el reparto entre varias, asi que lo primero
        // es tenerlas. Se enlazan **todas** aqui y no de una en una mas tarde: anadir una
        // cuenta a un volumen ya creado mueve datos, y eso hoy solo esta en el escritorio.
        cuentas = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 0, 0, 12)
        }
        drive = Button(this).apply {
            text = "Conectar Google Drive"
            setOnClickListener { conectar("GDRIVE") }
        }
        onedrive = Button(this).apply {
            text = "Conectar OneDrive"
            setOnClickListener { conectar("ONEDRIVE") }
        }
        seguir = Button(this).apply {
            text = "Continuar"
            setOnClickListener { alPasoDos() }
        }
        // La salida mientras se espera al navegador. Sin ella, quien vea que algo se tuerce no
        // tiene mas recurso que el boton atras del navegador, que reenvia la misma peticion y
        // la deja inservible para siempre.
        cancelar = Button(this).apply {
            text = "Cancelar"
            visibility = View.GONE
            setOnClickListener { Nucleo.cancelarConexion() }
        }
        pasoCuentas = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(cuentas)
            addView(drive)
            addView(onedrive)
            addView(seguir)
        }
        caja.addView(pasoCuentas)

        // --- Paso 2: la topologia y la frase.
        lista = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            visibility = View.GONE
        }
        caja.addView(lista)

        frase = EditText(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            )
            hint = "Frase de paso"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            setTextColor(getColor(R.color.texto))
            visibility = View.GONE
        }
        otraVez = EditText(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            )
            hint = "Escríbela otra vez"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            setTextColor(getColor(R.color.texto))
            visibility = View.GONE
        }
        // Dos veces, y no por costumbre: la frase **es** la llave. No se guarda en ningun sitio
        // y no se puede recuperar, asi que una errata en el teclado del movil no da un error mas
        // tarde: da un volumen que ya nunca se abre, con la cabecera escrita en todas las nubes.
        val aviso = TextView(this).apply {
            text = "La frase de paso no se guarda en ninguna parte y no se puede recuperar. " +
                "Si la pierdes, se pierde todo lo que guardes en el volumen."
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 12, 0, 12)
            visibility = View.GONE
        }
        boton = Button(this).apply {
            text = "Crear volumen"
            visibility = View.GONE
            setOnClickListener { confirmar() }
        }
        caja.addView(frase)
        caja.addView(otraVez)
        caja.addView(aviso)
        caja.addView(boton)
        avisoFrase = aviso

        // El aviso va **fuera** del ScrollView, anclado abajo.
        //
        // El texto de estado vive arriba del todo y los botones estan mas abajo, asi que al
        // pulsar «Crear» el mensaje de «creando, son unos segundos» quedaba fuera de la
        // pantalla: habia que subir para saber si estaba pasando algo. Igual que en la
        // pantalla principal, y por lo mismo.
        flotante = TextView(this).apply {
            textSize = 14f
            setTextColor(getColor(R.color.texto))
        }
        cajaFlotante = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundResource(R.drawable.aviso_flotante)
            setPadding(40, 40, 40, 40)
            elevation = 8f
            visibility = View.GONE
            addView(flotante)
            addView(cancelar)
            setOnClickListener { avisar(null) }
            layoutParams = android.widget.FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply {
                gravity = Gravity.BOTTOM
                setMargins(40, 40, 40, 40)
            }
        }
        setContentView(
            android.widget.FrameLayout(this).apply {
                addView(ScrollView(this@Crear).apply { addView(caja) })
                addView(cajaFlotante)
            }
        )

        conf = File(filesDir, "multidrive.conf")
        pintarCuentas(null)
    }

    /**
     * Atras vuelve al paso de las cuentas antes de salir.
     *
     * Sin esto, quien llega a la lista de topologias y se da cuenta de que le falta una cuenta
     * tiene que salir de la pantalla entera y empezar otra vez.
     */
    @Deprecated("El reemplazo moderno exige androidx, que esta aplicacion todavia no usa")
    override fun onBackPressed() {
        if (lista.visibility != View.VISIBLE) {
            @Suppress("DEPRECATION")
            super.onBackPressed()
            return
        }
        lista.visibility = View.GONE
        frase.visibility = View.GONE
        otraVez.visibility = View.GONE
        avisoFrase.visibility = View.GONE
        boton.visibility = View.GONE
        elegida = -1
        pasoCuentas.visibility = View.VISIBLE
        pintarCuentas(null)
    }

    private fun enlazadas(): List<String> = try {
        val a = JSONArray(Nucleo.cuentas(conf.absolutePath))
        (0 until a.length()).map { a.getString(it) }
    } catch (e: Throwable) {
        Log.w(ETIQUETA, "no se pudieron leer las cuentas", e)
        emptyList()
    }

    private fun pintarCuentas(dicho: String?) {
        val hay = enlazadas()
        cuentas.text = if (hay.isEmpty()) {
            "Ninguna todavía."
        } else {
            "${hay.size} enlazada(s): ${hay.joinToString(", ")}"
        }
        drive.isEnabled = !ocupado
        onedrive.isEnabled = !ocupado
        // Con una sola cuenta lo unico que cabe es `concat`, que no reparte nada ni aguanta
        // nada. Se deja pasar igualmente --es un volumen valido y cifrado-- pero el sitio de
        // decir lo que cuesta es la lista, donde sale con su aviso al lado.
        seguir.isEnabled = hay.isNotEmpty() && !ocupado
        cancelar.visibility = if (ocupado) View.VISIBLE else View.GONE

        estado.text = dicho ?: if (hay.isEmpty()) {
            "Conecta las cuentas entre las que quieres repartir tus archivos. Cuantas más, más " +
                "espacio y más caídas aguanta."
        } else {
            "Conecta las que falten y continúa. Después no se pueden añadir desde el móvil."
        }
    }

    private fun conectar(tipo: String) {
        ocupado = true
        pintarCuentas(MIENTRAS_NAVEGADOR)
        thread {
            val dicho = try {
                "Cuenta conectada como «${Nucleo.conectar(tipo, conf.absolutePath)}»."
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo conectar", e)
                e.message ?: e.toString()
            }
            runOnUiThread {
                ocupado = false
                pintarCuentas(dicho)
            }
        }
    }

    private fun alPasoDos() {
        pasoCuentas.visibility = View.GONE
        lista.visibility = View.VISIBLE
        estado.text = "Preguntando a tus cuentas cuánto espacio hay…"
        cargar()
    }

    /** Pregunta a cada cuenta cuanto le queda. Cuesta segundos: nunca en el hilo de la interfaz. */
    private fun cargar() {
        thread {
            val (leidas, fallo) = try {
                leer() to null
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudieron calcular las topologias", e)
                emptyList<Opcion>() to (e.message ?: e.toString())
            }
            runOnUiThread {
                opciones = leidas
                if (fallo != null) {
                    estado.text = "No se pudo preparar la lista: $fallo"
                    return@runOnUiThread
                }
                if (leidas.isEmpty()) {
                    estado.text = "No hay ninguna cuenta enlazada. Conecta una primero."
                    return@runOnUiThread
                }
                estado.text = "Elige cómo se reparten tus archivos entre las cuentas. " +
                    "Esto no se puede cambiar luego desde el móvil."
                pintarLista()
            }
        }
    }

    private fun leer(): List<Opcion> {
        val json = JSONArray(Nucleo.topologias(conf.absolutePath))
        return (0 until json.length()).map { i ->
            val o: JSONObject = json.getJSONObject(i)
            Opcion(
                spec = o.getString("spec"),
                tolera = o.getInt("tolera"),
                sinRed = o.getBoolean("sin_red"),
                suma = o.getBoolean("suma"),
                libre = if (o.isNull("libre")) null else o.getLong("libre"),
                sinMedir = o.getJSONArray("sin_medir").let { a ->
                    (0 until a.length()).map { a.getString(it) }
                },
            )
        }
    }

    private fun pintarLista() {
        lista.removeAllViews()
        for ((i, o) in opciones.withIndex()) {
            lista.addView(tarjeta(i, o))
        }
    }

    private fun tarjeta(i: Int, o: Opcion): View {
        val elegido = i == elegida
        val caja = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(28, 24, 28, 24)
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { bottomMargin = 16 }
            setBackgroundColor(getColor(if (elegido) R.color.linea else R.color.panel))
            setOnClickListener {
                elegida = i
                pintarLista()
                frase.visibility = View.VISIBLE
                otraVez.visibility = View.VISIBLE
                avisoFrase.visibility = View.VISIBLE
                boton.visibility = View.VISIBLE
            }
        }

        caja.addView(
            TextView(this).apply {
                text = o.spec
                textSize = 17f
                setTextColor(getColor(if (elegido) R.color.acento else R.color.texto))
            }
        )
        caja.addView(
            TextView(this).apply {
                // El aviso va con todas las letras. Elegir esto sin enterarse es perder el
                // volumen entero el dia que una cuenta caiga o alguien borre una carpeta.
                text = if (o.sinRed) {
                    "No aguanta perder ninguna cuenta: si falla una, se pierde todo."
                } else {
                    "Aguanta perder ${o.tolera} cuenta(s)."
                }
                textSize = 13f
                setTextColor(getColor(if (o.sinRed) R.color.texto else R.color.apagado))
                setPadding(0, 6, 0, 0)
            }
        )
        caja.addView(
            TextView(this).apply {
                text = espacio(o)
                textSize = 13f
                setTextColor(getColor(R.color.apagado))
                setPadding(0, 4, 0, 0)
            }
        )
        return caja
    }

    /**
     * Lo que da esta opcion, **diciendo cuando la cifra no es una medida**.
     *
     * Una cuenta que no declara cupo --un bucket de S3 se factura por lo que ocupa y no se
     * llena-- deja la cifra coja, y de dos maneras contrarias segun la topologia: donde se
     * parte cada trozo entre todas, manda la mas apretada y lo que sale es un **techo**; donde
     * cada trozo va entero a una cuenta, la cifra es la suma y lo que falta se queda **corto**.
     * Dar el mismo numero en los dos casos sin decir cual es seria dar un numero inventado.
     */
    private fun espacio(o: Opcion): String {
        val libre = o.libre ?: return "No se pudo saber cuánto espacio queda."
        if (o.sinMedir.isEmpty()) return "Espacio disponible: ${legible(libre)}."
        val quienes = o.sinMedir.joinToString(", ")
        return if (o.suma) {
            "Espacio disponible: ${legible(libre)} o más ($quienes no dice cuánto tiene)."
        } else {
            "Espacio disponible: ${legible(libre)} como mucho ($quienes no dice cuánto tiene)."
        }
    }

    /**
     * Pregunta antes de escribir en las nubes.
     *
     * Crear no se deshace desde aqui: deja una cabecera en cada cuenta, y el nucleo se niega a
     * poner otra encima porque una sal nueva dejaria inalcanzable todo lo anterior. Asi que lo
     * que se va a hacer se dice **con su nombre** antes de hacerlo.
     */
    private fun confirmar() {
        val o = opciones.getOrNull(elegida) ?: return
        val f = frase.text.toString()
        if (f.isEmpty()) {
            estado.text = "Escribe una frase de paso."
            return
        }
        if (f != otraVez.text.toString()) {
            estado.text = "Las dos frases no coinciden. Escríbelas otra vez."
            return
        }

        AlertDialog.Builder(this)
            .setTitle("¿Crear el volumen?")
            .setMessage(
                buildString {
                    append("Se creará con «${o.spec}».\n\n")
                    if (o.sinRed) {
                        append("Esta opción no aguanta perder ninguna cuenta: si falla una, se ")
                        append("pierde todo lo que haya en el volumen.\n\n")
                    }
                    append("Cambiar esto después no se puede desde el móvil.")
                }
            )
            .setNegativeButton("No", null)
            .setPositiveButton("Crear") { _, _ -> crear(o.spec, f) }
            .show()
    }

    /** Enseña algo donde se lea sin desplazarse. `null` lo quita. */
    private fun avisar(texto: String?) {
        if (texto.isNullOrEmpty()) {
            cajaFlotante.visibility = View.GONE
            return
        }
        flotante.text = texto
        cajaFlotante.visibility = View.VISIBLE
    }

    private fun crear(spec: String, f: String) {
        boton.isEnabled = false
        val mientras = "Creando el volumen. Son unos segundos: hay que escribir en cada cuenta…"
        estado.text = mientras
        avisar(mientras)

        thread {
            val resumen = try {
                val r = JSONObject(Nucleo.crear(conf.absolutePath, f, spec))
                // Ya hay volumen: la pantalla principal vuelve a ofrecer abrirlo.
                Sesion.sinVolumen = false
                buildString {
                    append("Volumen creado con «${r.getString("topologia")}» en ")
                    append(r.getString("proveedores"))
                    append(".\n\nAguanta perder ${r.getInt("tolera")} cuenta(s).\n\n")
                    append(r.getString("maleta"))
                }
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo crear el volumen", e)
                runOnUiThread { fallo(e.message ?: e.toString()) }
                return@thread
            }
            runOnUiThread {
                // Creado: se vuelve a la pantalla principal y el resumen se cuenta alli.
                //
                // Quedarse aqui seria dejar a alguien en una pantalla de «crear volumen» con el
                // volumen ya creado, teniendo que adivinar que lo siguiente es salir. Y el
                // resumen no se pierde por el camino: dice donde quedo la configuracion
                // sellada, que es lo mas util que esta aplicacion llega a decir.
                Log.i(ETIQUETA, resumen)
                Sesion.ultimoAviso = resumen
                finish()
            }
        }
    }

    /** No se pudo crear: se queda aqui, que es donde se puede volver a intentar. */
    private fun fallo(dicho: String) {
        boton.isEnabled = true
        estado.text = dicho
        avisar(dicho)
    }

}
