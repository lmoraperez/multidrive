package net.multidrive

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.util.Calendar

/**
 * Una entrada del volumen, tal y como la pinta el explorador.
 *
 * `modificado` es opcional y no por descuido: el arbol guarda fechas desde hace poco, asi que lo
 * escrito antes no tiene ninguna. `null` quiere decir **no se sabe**, y se enseña como tal.
 */
data class Fichero(
    val ruta: String,
    val nombre: String,
    val dir: Boolean,
    val bytes: Long,
    val modificado: Long?,
) {
    companion object {
        /** De una entrada de `listar`, que no trae ruta porque ya se sabe el directorio. */
        fun deListado(padre: String, o: JSONObject): Fichero {
            val nombre = o.getString("nombre")
            return Fichero(
                ruta = if (padre == "/") "/$nombre" else "$padre/$nombre",
                nombre = nombre,
                dir = o.getBoolean("dir"),
                bytes = o.getLong("bytes"),
                modificado = if (o.isNull("modificado")) null else o.getLong("modificado"),
            )
        }

        /** De un hallazgo de `buscar` o `recientes`, que sí trae ruta. */
        fun deHallazgo(o: JSONObject) = Fichero(
            ruta = o.getString("ruta"),
            nombre = o.getString("nombre"),
            dir = o.getBoolean("dir"),
            bytes = o.getLong("bytes"),
            modificado = if (o.isNull("modificado")) null else o.getLong("modificado"),
        )

        /** Directorios primero y, dentro de cada grupo, por nombre. Es como se espera leer. */
        fun ordenar(l: List<Fichero>): List<Fichero> =
            l.sortedWith(compareByDescending<Fichero> { it.dir }.thenBy { it.nombre.lowercase() })
    }
}

/** Lo que devuelven `buscar` y `recientes`: los hallazgos y si hubo que parar antes de terminar. */
data class Hallazgos(val ficheros: List<Fichero>, val incompleto: Boolean) {
    companion object {
        fun de(json: String): Hallazgos {
            val o = JSONObject(json)
            val a: JSONArray = o.getJSONArray("hallazgos")
            return Hallazgos(
                ficheros = (0 until a.length()).map { Fichero.deHallazgo(a.getJSONObject(it)) },
                incompleto = o.getBoolean("incompleto"),
            )
        }
    }
}

/**
 * Una fecha como la lee una persona: «hoy», «ayer», «hace 4 días», o el día y el mes.
 *
 * Devuelve cadena vacía cuando no hay fecha. **No se inventa ninguna**: lo escrito antes de que
 * el árbol las guardara no tiene, y poner «1 ene 1970» sería afirmar algo falso sobre un fichero
 * de alguien.
 */
fun cuando(segundos: Long?): String {
    if (segundos == null) return ""
    val ahora = System.currentTimeMillis() / 1000
    val dif = ahora - segundos
    return when {
        dif < 60 -> "ahora mismo"
        dif < 3600 -> "hace ${dif / 60} min"
        dif < 86_400 -> "hace ${dif / 3600} h"
        dif < 172_800 -> "ayer"
        dif < 604_800 -> "hace ${dif / 86_400} días"
        else -> {
            val c = Calendar.getInstance().apply { timeInMillis = segundos * 1000 }
            val meses = listOf(
                "ene", "feb", "mar", "abr", "may", "jun",
                "jul", "ago", "sep", "oct", "nov", "dic",
            )
            val dia = c.get(Calendar.DAY_OF_MONTH)
            val mes = meses[c.get(Calendar.MONTH)]
            val año = c.get(Calendar.YEAR)
            // El año solo si no es este: en una lista de ficheros de hoy, repetirlo es ruido.
            if (año == Calendar.getInstance().get(Calendar.YEAR)) "$dia $mes" else "$dia $mes $año"
        }
    }
}

/**
 * Los favoritos, guardados **solo en este teléfono**.
 *
 * # Por qué aquí y no en el volumen
 *
 * Porque marcar un favorito en el árbol publicaría una raíz nueva: una vuelta a todas las
 * cuentas, y una ocasión de chocar con otro equipo que esté escribiendo. Un gesto que parece
 * gratis y costaría lo mismo que subir un fichero.
 *
 * Lo que eso cuesta, y hay que decirlo donde se vea: **no te siguen**. Abrir el volumen desde el
 * escritorio o desde otro móvil no enseña estos favoritos.
 *
 * Se guardan por ruta. Si un fichero se renombra o se mueve, su favorito se queda apuntando a
 * donde ya no hay nada; [limpiar] los quita cuando se nota, en vez de dejar una lista con
 * entradas que no abren.
 */
object Favoritos {
    private const val FICHERO = "favoritos"
    private const val CLAVE = "rutas"

    private fun prefs(c: Context) = c.getSharedPreferences(FICHERO, Context.MODE_PRIVATE)

    fun todos(c: Context): Set<String> = prefs(c).getStringSet(CLAVE, emptySet()) ?: emptySet()

    fun es(c: Context, ruta: String) = todos(c).contains(ruta)

    fun cambiar(c: Context, ruta: String): Boolean {
        val ahora = todos(c).toMutableSet()
        val puesto = if (ahora.contains(ruta)) {
            ahora.remove(ruta); false
        } else {
            ahora.add(ruta); true
        }
        // Una copia nueva: `getStringSet` devuelve un conjunto que no se debe modificar, y
        // hacerlo deja el valor guardado y el de memoria diciendo cosas distintas.
        prefs(c).edit().putStringSet(CLAVE, HashSet(ahora)).apply()
        return puesto
    }

    /** Quita los que ya no existen. Se llama tras listarlos, que es cuando se sabe. */
    fun limpiar(c: Context, vivos: Collection<String>) {
        val quedan = todos(c).filter { vivos.contains(it) }
        if (quedan.size != todos(c).size) {
            prefs(c).edit().putStringSet(CLAVE, HashSet(quedan)).apply()
        }
    }
}
