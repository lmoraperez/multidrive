package net.multidrive

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.DocumentsContract
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.CheckBox
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import net.multidrive.nucleo.Nucleo

/**
 * Lo que guarda la configuración de la carga automática.
 *
 * Son las mismas decisiones que ofrece Nextcloud, que es un buen catálogo de lo que hay que
 * preguntar, menos una: **no se ofrece borrar el original tras subirlo**. Borrar las fotos de
 * alguien con un sistema en pre-alfa no se hace, y un ajuste que existe se acaba activando.
 */
object Carga {
    private const val FICHERO = "carga"

    private fun p(c: Context) = c.getSharedPreferences(FICHERO, Context.MODE_PRIVATE)

    private fun si(c: Context, k: String, porDefecto: Boolean) = p(c).getBoolean(k, porDefecto)
    private fun pon(c: Context, k: String, v: Boolean) = p(c).edit().putBoolean(k, v).apply()

    fun activa(c: Context) = si(c, "activa", false)
    fun activa(c: Context, v: Boolean) = pon(c, "activa", v)

    fun origen(c: Context): String? = p(c).getString("origen", null)
    fun origen(c: Context, v: String) = p(c).edit().putString("origen", v).apply()

    fun destino(c: Context): String = p(c).getString("destino", "/Fotos") ?: "/Fotos"

    fun soloWifi(c: Context) = si(c, "solo_wifi", true)
    fun soloWifi(c: Context, v: Boolean) = pon(c, "solo_wifi", v)

    fun soloCargando(c: Context) = si(c, "solo_cargando", false)
    fun soloCargando(c: Context, v: Boolean) = pon(c, "solo_cargando", v)

    fun tambienExistentes(c: Context) = si(c, "tambien_existentes", false)
    fun tambienExistentes(c: Context, v: Boolean) = pon(c, "tambien_existentes", v)

    fun porFecha(c: Context) = si(c, "por_fecha", false)
    fun porFecha(c: Context, v: Boolean) = pon(c, "por_fecha", v)
}

/**
 * Configuración de la aplicación.
 *
 * # La casilla que importa
 *
 * La carga automática sube cosas **cuando no estás delante**, y para escribir en el volumen hace
 * falta su clave. Hoy esa clave vive solo en memoria y muere con el proceso, que Android mata
 * cuando le conviene. Así que o se guarda, o la carga automática no ocurre.
 *
 * Se guarda en el Keystore de Android, respaldado por hardware, atada a este teléfono y
 * exigiendo pantalla desbloqueada. No se guarda la frase de paso: lo que se guarda no sirve para
 * abrir el volumen en otro equipo ni se puede teclear en ningún sitio.
 *
 * Viene **apagada**, y al encenderla se dice lo que cambia. Esa frase no es un trámite: es la
 * diferencia entre «solo tú puedes abrir esto» y «quien tenga tu teléfono desbloqueado puede».
 */
class Ajustes : Activity() {

