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

        caja.addView(estado)
        caja.addView(frase)
        caja.addView(boton)
        caja.addView(cerrar)
        setContentView(caja)

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
