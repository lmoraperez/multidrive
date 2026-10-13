package net.multidrive

import android.app.Activity
import android.app.AlertDialog
import android.os.Bundle
import android.provider.DocumentsContract
import android.util.Log
import android.view.View
import android.widget.Button
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
 * Meter en el volumen las cuentas enlazadas que todavia no lo forman.
 *
 * # Lo que esta pantalla existe para decir
 *
 * «Anado una cuenta y tengo mas sitio» es verdad en **un** caso y mentira en los demas, y
 * enterarse despues de esperar seria la peor manera de enterarse:
 *
 * - En un volumen **`lineal`** cada trozo va entero a una cuenta. Llega otra, se le muda la
 *   fraccion que le toca, y el volumen **crece**.
 * - En espejo, paridad o union cada trozo se parte entre todas. Una cuenta mas no agranda
 *   nada: da **margen de caidas**. Para ganar sitio habria que repartir otra vez todos los
 *   trozos, o sea reescribir el volumen entero, que puede tardar horas y se hace desde el
 *   escritorio.
 *
 * Por eso se mira antes y se ensena lo que se gana **con su nombre**, en vez de un boton
 * «ampliar» que cumple la mitad de las veces.
 *
 * # Por que se enlaza desde aqui
 *
 * Porque es donde se descubre que hace falta. Quien entra y lee «no hay ninguna cuenta que el
 * volumen no use ya» tiene que poder enlazar una sin salir, volver y seguir; mandarlo a la
 * pantalla anterior a buscar un boton es hacerle recordar a donde iba.
 */
class Ampliar : Activity() {

    private lateinit var estado: TextView
    private lateinit var detalle: TextView
    private lateinit var drive: Button
    private lateinit var onedrive: Button
    private lateinit var cancelar: Button
    /** El titulo y la nota de la seccion de enlazar: se esconden con sus botones. */
    private lateinit var tituloSinUsar: View
    private lateinit var lista: LinearLayout
    private lateinit var tituloEnlazar: View
    private lateinit var notaEnlazar: View
    private lateinit var boton: Button
    private lateinit var conf: File

    /** Lo que se gana: `"espacio"`, `"margen"`, o nada si todavia no se sabe o no se puede. */
    private var gana: String? = null

    /** Esperando al navegador. Mientras tanto no se ofrece nada mas. */
    private var ocupado = false

