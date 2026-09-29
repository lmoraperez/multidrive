package net.multidrive

import android.app.Activity
import android.os.Bundle
import android.widget.TextView
import net.multidrive.nucleo.Nucleo
import kotlin.concurrent.thread

/**
 * Lo minimo que demuestra que la cadena entera funciona.
 *
 * Sin interfaz de verdad a proposito. Lo que hay que saber en estos pasos es si el `.so` se
 * empaqueto, se cargo, se encontraron los simbolos, las cadenas cruzan y el verificador de
 * certificados esta inicializado: cosas que pueden fallar por separado y que fallan de maneras
 * parecidas. Una pantalla bonita encima solo dificultaria ver cual de ellas fue.
 */
class Principal : Activity() {
    override fun onCreate(estado: Bundle?) {
        super.onCreate(estado)

        val texto = TextView(this)
        texto.setPadding(48, 96, 48, 48)
        texto.textSize = 16f
        setContentView(texto)

        // Se recoge el fallo de carga en vez de dejar que cierre la aplicacion: un
        // `UnsatisfiedLinkError` que mata el proceso no dice nada, y estos son justo los pasos
        // en los que mas probable es que pase.
        val cabecera = try {
            // Lo primero de todo, antes de cualquier peticion.
            Nucleo.iniciar(applicationContext)
            buildString {
                append("Nucleo cargado\n\n")
                append("version   ").append(Nucleo.version()).append('\n')
                append("prueba    ").append(Nucleo.prueba()).append('\n')
                append("\nhttps     consultando...")
            }
        } catch (e: Throwable) {
            texto.text = "No se pudo cargar el nucleo:\n\n$e"
            return
        }
        texto.text = cabecera

        // En otro hilo: una peticion de red en el hilo de la interfaz la bloquea, y Android
        // ademas lo prohibe. Es tambien lo que hara el `DocumentsProvider`, que se llama desde
        // cualquier hilo menos ese.
        thread {
            val dicho = try {
                Nucleo.https()
            } catch (e: Throwable) {
                "excepcion: $e"
            }
            runOnUiThread {
                texto.text = cabecera.replace("consultando...", dicho)
            }
        }
    }
}
