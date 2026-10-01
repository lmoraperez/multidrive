package net.multidrive

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.provider.DocumentsContract
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
import java.io.File
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/**
 * Abrir un volumen que ya existe, en un telefono donde no hay nada configurado.
 *
 * # El recorrido
 *
 * 1. Enlazar **una** cuenta. Una sola, no todas: es lo que separa esto de crear.
 * 2. Mirar si esa cuenta trae maleta. No hace falta la frase para saberlo, solo para abrirla.
 * 3. Con la frase: sacar de la maleta las demas cuentas y abrir el volumen, de una vez.
 *
 * # Por que se mira la maleta antes de pedir la frase
 *
 * Porque si no la hay, la frase no va a servir de nada y pedirla es mandar a alguien a probar
 * algo que no puede funcionar. Cuando no la hay lo util es decir **que hacer**: abrir el volumen
 * una vez en el equipo que si tiene la configuracion --se guarda sola-- o enlazar a mano las
 * cuentas que falten.
 *
 * # Por que una sola frase para las dos cosas
 *
 * La maleta se cifra con la frase del volumen. Son la misma, asi que pedirla dos veces seria
 * hacer creer que son dos.
 */
class Abrir : Activity() {

    private lateinit var estado: TextView
    private lateinit var lista: TextView
    private lateinit var drive: Button
    private lateinit var onedrive: Button
    private lateinit var frase: EditText
    private lateinit var notaFrase: TextView
    private lateinit var boton: Button
    private lateinit var cancelar: Button
    private lateinit var conf: File

    @Volatile
    private var ocupado = false

