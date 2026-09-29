package net.multidrive

import android.app.Activity
import android.os.Bundle
import android.provider.DocumentsContract
import android.text.InputType
import android.util.Log
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import net.multidrive.nucleo.Nucleo
import java.io.File
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"
private const val AUTORIDAD = "net.multidrive.documentos"

/**
 * Pide la frase de paso y abre el volumen.
 *
 * Es lo minimo, y no por dejadez: un `DocumentsProvider` **no tiene pantalla**, asi que alguien
 * tiene que pedir la frase, y ese alguien es esta actividad. Mientras no haya sesion, MultiDrive
 * no aparece en la app de Archivos; en cuanto la hay, se avisa al sistema y aparece.
 *
 * La frase no se guarda en ningun sitio: vive en la sesion, en memoria, y muere con el proceso.
 * Android puede matar el proceso cuando le convenga, asi que **volver a Archivos puede pedirla
 * otra vez**. Eso se dice, en vez de fingir que la sesion es eterna.
 *
 * Lo que todavia se hace a mano es la configuracion de las cuentas, que hay que dejar en
 * `files/multidrive.conf`. Conectarlas por el navegador es el paso siguiente; hasta entonces,
 * esta pantalla dice donde falta el fichero en vez de fallar sin explicacion.
 */
class Principal : Activity() {

    private lateinit var estado: TextView

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)

        val caja = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 96, 48, 48)
        }

        estado = TextView(this).apply { textSize = 15f }
        val frase = EditText(this).apply {
            hint = "Frase de paso"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
        }
        val boton = Button(this).apply { text = "Abrir volumen" }
        val cerrar = Button(this).apply { text = "Cerrar volumen" }
        val drive = Button(this).apply { text = "Conectar Google Drive" }
        val onedrive = Button(this).apply { text = "Conectar OneDrive" }
        val ver = Button(this).apply { text = "Ver el volumen en Archivos" }

        caja.addView(estado)
        caja.addView(drive)
        caja.addView(onedrive)
        caja.addView(frase)
        caja.addView(boton)
        caja.addView(cerrar)
        caja.addView(ver)
        setContentView(caja)

        // Abre el selector de ficheros **del sistema**, que es donde MultiDrive tiene que
        // aparecer como origen. Existe porque cada fabricante esconde eso en un sitio distinto:
        // el gestor de archivos que trae el telefono puede no ensenar los origenes del Storage
        // Access Framework, y entonces «no aparece» y «no funciona» se confunden.
        ver.setOnClickListener {
            val i = android.content.Intent(android.content.Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(android.content.Intent.CATEGORY_OPENABLE)
                type = "*/*"
            }
            startActivity(android.content.Intent.createChooser(i, "Busca MultiDrive en la lista"))
        }

        val conf = File(filesDir, "multidrive.conf")

        try {
            Nucleo.iniciar(applicationContext)
        } catch (e: Throwable) {
            decir("No se pudo cargar el nucleo:\n\n$e")
            return
        }

        decir(
            if (Sesion.id != null) "Volumen abierto. Busca MultiDrive en la app de Archivos."
            else if (!conf.exists()) "Falta la configuracion de cuentas en:\n${conf.absolutePath}"
            else "MultiDrive ${Nucleo.version()}\nEscribe la frase de paso."
        )

        boton.setOnClickListener {
            val f = frase.text.toString()
            if (f.isEmpty()) return@setOnClickListener
            // Se borra del campo en cuanto se toma: una frase a la vista es una captura de
            // pantalla de mas.
            frase.setText("")
            boton.isEnabled = false
            decir("Abriendo... son unos segundos: hay que preguntarle a cada cuenta.")

            // En otro hilo: abrir tarda seis segundos medidos, y hacerlo aqui congelaria la
            // pantalla y Android acabaria matando la aplicacion por no responder.
            thread {
                val dicho = try {
                    Sesion.abrir(conf.absolutePath, f)
                    avisarAlSistema()
                    "Volumen abierto. Busca MultiDrive en la app de Archivos."
                } catch (e: Throwable) {
                    Log.w(ETIQUETA, "no se pudo abrir", e)
                    "No se pudo abrir:\n\n${e.message ?: e.toString()}"
                }
                runOnUiThread {
                    decir(dicho)
                    boton.isEnabled = true
                }
            }
        }

        // Conectar una cuenta: abre el navegador y espera. Es lo que quita de en medio tener
        // que pasar credenciales por cable, que era la unica forma hasta ahora.
        for ((b, tipo) in listOf(drive to "GDRIVE", onedrive to "ONEDRIVE")) {
            b.setOnClickListener {
                b.isEnabled = false
                decir("Abriendo el navegador. Entra en tu cuenta y da permiso...")
                thread {
                    val dicho = try {
                        val nombre = Nucleo.conectar(tipo, conf.absolutePath)
                        "Cuenta conectada como '$nombre'.\nYa puedes abrir el volumen."
                    } catch (e: Throwable) {
                        Log.w(ETIQUETA, "no se pudo conectar $tipo", e)
                        "No se pudo conectar:\n\n${e.message ?: e.toString()}"
                    }
                    runOnUiThread {
                        decir(dicho)
                        b.isEnabled = true
                    }
                }
            }
        }

        cerrar.setOnClickListener {
            Sesion.cerrar()
            avisarAlSistema()
            decir("Volumen cerrado. Ya no aparece en Archivos.")
        }
    }

    /**
     * Avisa de que las raices cambiaron.
     *
     * Sin esto, la app de Archivos sigue ensenando lo de antes: que MultiDrive no esta cuando
     * acaba de abrirse, o que esta cuando acaba de cerrarse.
     */
    private fun avisarAlSistema() {
        contentResolver.notifyChange(DocumentsContract.buildRootsUri(AUTORIDAD), null)
    }

    private fun decir(texto: String) {
        estado.text = texto
        Log.i(ETIQUETA, texto)
    }
}