    private lateinit var estado: TextView
    private lateinit var parte: TextView
    private lateinit var activa: CheckBox
    private lateinit var origen: TextView
    private lateinit var destino: TextView
    private lateinit var soloWifi: CheckBox
    private lateinit var soloCargando: CheckBox
    private lateinit var existentes: CheckBox
    private lateinit var porFecha: CheckBox

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)

        val caja = LinearLayout(this).comoPantalla()
        caja.addView(cabecera("Configuración"))

        estado = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 0, 0, 24)
        }
        caja.addView(estado)
        parte = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.acento))
            setPadding(0, 0, 0, 16)
            visibility = View.GONE
        }
        caja.addView(parte)

        caja.addView(titulo("Carga automática", 16f))
        caja.addView(
            nota(
                "Lo que aparezca en una carpeta del teléfono sube solo al volumen. Es lo que " +
                    "hace falta para que las fotos se guarden sin acordarse."
            )
        )

        activa = casilla("Activar la carga automática") { puesta ->
            if (puesta) confirmarClave() else apagar()
        }
        caja.addView(activa)

        origen = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 8, 0, 8)
        }
        caja.addView(origen)
        caja.addView(
            Button(this).apply {
                text = "Elegir carpeta del teléfono"
                setOnClickListener { elegirCarpeta() }
            }
        )

        destino = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 16, 0, 8)
        }
        caja.addView(destino)

        // Cambiar una condicion obliga a reprogramar: `JobScheduler` las fijo al crear el
        // trabajo, y sin esto seguiria subiendo con las de antes.
        soloWifi = casilla("Solo con Wi-Fi no tarificada") { Carga.soloWifi(this, it); Subidor.programar(this) }
        soloCargando = casilla("Solo con el teléfono cargando") { Carga.soloCargando(this, it); Subidor.programar(this) }
        existentes = casilla("Subir también lo que ya está en la carpeta") {
            Carga.tambienExistentes(this, it)
            // Marcarlo tiene efecto ahora, no en el proximo ciclo: quien lo marca es porque
            // quiere que suba lo que ya estaba.
            Subidor.ahora(this)
        }
        porFecha = casilla("Guardar en subcarpetas por fecha") { Carga.porFecha(this, it) }
        for (c in listOf(soloWifi, soloCargando, existentes, porFecha)) caja.addView(c)

        caja.addView(
            nota(
                "El original se queda donde está, siempre. MultiDrive no borra nada de tu " +
                    "teléfono."
            )
        )

        setContentView(ScrollView(this).apply { addView(caja) })
        pintar()
        pintarParte()
    }

    override fun onResume() {
        super.onResume()
        pintar()
        pintarParte()
    }

    /**
     * El nombre de la carpeta como lo escribiría una persona.
     *
     * Lo que guarda Android es un `content://…/tree/primary%3ADCIM%2FCamera%2Fpruebas`, que
     * ocupa cuatro renglones y no dice nada. Dentro lleva la ruta de verdad; se saca de ahí.
     *
     * Si el formato no es el esperado --otro proveedor de documentos, una tarjeta SD-- se
     * devuelve lo que haya en vez de inventarse un nombre bonito: un nombre equivocado es peor
     * que uno feo.
     */
    private fun legibleDe(uri: String): String = try {
        val id = DocumentsContract.getTreeDocumentId(Uri.parse(uri))
        // `primary:` es el almacenamiento interno; para el resto se deja el prefijo, que ahí
        // sí distingue de dónde sale.
        if (id.startsWith("primary:")) "/" + id.removePrefix("primary:") else id
    } catch (e: Throwable) {
        uri
    }

    private fun titulo(t: String, tam: Float) = TextView(this).apply {
        text = t
        textSize = tam
        setTextColor(getColor(R.color.texto))
        setPadding(0, 16, 0, 8)
    }

    private fun nota(t: String) = TextView(this).apply {
        text = t
        textSize = 13f
        setTextColor(getColor(R.color.apagado))
        setPadding(0, 4, 0, 12)
    }

    private fun casilla(t: String, alCambiar: (Boolean) -> Unit) = CheckBox(this).apply {
        text = t
        textSize = 14f
        setTextColor(getColor(R.color.texto))
        layoutParams = LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.WRAP_CONTENT,
        )
        setOnClickListener { alCambiar(isChecked) }
    }

    private fun pintar() {
        activa.isChecked = Carga.activa(this)
        soloWifi.isChecked = Carga.soloWifi(this)
        soloCargando.isChecked = Carga.soloCargando(this)
        existentes.isChecked = Carga.tambienExistentes(this)
        porFecha.isChecked = Carga.porFecha(this)

        // El nombre legible, no el `content://` entero: lo que salia era una linea de URI que
        // no dice nada y ocupa cuatro renglones.
        origen.text = Carga.origen(this)?.let { "Carpeta del teléfono: ${legibleDe(it)}" }
            ?: "Carpeta del teléfono: sin elegir"
        destino.text = "Carpeta en el volumen: ${Carga.destino(this)}"

        estado.text = when {
            !Carga.activa(this) -> "Apagada. Nada sube solo."
            // No se puede subir sin llave guardada, y decir que está activa sería mentir.
            !Llave.hay(this) -> "Activa, pero sin la llave guardada: vuelve a activarla."
            Carga.origen(this) == null -> "Activa, pero falta elegir la carpeta del teléfono."
            // Decir «subirá» sin decir **qué** dejó a quien la activó mirando una carpeta donde
            // no pasaba nada. Con lo de abajo apagado, lo que ya estaba no sube: es lo correcto
            // --encenderla no debe subir diez años de fotos-- pero hay que decirlo donde se lee.
            Carga.tambienExistentes(this) ->
                "Activa. Subirá todo lo que haya en la carpeta, y lo que vaya apareciendo."
            else ->
                "Activa. Subirá las fotos nuevas, no las que ya estaban. Para esas, marca " +
                    "«Subir también lo que ya está en la carpeta»."
        }
    }

    /** Lo que paso en la ultima pasada, debajo del estado. Vacio mientras no haya habido. */
    private fun pintarParte() {
        val p = Subidos.parte(this)
        parte.visibility = if (p == null) View.GONE else View.VISIBLE
        parte.text = p?.let { "Última comprobación: $it" } ?: ""
    }

    /**
     * Pregunta antes de guardar la llave, diciendo **qué cambia**.
     *
     * No es un trámite. Es la diferencia entre «solo tú puedes abrir esto» y «quien tenga tu
     * teléfono desbloqueado puede», y quien activa la casilla tiene derecho a saberlo antes.
     */
    private fun confirmarClave() {
        val campo = android.widget.EditText(this).apply {
            hint = "Frase de paso del volumen"
            inputType = android.text.InputType.TYPE_CLASS_TEXT or
                android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD
            setTextColor(getColor(R.color.texto))
        }
        AlertDialog.Builder(this)
            .setTitle("Hay que guardar la llave del volumen")
            .setMessage(
                "Para subir sin ti delante, MultiDrive tiene que poder abrir el volumen sin " +
                    "ti delante, y para eso hace falta guardar tu frase de paso en el " +
                    "teléfono.\n\n" +
                    "Se guarda cifrada con una clave que vive dentro del almacén protegido por " +
                    "hardware de Android: no sale de este teléfono y solo se puede usar con la " +
                    "pantalla desbloqueada.\n\n" +
                    "Lo que cambia, y es el trato: hasta ahora abrir el volumen exigía que la " +
                    "escribieras. A partir de ahora, quien tenga tu teléfono desbloqueado " +
                    "puede abrirlo.\n\n" +
                    "Escríbela para comprobar que es la correcta antes de guardarla."
            )
            .setView(campo)
            .setNegativeButton("No, gracias") { _, _ ->
                activa.isChecked = false
                pintar()
            }
            .setPositiveButton("Guardar la llave") { _, _ ->
                comprobarYGuardar(campo.text.toString())
            }
            .show()
    }

    /**
     * Comprueba la frase **abriendo el volumen con ella** antes de guardarla.
     *
     * Cuesta unos segundos y vale la pena: una frase mal escrita se guardaría igual de bien, y
     * el fallo no aparecería hasta la primera subida automática, de madrugada, sin nadie
     * delante y sin forma de relacionarlo con esta pantalla.
     */
    private fun comprobarYGuardar(frase: String) {
        if (frase.isEmpty()) {
            activa.isChecked = false
            estado.text = "Hace falta la frase de paso."
            return
        }
        estado.text = "Comprobando la frase…"
        activa.isEnabled = false
        val conf = java.io.File(filesDir, "multidrive.conf").absolutePath

        kotlin.concurrent.thread {
            val dicho = try {
                // Se abre solo para comprobar y se cierra enseguida: no se toca la sesión que
                // el usuario pueda tener abierta, que es otra distinta.
                val s = Nucleo.abrir(conf, frase, false)
                Nucleo.cerrar(s)
                if (Llave.guardar(this, frase)) null else "No se pudo guardar la llave en este teléfono."
            } catch (e: Throwable) {
                "No se pudo abrir el volumen con esa frase. No se ha guardado nada."
            }
            runOnUiThread {
                activa.isEnabled = true
                Carga.activa(this, dicho == null)
                if (dicho == null) {
                    // Desde ahora cuenta como «nuevo». Sin esto, encenderla subiria el carrete
                    // entero sin que nadie lo haya pedido.
                    Subidos.empiezaAhora(this)
                }
                Subidor.programar(this)
                if (dicho != null) estado.text = dicho
                pintar()
            }
        }
    }

    private fun apagar() {
        Carga.activa(this, false)
        Subidor.programar(this)
        Subidos.olvidar(this)
        // La llave se borra al apagar. Dejarla guardada «por si acaso» sería quedarse con algo
        // que ya no hace falta, y lo que no hace falta no se guarda.
        Llave.borrar(this)
        pintar()
    }

    /**
     * Elige la carpeta local con el selector del sistema.
     *
     * Se usa el selector y no una lista nuestra porque es lo que concede el permiso: Android no
     * deja leer una carpeta porque sepamos su nombre, sino porque el usuario la señaló.
     */
    private fun elegirCarpeta() {
        val i = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
        i.addFlags(
            Intent.FLAG_GRANT_READ_URI_PERMISSION or
                Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
        )
        @Suppress("DEPRECATION")
        startActivityForResult(i, PEDIR_CARPETA)
    }

    @Deprecated("El reemplazo moderno exige androidx, que esta aplicacion todavia no usa")
    override fun onActivityResult(peticion: Int, resultado: Int, datos: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(peticion, resultado, datos)
        if (peticion != PEDIR_CARPETA || resultado != RESULT_OK) return
        val uri: Uri = datos?.data ?: return
        // El permiso tiene que sobrevivir a reiniciar el teléfono: la carga ocurre después, sin
        // nadie delante, y sin esto Android lo habría olvidado para entonces.
        contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        Carga.origen(this, uri.toString())
        // Elegir carpeta tambien dispara una pasada: hasta ahora no habia donde mirar.
        Subidor.ahora(this)
        pintar()
    }

    private companion object {
        const val PEDIR_CARPETA = 7301
    }
}