    /** Si alguna de las cuentas enlazadas trae maleta. `null` mientras no se ha mirado. */
    private var maleta: Boolean? = null

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)
        conf = File(filesDir, "multidrive.conf")

        val caja = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 72, 48, 48)
            setBackgroundColor(getColor(R.color.fondo))
        }

        caja.addView(
            TextView(this).apply {
                text = "Abrir un volumen"
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
        lista = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 0, 0, 16)
        }
        drive = Button(this).apply {
            text = "Conectar Google Drive"
            setOnClickListener { conectar("GDRIVE") }
        }
        onedrive = Button(this).apply {
            text = "Conectar OneDrive"
            setOnClickListener { conectar("ONEDRIVE") }
        }
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
        notaFrase = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 10, 0, 10)
            visibility = View.GONE
        }
        boton = Button(this).apply {
            text = "Abrir volumen"
            visibility = View.GONE
            setOnClickListener { abrir() }
        }
        // La salida mientras se espera al navegador. Sin ella, el unico recurso es el boton
        // atras del navegador, que reenvia la misma peticion y la deja inservible.
        cancelar = Button(this).apply {
            text = "Cancelar"
            visibility = View.GONE
            setOnClickListener { Nucleo.cancelarConexion() }
        }

        caja.addView(estado)
        caja.addView(lista)
        caja.addView(drive)
        caja.addView(onedrive)
        caja.addView(frase)
        caja.addView(notaFrase)
        caja.addView(boton)
        caja.addView(cancelar)
        setContentView(ScrollView(this).apply { addView(caja) })

        // Si ya hay cuentas --se vuelve aqui, o se enlazaron antes-- se mira la maleta de
        // entrada en vez de pedir que se conecte otra que ya esta.
        if (cuentas().isNotEmpty()) mirarMaleta() else pintar("Aún no hay ninguna cuenta.")
    }

    private fun cuentas(): List<String> = try {
        val a = JSONArray(Nucleo.cuentas(conf.absolutePath))
        (0 until a.length()).map { a.getString(it) }
    } catch (e: Throwable) {
        Log.w(ETIQUETA, "no se pudieron leer las cuentas", e)
        emptyList()
    }

    private fun pintar(dicho: String?) {
        val hay = cuentas()
        lista.text = if (hay.isEmpty()) {
            "Ninguna cuenta enlazada todavía."
        } else {
            "Enlazadas: ${hay.joinToString(", ")}"
        }
        drive.isEnabled = !ocupado
        onedrive.isEnabled = !ocupado
        boton.isEnabled = !ocupado
        cancelar.visibility = if (ocupado) View.VISIBLE else View.GONE

        val conMaleta = maleta == true
        frase.visibility = if (conMaleta) View.VISIBLE else View.GONE
        notaFrase.visibility = if (conMaleta) View.VISIBLE else View.GONE
        boton.visibility = if (conMaleta) View.VISIBLE else View.GONE
        notaFrase.text = "Con ella se sacan las demás cuentas y se abre el volumen, de una vez. " +
            "Es la misma frase con la que se creó."

        estado.text = dicho ?: when {
            hay.isEmpty() -> "Conecta una de las cuentas donde está el volumen. Con una basta."
            conMaleta -> "Encontrada tu configuración. Escribe la frase de paso."
            else -> "Comprobando…"
        }
    }

    private fun enSegundoPlano(mientras: String, trabajo: () -> String) {
        ocupado = true
        pintar(mientras)
        thread {
            val dicho = try {
                trabajo()
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "fallo", e)
                e.message ?: e.toString()
            }
            runOnUiThread {
                ocupado = false
                pintar(dicho)
            }
        }
    }

    private fun conectar(tipo: String) {
        ocupado = true
        pintar(MIENTRAS_NAVEGADOR)
        thread {
            val dicho = try {
                val nombre = Nucleo.conectar(tipo, conf.absolutePath)
                "Cuenta conectada como «$nombre». Mirando si trae tu configuración…"
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo conectar", e)
                ocupado = false
                runOnUiThread { pintar(e.message ?: e.toString()) }
                return@thread
            }
            runOnUiThread { pintar(dicho) }
            // Encadenado a proposito: recien enlazada la cuenta, lo siguiente que quiere saber
            // quien esta aqui es si de ahi sale el resto. Hacerle pulsar otro boton para
            // averiguarlo seria pedirle un paso que no aporta nada.
            mirarEnEsteHilo()
        }
    }

    private fun mirarMaleta() {
        ocupado = true
        pintar("Mirando si tus cuentas traen la configuración…")
        thread { mirarEnEsteHilo() }
    }

    /** Cuesta una petición por cuenta. Se llama siempre desde un hilo que no es el de la interfaz. */
    private fun mirarEnEsteHilo() {
        val (hay, fallo) = try {
            Nucleo.hayMaleta(conf.absolutePath) to null
        } catch (e: Throwable) {
            Log.w(ETIQUETA, "no se pudo mirar la maleta", e)
            false to (e.message ?: e.toString())
        }
        runOnUiThread {
            ocupado = false
            maleta = hay
            pintar(
                when {
                    fallo != null -> "No se pudo comprobar: $fallo"
                    hay -> null
                    else -> "Esta cuenta no guarda tu configuración.\n\nEn el equipo donde sí " +
                        "tienes MultiDrive, abre el volumen una vez: la configuración se guarda " +
                        "sola. Después vuelve aquí.\n\nO conecta a mano todas las cuentas del " +
                        "volumen y ábrelo desde la pantalla principal."
                }
            )
        }
    }

    /**
     * Trae las demas cuentas y abre, con la misma frase.
     *
     * Si la maleta se abre pero el volumen no, **no se deshace lo traido**: las cuentas que
     * ahora estan configuradas siguen siendo las correctas, y borrarlas por no haber podido
     * abrir dejaria el telefono peor que antes de intentarlo.
     */
    private fun abrir() {
        val f = frase.text.toString()
        if (f.isEmpty()) {
            pintar("Escribe la frase de paso.")
            return
        }
        frase.setText("")

        enSegundoPlano("Trayendo tus cuentas y abriendo. Son unos segundos…") {
            val traido = Nucleo.recuperar(conf.absolutePath, f)
            Sesion.abrir(conf.absolutePath, f, true)
            contentResolver.notifyChange(DocumentsContract.buildRootsUri(AUTORIDAD), null)
            runOnUiThread {
                startActivity(
                    Intent(this, Principal::class.java)
                        .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP)
                )
                finish()
            }
            "$traido\n\nVolumen abierto."
        }
    }
}
