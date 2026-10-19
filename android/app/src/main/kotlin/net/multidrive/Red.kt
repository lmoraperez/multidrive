package net.multidrive

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
import android.net.Uri
import android.provider.Settings
import android.util.Log

/**
 * El ahorro de datos, y por qué hay que mirarlo antes de mandar a nadie al navegador.
 *
 * # Lo que pasaba
 *
 * Un tester intentó conectar su Google Drive y le salió «no address associated with hostname».
 * Eso se lee como un problema de Google, y no lo era: tenía el ahorro de datos encendido, y
 * Android le estaba cortando la red a MultiDrive.
 *
 * El flujo de OAuth es especialmente frágil con esto porque **se parte en dos**: la primera
 * mitad la hace el navegador --que sí tiene red, porque es quien está delante-- y la segunda
 * la hace MultiDrive, que para entonces lleva un rato detrás. La parte que falla no es la que
 * el usuario está mirando.
 *
 * # Por qué no se arregla esperando
 *
 * Se intentó: primero una pausa de 1,2 s, luego esperar a que la aplicación volviera a estar
 * delante de verdad, y luego reintentar el canje tres veces. Medido en un TECNO LH7n con
 * Android 14, con el ahorro de datos puesto, **las tres cosas fallan igual**. El registro de
 * `netpolicy` dice por qué no bastan: el sistema abre el cortafuegos del UID en el mismo
 * instante en que la pantalla resume, así que la primera consulta de DNS sale justo en la
 * frontera; y los reintentos a los 0,4, 1,6 y 4,6 segundos fallan también, lo que encaja con
 * que esa primera consulta fallida se quede cacheada en negativo.
 *
 * Así que esto no se arregla por dentro. Lo que sí se puede hacer es **no empezar a ciegas**:
 * preguntar antes, decirlo con claridad, y llevar al usuario al ajuste que lo resuelve.
 */

/**
 * Si el ahorro de datos le está cortando la red a MultiDrive **en la red de ahora mismo**.
 *
 * La pregunta lleva dentro lo de «ahora mismo», y eso es lo que la hace útil: Android sólo
 * restringe en redes medidas, así que con wifi esto devuelve `false` aunque el ahorro de datos
 * esté encendido. Preguntar por el ajuste a secas daría un aviso falso a quien está en casa.
 *
 * Devuelve `false` ante cualquier duda. Un aviso que no toca molesta más que no avisar, y el
 * intento de conectar sigue estando ahí para contar lo que pase.
 */
fun ahorroDeDatosCorta(contexto: Context): Boolean {
    val cm = contexto.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
        ?: return false
    return try {
        cm.restrictBackgroundStatus == ConnectivityManager.RESTRICT_BACKGROUND_STATUS_ENABLED
    } catch (e: Throwable) {
        Log.w("multidrive", "no se pudo mirar el ahorro de datos", e)
        false
    }
}

/**
 * Hace [seguir] si se puede conectar, y si no, lo explica antes.
 *
 * El aviso no bloquea: deja «Intentarlo igual», porque la respuesta de Android vale para la
 * red de este segundo y el usuario puede estar a punto de cambiar de red. Lo que no hace es
 * dejar que alguien se meta en el navegador sin saber que la vuelta va a fallar.
 */
fun conPermisoDeRed(actividad: Activity, seguir: () -> Unit) {
    if (!ahorroDeDatosCorta(actividad)) {
        seguir()
        return
    }

    AlertDialog.Builder(actividad)
        .setTitle("El ahorro de datos va a cortar la conexión")
        .setMessage(
            "Android tiene el ahorro de datos encendido y MultiDrive no está entre las " +
                "excepciones.\n\n" +
                "La primera parte la harías en el navegador, que sí tiene red. La que falla " +
                "es la de después: al volver, MultiDrive no podrá hablar con el proveedor " +
                "para terminar de enlazar la cuenta, y el error dirá que no encuentra la " +
                "dirección, como si fuera cosa de Google.\n\n" +
                "Dale a MultiDrive datos sin restricción, o apaga el ahorro de datos, y " +
                "vuelve a intentarlo."
        )
        .setPositiveButton("Abrir los ajustes") { _, _ -> abrirAjusteDeDatos(actividad) }
        .setNeutralButton("Intentarlo igual") { _, _ -> seguir() }
        .setNegativeButton("Cancelar", null)
        .show()
}

/**
 * Lleva a la pantalla donde se le dan datos sin restricción a esta aplicación.
 *
 * Con el `package:` delante, Android abre el ajuste **de MultiDrive**, no la lista entera. Si
 * el fabricante no trae esa pantalla --pasa--, se cae a la de uso de datos, que está en todos
 * sitios y desde la que también se llega.
 */
private fun abrirAjusteDeDatos(actividad: Activity) {
    val suyo = Intent(
        Settings.ACTION_IGNORE_BACKGROUND_DATA_RESTRICTIONS_SETTINGS,
        Uri.parse("package:${actividad.packageName}"),
    )
    try {
        actividad.startActivity(suyo)
        return
    } catch (e: Throwable) {
        Log.w("multidrive", "sin pantalla de datos sin restriccion", e)
    }
    try {
        actividad.startActivity(Intent(Settings.ACTION_DATA_USAGE_SETTINGS))
    } catch (e: Throwable) {
        Log.w("multidrive", "sin pantalla de uso de datos", e)
    }
}
