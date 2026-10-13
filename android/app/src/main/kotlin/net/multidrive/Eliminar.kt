package net.multidrive

import android.app.Activity
import android.os.Bundle
import android.text.InputType
import android.util.Log
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
 * Eliminar el volumen: borrar sus fragmentos, su raiz y su cabecera de todas las cuentas.
 *
 * # Por que se cuenta antes de preguntar
 *
 * Porque «se va a borrar el volumen» no es una cantidad. Enumerar cada cuenta cuesta, y aun asi
 * se hace antes de ofrecer el boton: la cifra de lo que hay dentro es lo unico que convierte
 * esto en una decision en vez de un impulso.
 *
 * # Por que se escribe el nombre de una cuenta
 *
 * Es la misma confirmacion que pide el escritorio. Un boton de «sí» se pulsa sin leer; escribir
 * un nombre obliga a mirar la lista. **Y es una sola cuenta, la que toque por posicion**: pedir
 * la lista entera crecia con el numero de cuentas, y a partir de unas pocas se copiaba y se
 * pegaba sin leerla, que es justo lo contrario de lo que una confirmacion busca.
 *
 * # Lo que no se borra
 *
 * Solo lo que es del volumen. Si hay otras cosas guardadas en esa misma carpeta se quedan donde
 * estan. Tambien se va la configuracion sellada --la maleta-- y se dice antes: sin volumen no
 * abre nada, y dejarla seria no devolver la cuenta a como estaba.
 */
class Eliminar : Activity() {

    private lateinit var estado: TextView
    private lateinit var detalle: TextView
    private lateinit var instruccion: TextView
    private lateinit var campo: EditText
    private lateinit var boton: Button
    private lateinit var conf: File

    private var esperado: String? = null