    /** Lo que declara cada cuenta sobre su espacio. Vacio hasta que contesten. */
    private var cuotas: Map<String, Cuota> = emptyMap()

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)

        estado = TextView(this).apply {
            textSize = 15f
            setTextColor(getColor(R.color.texto))
        }
        detalle = TextView(this).apply {
            textSize = 14f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 18, 0, 0)
            visibility = View.GONE
        }

        drive = boton("Conectar Google Drive").apply {
            setOnClickListener { conectar("GDRIVE") }
        }
        onedrive = boton("Conectar OneDrive").apply {
            setOnClickListener { conectar("ONEDRIVE") }
        }
        // La salida mientras se espera al navegador, igual que en crear y en abrir. Sin ella,
        // quien vea que algo se tuerce no tiene mas recurso que el boton atras del navegador,
        // que reenvia la misma peticion y la deja inservible para siempre.
        cancelar = boton("Cancelar y volver").apply {
            visibility = View.GONE
            setOnClickListener { Nucleo.cancelarConexion() }
        }

        tituloSinUsar = tituloDeSeccion("Cuentas que el volumen no usa")
        lista = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        tituloEnlazar = tituloDeSeccion("Enlazar otra cuenta")
        notaEnlazar = nota(
            "Enlazarla aquí no la mete en el volumen: eso lo hace el botón de abajo, y antes " +
                "te dice qué se gana."
        )

        // Abajo del todo, y a proposito: es la accion que escribe en todas las cuentas y
        // tarda. Se llega a el despues de leer lo que se gana, no antes.
        boton = botonPrincipal("Ampliar el volumen").apply {
            visibility = View.GONE
            setOnClickListener { confirmar() }
        }

        val caja = LinearLayout(this).comoPantalla().apply {
            addView(cabecera("Ampliar"))
            addView(tituloSinUsar)
            addView(estado)
            addView(lista)
            addView(detalle)
            addView(tituloEnlazar)
            addView(notaEnlazar)
            addView(drive)
            addView(onedrive)
            addView(cancelar)
            addView(boton)
        }
        setContentView(ScrollView(this).apply { addView(caja) })

        conf = File(filesDir, "multidrive.conf")
        mirar()
    }

    private fun conectar(tipo: String) {
        ocupado = true
        pintarOcupado(MIENTRAS_NAVEGADOR)

        thread {
            val dicho = try {
                "Cuenta conectada como «${Nucleo.conectar(tipo, conf.absolutePath, VOLVER_A)}»."
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo conectar", e)
                dicho(e)
            }
            runOnUiThread {
                ocupado = false
                estado.text = dicho
                // Y se vuelve a mirar: con una cuenta mas, lo que se gana es otra cosa.
                mirar()
            }
        }
    }

    /** Mientras se espera al navegador no se ofrece nada: solo cancelar. */
    private fun pintarOcupado(dicho: String) {
        estado.text = dicho
        detalle.visibility = View.GONE
        tituloEnlazar.visibility = View.GONE
        notaEnlazar.visibility = View.GONE
        drive.visibility = View.GONE
        onedrive.visibility = View.GONE
        boton.visibility = View.GONE
        cancelar.visibility = View.VISIBLE
    }

    /** Pregunta que se ganaria. Cuesta una peticion por cuenta nueva: hilo de fondo. */
    private fun mirar() {
        cancelar.visibility = View.GONE
        tituloEnlazar.visibility = View.VISIBLE
        notaEnlazar.visibility = View.VISIBLE
        drive.visibility = View.VISIBLE
        onedrive.visibility = View.VISIBLE

        val s = Sesion.id
        if (s == null) {
            estado.text = "El volumen está cerrado. Ábrelo y vuelve aquí."
            tituloEnlazar.visibility = View.GONE
            notaEnlazar.visibility = View.GONE
            drive.visibility = View.GONE
            onedrive.visibility = View.GONE
            return
        }
        estado.text = "Mirando qué cuentas hay sin usar y qué se ganaría…"

        thread {
            val r = try {
                JSONObject(Nucleo.mirarAmpliacion(s, conf.absolutePath))
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo mirar la ampliacion", e)
                runOnUiThread { estado.text = dicho(e) }
                return@thread
            }
            runOnUiThread { pintar(r) }
        }
    }

    private fun pintar(r: JSONObject) {
        val nuevos = r.optJSONArray("nuevos")?.let { a: JSONArray ->
            (0 until a.length()).map { a.getString(it) }
        } ?: emptyList()

        val problema = if (r.isNull("problema")) null else r.optString("problema")
        if (problema != null) {
            estado.text = problema
            detalle.visibility = View.GONE
            boton.visibility = View.GONE
            // Sin lista no hay encabezado: uno solo, sobre un mensaje, hace buscar una lista
            // que no está. Los botones de enlazar sí se quedan: son la salida de aquí.
            tituloSinUsar.visibility = View.GONE
            lista.removeAllViews()
            return
        }

        gana = r.getString("gana")
        val antes = r.getString("antes")
        val despues = r.getString("despues")
        val suma = if (r.isNull("suma")) null else r.getLong("suma")

        estado.text = if (nuevos.size == 1) "Sin usar: una cuenta." else "Sin usar: ${nuevos.size} cuentas."
        pintarLista(nuevos)
        detalle.visibility = View.VISIBLE
        boton.visibility = View.VISIBLE

        detalle.text = buildString {
            append("La topología pasa de «$antes» a «$despues».\n\n")
            if (gana == "espacio") {
                append("Ganas espacio")
                // Sin cifra se dice que no se sabe, en vez de callar y dejar que se suponga.
                // Una cuenta que no declara cupo no es una cuenta de cero.
                if (suma != null && suma > 0) append(": unos ${legible(suma)} más")
                else append(", pero esas cuentas no declaran cuánto tienen libre")
                append(".\n\nSe mueve a la cuenta nueva la fracción que le toca. El volumen " +
                    "se sigue leyendo mientras tanto.")
            } else {
                // Lo contrario de lo que se espera: hay que decirlo con todas las letras.
                append("No ganas espacio. En «$antes» cada trozo se parte entre todas las " +
                    "cuentas, así que una más no agranda el volumen: lo que ganas es aguantar " +
                    "una caída más.\n\nPara ganar sitio habría que repartir otra vez todos los " +
                    "trozos entre más cuentas, que es reescribir el volumen entero. Eso puede " +
                    "tardar horas y se hace desde el escritorio.")
            }
        }
    }

    /**
     * Las cuentas que el volumen no usa, cada una con su barra de espacio.
     *
     * Las cifras se piden aparte y llegan después: la lista sale enseguida con los nombres y
     * las barras aparecen cuando contesten. Esperar a tenerlas para enseñar los nombres haría
     * mirar una pantalla vacía durante segundos.
     */
    private fun pintarLista(nombres: List<String>) {
        lista.removeAllViews()
        tituloSinUsar.visibility = View.VISIBLE

        for (n in nombres) {
            lista.addView(
                LinearLayout(this).apply {
                    orientation = LinearLayout.VERTICAL
                    setPadding(0, 12, 0, 4)
                    addView(
                        TextView(this@Ampliar).apply {
                            text = n
                            textSize = 15f
                            setTextColor(getColor(R.color.texto))
                        }
                    )
                    addView(barraDeEspacio(cuotas[n], cuotas.isEmpty()))
                }
            )
        }
        lista.addView(
            nota("Lo ocupado es de la cuenta entera, no solo lo de MultiDrive.")
        )

        if (cuotas.isEmpty()) pedirCuotas(nombres)
    }

    /** Pregunta a cada cuenta cuánto le queda, y repinta la lista cuando contesten. */
    private fun pedirCuotas(nombres: List<String>) {
        thread {
            val leidas = try {
                leerCuotas(conf.absolutePath)
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudieron leer las cuotas", e)
                nombres.associateWith { Cuota(null, null, "No se pudo preguntar.") }
            }
            runOnUiThread {
                cuotas = leidas
                // Solo si la lista sigue en pantalla: entre la pregunta y la respuesta se pudo
                // ampliar ya, y entonces esta lista no describe nada.
                if (tituloSinUsar.visibility == View.VISIBLE) pintarLista(nombres)
            }
        }
    }

    private fun confirmar() {
        val queGana = if (gana == "espacio") "más espacio" else "una caída más de margen"
        AlertDialog.Builder(this)
            .setTitle("¿Ampliar el volumen?")
            .setMessage(
                "Se escribe en todas tus cuentas y tarda. Al terminar tendrás $queGana.\n\n" +
                    "Si algo falla a mitad no se cambia nada: el volumen sigue como está."
            )
            .setPositiveButton("Ampliar") { _, _ -> hacer() }
            .setNegativeButton("No", null)
            .show()
    }

    private fun hacer() {
        val s = Sesion.id ?: return
        boton.isEnabled = false
        detalle.visibility = View.GONE
        tituloEnlazar.visibility = View.GONE
        notaEnlazar.visibility = View.GONE
        drive.visibility = View.GONE
        onedrive.visibility = View.GONE
        estado.text = "Ampliando. Esto tarda: hay que escribir en cada cuenta…"

        thread {
            val dicho = try {
                val r = JSONObject(Nucleo.ampliar(s, conf.absolutePath))
                val anadidas = r.getJSONArray("anadidas").let { a: JSONArray ->
                    (0 until a.length()).map { a.getString(it) }
                }

                // La sesión abierta se quedó con la cabecera vieja, así que **se cierra**. No
                // es una precaución de más: seguir leyendo con la cabecera de antes buscaría
                // los trozos donde ya no están.
                Sesion.cerrar()
                contentResolver.notifyChange(DocumentsContract.buildRootsUri(AUTORIDAD), null)

                val suma = if (r.isNull("suma")) null else r.getLong("suma")
                val creció = r.getString("gana") == "espacio"

                buildString {
                    append("Volumen ampliado con ${anadidas.joinToString(", ")}.\n\n")
                    append("Ahora es «${r.getString("despues")}»")
                    // La cifra es lo que convierte «ahora es lineal:3» en algo que dice qué
                    // ganaste. Y en margen se dice que no creció, que es lo que pasó.
                    append(
                        when {
                            !creció -> ", y aguanta una caída más. El tamaño no cambia: en " +
                                "esta topología cada trozo se parte entre todas las cuentas"
                            suma == null || suma == 0L -> ", pero esas cuentas no declaran " +
                                "cuánto espacio aportan, así que no puedo decirte cuánto creció"
                            else -> ", con ${legible(suma)} más de espacio"
                        }
                    )
                    append(".\n\nSe cerró el volumen para que vuelva a leerse con la cabecera ")
                    append("nueva. Ábrelo otra vez con tu frase de paso.")
                }
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo ampliar", e)
                dicho(e)
            }
            runOnUiThread { terminar(dicho) }
        }
    }

    /** Enseña cómo quedó y deja salir. */
    private fun terminar(dicho: String) {
        Sesion.ultimoAviso = dicho
        tituloSinUsar.visibility = View.GONE
        lista.removeAllViews()
        estado.text = dicho
        boton.isEnabled = true
        boton.text = "Volver"
        boton.visibility = View.VISIBLE
        boton.setOnClickListener { finish() }
    }
}
