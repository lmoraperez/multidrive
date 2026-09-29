package net.multidrive

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.provider.DocumentsContract
import android.util.Log
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import net.multidrive.nucleo.Nucleo
import org.json.JSONArray
import java.io.File
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"
private const val AUTORIDAD = "net.multidrive.documentos"

/**
 * La pantalla de MultiDrive.
 *
 * # Una sola funcion decide lo que se ve
 *
 * [pintar] fija el estado de **todo**, siempre y por completo, no por trozos segun lo que acabe
 * de pasar. Eso no es manía: en la ventana del escritorio el mismo fallo aparecio tres veces
 * seguidas --un aviso, una seccion y una oferta que se quedaban de la vez anterior diciendo algo
 * que ya no era verdad-- y las tres veces la causa fue pintar solo la rama que habia cambiado.
 *
 * # La frase de paso
 *
 * Vive en la sesion, en memoria, y muere con el proceso. Se borra del campo en cuanto se toma:
 * una frase a la vista es una captura de pantalla de mas. Android puede matar el proceso cuando
 * le convenga, asi que puede hacer falta escribirla otra vez, y eso se dice en la pantalla en
 * vez de fingir que la sesion es eterna.
 */
class Principal : Activity() {

    private lateinit var estado: TextView
    private lateinit var cuentas: TextView
    private lateinit var frase: EditText
    private lateinit var conf: File

    private lateinit var drive: Button
    private lateinit var onedrive: Button
    private lateinit var traer: Button
    private lateinit var abrir: Button
    private lateinit var ver: Button
    private lateinit var cerrar: Button
    private lateinit var tituloVolumen: TextView
    private lateinit var tarjetaVolumen: View
    private lateinit var notaMaleta: TextView

    /** Lo que se esta haciendo ahora mismo, si es que se esta haciendo algo. */
    @Volatile
    private var ocupado = false

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)
        setContentView(R.layout.principal)

        estado = findViewById(R.id.estado)
        cuentas = findViewById(R.id.cuentas)
        frase = findViewById(R.id.frase)
        drive = findViewById(R.id.drive)
        onedrive = findViewById(R.id.onedrive)
        traer = findViewById(R.id.traer)
        abrir = findViewById(R.id.abrir)
        ver = findViewById(R.id.ver)
        cerrar = findViewById(R.id.cerrar)
        tituloVolumen = findViewById(R.id.titulo_volumen)
        tarjetaVolumen = findViewById(R.id.tarjeta_volumen)
        notaMaleta = findViewById(R.id.nota_maleta)

        conf = File(filesDir, "multidrive.conf")

        try {
            Nucleo.iniciar(applicationContext)
        } catch (e: Throwable) {
            estado.text = "No se pudo cargar el núcleo:\n\n$e"
            return
        }

        drive.setOnClickListener { conectar("GDRIVE") }
        onedrive.setOnClickListener { conectar("ONEDRIVE") }
        traer.setOnClickListener { traerMaleta() }
        abrir.setOnClickListener { abrirVolumen() }
        cerrar.setOnClickListener {
            Sesion.cerrar()
            avisarAlSistema()
            pintar("Volumen cerrado. Ya no se ve en Archivos.")
        }
        ver.setOnClickListener {
            val i = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "*/*"
            }
            startActivity(Intent.createChooser(i, "Busca MultiDrive en la lista"))
        }
    }

    override fun onResume() {
        super.onResume()
        // Al volver de otra pantalla --el navegador, la app de Archivos-- el estado puede ser
        // otro: una cuenta recien conectada, o un proceso que Android mato por el camino.
        if (!ocupado) pintar(null)
    }

    /**
     * Deja la pantalla entera coherente con lo que hay. **Todo, siempre.**
     *
     * @param dicho qué contar arriba. `null` para que lo deduzca del estado.
     */
    private fun pintar(dicho: String?) {
        val lista = try {
            JSONArray(Nucleo.cuentas(conf.absolutePath))
        } catch (e: Throwable) {
            Log.w(ETIQUETA, "no se pudieron leer las cuentas", e)
            JSONArray()
        }
        val nombres = (0 until lista.length()).map { lista.getString(it) }
        val hayCuentas = nombres.isNotEmpty()
        val abierto = Sesion.id != null

        cuentas.text = if (hayCuentas) {
            "${nombres.size} configurada(s): ${nombres.joinToString(", ")}"
        } else {
            "Ninguna todavía. Conecta una para empezar."
        }

        // La maleta solo tiene sentido con algo enlazado: es de ahi de donde la saca.
        val puedeTraer = hayCuentas && !abierto
        notaMaleta.visibility = if (puedeTraer) View.VISIBLE else View.GONE
        traer.visibility = if (puedeTraer) View.VISIBLE else View.GONE

        // Sin cuentas no hay volumen que abrir, asi que la seccion entera sobra: ensenar un
        // campo de frase de paso que no puede servir para nada solo confunde.
        tituloVolumen.visibility = if (hayCuentas) View.VISIBLE else View.GONE
        tarjetaVolumen.visibility = if (hayCuentas) View.VISIBLE else View.GONE

        frase.visibility = if (!abierto) View.VISIBLE else View.GONE
        abrir.visibility = if (!abierto) View.VISIBLE else View.GONE
        ver.visibility = if (abierto) View.VISIBLE else View.GONE
        cerrar.visibility = if (abierto) View.VISIBLE else View.GONE

        // Conectar mas cuentas con el volumen abierto no vale: el volumen es de las que tiene,
        // y una de mas no entra sin reescribir su cabecera, que es cosa del escritorio.
        drive.isEnabled = !abierto && !ocupado
        onedrive.isEnabled = !abierto && !ocupado
        traer.isEnabled = !ocupado
        abrir.isEnabled = !ocupado

        estado.text = dicho ?: when {
            !hayCuentas -> "Conecta una cuenta de nube para empezar."
            abierto -> "Volumen abierto. Búscalo en la app de Archivos."
            else -> "Escribe tu frase de paso y abre el volumen."
        }
    }

    /** Lanza algo que tarda, sin congelar la pantalla y sin dejar botones a medias. */
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

    private fun conectar(tipo: String) = enSegundoPlano(
        "Abriendo el navegador. Entra en tu cuenta y da permiso…"
    ) {
        val nombre = Nucleo.conectar(tipo, conf.absolutePath)
        "Cuenta conectada como «$nombre»."
    }

    private fun traerMaleta() {
        val f = tomarFrase() ?: return
        enSegundoPlano("Buscando tu configuración en las cuentas que ya hay…") {
            Nucleo.recuperar(conf.absolutePath, f)
        }
    }

    private fun abrirVolumen() {
        val f = tomarFrase() ?: return
        enSegundoPlano("Abriendo. Son unos segundos: hay que preguntarle a cada cuenta…") {
            Sesion.abrir(conf.absolutePath, f)
            avisarAlSistema()
            "Volumen abierto. Búscalo en la app de Archivos."
        }
    }

    /**
     * La frase escrita, y borra el campo.
     *
     * Devuelve `null` si esta vacio, y lo dice: un boton que no hace nada al pulsarlo se lee
     * como una aplicacion rota.
     */
    private fun tomarFrase(): String? {
        val f = frase.text.toString()
        if (f.isEmpty()) {
            pintar("Escribe primero la frase de paso.")
            return null
        }
        frase.setText("")
        return f
    }

    /** Sin esto, la app de Archivos sigue enseñando el estado de antes. */
    private fun avisarAlSistema() {
        contentResolver.notifyChange(DocumentsContract.buildRootsUri(AUTORIDAD), null)
    }
}
