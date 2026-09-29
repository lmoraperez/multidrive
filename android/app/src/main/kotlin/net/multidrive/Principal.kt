package net.multidrive

import android.app.Activity
import android.os.Bundle
import android.widget.TextView
import net.multidrive.nucleo.Nucleo

/**
 * Lo minimo que demuestra que la cadena entera funciona.
 *
 * Sin interfaz de verdad a proposito. Lo que hay que saber en este paso es si el `.so` se
 * empaqueto, se cargo, se encontraron los simbolos y las cadenas cruzan: cuatro cosas que
 * pueden fallar por separado y que fallan de maneras parecidas. Una pantalla bonita encima
 * solo dificultaria ver cual de las cuatro fue.
 */
class Principal : Activity() {
    override fun onCreate(estado: Bundle?) {
        super.onCreate(estado)

        val texto = TextView(this)
        texto.setPadding(48, 96, 48, 48)
        texto.textSize = 16f

        // Se recoge el fallo de carga en vez de dejar que cierre la aplicacion: un
        // `UnsatisfiedLinkError` que mata el proceso no dice nada, y este es justo el paso en
        // el que mas probable es que pase.
        texto.text = try {
            buildString {
                append("Nucleo cargado\n\n")
                append("version   ").append(Nucleo.version()).append('\n')
                append("prueba    ").append(Nucleo.prueba()).append('\n')
                append("\nabi       ").append(android.os.Build.SUPPORTED_ABIS.joinToString())
            }
        } catch (e: Throwable) {
            "No se pudo cargar el nucleo:\n\n$e"
        }

        setContentView(texto)
    }
}