    @Volatile
    private var ocupado = false

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)
        conf = File(filesDir, "multidrive.conf")

        val caja = LinearLayout(this).comoPantalla()
        caja.addView(cabecera("Eliminar volumen"))
        estado = TextView(this).apply {
            textSize = 14f
            setTextColor(getColor(R.color.texto))
            setPadding(0, 0, 0, 16)
            text = "Mirando qué hay dentro. Esto tarda: hay que enumerar cada cuenta…"
        }
        detalle = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 0, 0, 16)
        }
        // La instrucción va en una línea propia, no en el `hint` del campo. Un `hint` es de una
        // sola línea y se corta por la derecha: decía «Escribe el nombre de la cuenta número…»
        // y el número, que es lo único que hacía falta leer, quedaba fuera de la pantalla.
        instruccion = TextView(this).apply {
            textSize = 14f
            setTextColor(getColor(R.color.texto))
            setPadding(0, 0, 0, 10)
            visibility = View.GONE
        }
        campo = EditText(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            )
            inputType = InputType.TYPE_CLASS_TEXT
            hint = "Nombre de la cuenta"
            setTextColor(getColor(R.color.texto))
            setHintTextColor(getColor(R.color.apagado))
            visibility = View.GONE
        }
        boton = Button(this).apply {
            text = "Eliminar el volumen"
            visibility = View.GONE
            setOnClickListener { eliminar() }
        }
        caja.addView(estado)
        caja.addView(detalle)
        caja.addView(instruccion)
        caja.addView(campo)
        caja.addView(boton)
        setContentView(ScrollView(this).apply { addView(caja) })

        mirar()
    }

    /** Cuenta lo que hay. Tarda: una enumeración por cuenta, y nunca en el hilo de la interfaz. */
    private fun mirar() {
        thread {
            val (json, fallo) = try {
                JSONObject(Nucleo.mirarRetirada(conf.absolutePath)) to null
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo mirar la retirada", e)
                null to (dicho(e))
            }
            runOnUiThread {
                if (json == null) {
                    estado.text = fallo
                    return@runOnUiThread
                }
                pintar(json)
            }
        }
    }

    private fun pintar(r: JSONObject) {
        val nombres = r.getJSONArray("proveedores").let { a: JSONArray ->
            (0 until a.length()).map { a.getString(it) }
        }
        val conMaleta = r.getJSONArray("con_maleta").let { a: JSONArray ->
            (0 until a.length()).map { a.getString(it) }
        }
        val cual = r.getInt("cual")
        esperado = r.getString("esperado")

        estado.text = "Esto NO se puede deshacer y no hay papelera."
        detalle.text = buildString {
            append("Se va a retirar el volumen «${r.getString("topologia")}» de:\n")
            for ((i, n) in nombres.withIndex()) append("  ${i + 1}. $n\n")
            append("\n${r.getInt("fragmentos")} fragmento(s), ")
            append(legible(r.getLong("bytes")))
            append(".")
            // Las cuentas se desenlazan también, y eso hay que decirlo **antes**: quien lea
            // «se borra el volumen» no se espera quedarse además sin las cuentas puestas.
            append(
                "\n\nAdemás se quitan de este teléfono las ${nombres.size} cuenta(s) de arriba. " +
                    "Las cuentas no se tocan en la nube: solo dejan de estar enlazadas aquí, y " +
                    "el teléfono queda como recién instalado."
            )
            if (conMaleta.isNotEmpty()) {
                // Se nombra antes de confirmar: es lo único de la lista que sigue sirviendo
                // para algo después, y quien borra tiene que saber que también se va.
                append("\n\nTambién se borra tu configuración sellada, guardada en ")
                append(conMaleta.joinToString(", "))
                append(". Sin volumen no abre nada, y dejarla sería no devolver la cuenta a ")
                append("como estaba.")
            }
        }
        instruccion.text =
            "Para confirmar, escribe el nombre de la cuenta número ${cual + 1} de la lista de arriba."
        instruccion.visibility = View.VISIBLE
        campo.visibility = View.VISIBLE
        boton.visibility = View.VISIBLE
    }

    private fun eliminar() {
        val escrito = campo.text.toString().trim()
        if (escrito != esperado) {
            // No se dice cuál es el nombre correcto: decirlo convertiría la comprobación en un
            // trámite de copiar y pegar, que es lo que se está evitando.
            estado.text = "Eso no es lo que hay que escribir. No se ha borrado nada."
            return
        }
        ocupado = true
        boton.isEnabled = false
        campo.isEnabled = false
        estado.text = "Borrando. Esto tarda: hay que retirarlo de cada cuenta…"

        thread {
            val dicho = try {
                // La sesión se cierra antes: un volumen abierto sobre algo que ya no está solo
                // sirve para dar errores raros más tarde.
                Sesion.cerrar()
                val r = Nucleo.retirar(conf.absolutePath, escrito)
                // La pantalla principal ofrecera crear en vez de abrir: no hay nada que abrir,
                // y acabamos de comprobarlo de la unica forma que vale.
                Sesion.sinVolumen = true

                // **Y se desenlazan las cuentas.** Sin esto quedaban enlazadas apuntando a un
                // volumen que ya no existe: la pantalla principal seguia ensenando la tarjeta
                // de cuentas, y quien quisiera empezar de cero tenia que quitarlas una por una
                // sin que nada le dijera que hacia falta.
                //
                // Se quitan **despues** de retirar y no antes, porque retirar necesita hablar
                // con cada cuenta para borrar sus fragmentos. Quitarlas primero dejaria la
                // basura arriba y la cuenta fuera, que es el peor de los dos mundos.
                //
                // El fallo de una no para a las demas: lo que se busca es dejar el telefono
                // limpio, y pararse a la mitad lo deja peor que seguir.
                val sueltas = desenlazarTodas()
                if (sueltas.isEmpty()) r else "$r\n\n$sueltas"
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo retirar el volumen", e)
                dicho(e)
            }
            runOnUiThread {
                ocupado = false
                terminar(dicho)
            }
        }
    }

    /**
     * Quita del teléfono todas las cuentas enlazadas. Devuelve lo que haya que contar, o `""`
     * si salió todo bien, porque entonces no hay nada que contar que no se dé por supuesto.
     *
     * Corre en el hilo de fondo, como todo lo que habla con el núcleo.
     */
    private fun desenlazarTodas(): String {
        val nombres = try {
            val lista = JSONArray(Nucleo.cuentas(conf.absolutePath))
            (0 until lista.length()).map { lista.getString(it) }
        } catch (e: Throwable) {
            Log.w(ETIQUETA, "no se pudieron leer las cuentas para quitarlas", e)
            return "Las cuentas no se pudieron leer para quitarlas; siguen enlazadas."
        }

        val fallaron = mutableListOf<String>()
        for (n in nombres) {
            try {
                Nucleo.olvidar(conf.absolutePath, n)
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo quitar la cuenta $n", e)
                fallaron += n
            }
        }

        return when {
            fallaron.isEmpty() -> ""
            else -> "Estas cuentas no se pudieron quitar y siguen enlazadas: " +
                "${fallaron.joinToString(", ")}. Quítalas desde la pantalla principal."
        }
    }

    /** Enseña cómo quedó y **deja salir**. Esta pantalla no tiene barra. */
    private fun terminar(dicho: String) {
        val caja = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 96, 48, 48)
            setBackgroundColor(getColor(R.color.fondo))
        }
        caja.addView(
            TextView(this).apply {
                text = dicho
                textSize = 14f
                setTextColor(getColor(R.color.texto))
                setPadding(0, 0, 0, 24)
            }
        )
        caja.addView(
            Button(this).apply {
                text = "Volver"
                setOnClickListener {
                    contentResolver.notifyChange(
                        android.provider.DocumentsContract.buildRootsUri(AUTORIDAD),
                        null,
                    )
                    finish()
                }
            }
        )
        setContentView(ScrollView(this).apply { addView(caja) })
        Log.i(ETIQUETA, dicho)
    }
}
