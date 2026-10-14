package net.multidrive

import android.app.Activity
import android.app.AlertDialog
import android.os.Bundle
import android.provider.DocumentsContract
import android.text.Editable
import android.text.InputType
import android.text.TextWatcher
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
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
 * Cuentas que hacen falta para poder crear un volumen.
 *
 * **Una.** Con una sola la unica topologia que cabe es `concat`: cifra y trocea igual --los
 * nombres de fichero incluidos-- pero no reparte nada y **no aguanta ninguna caida**. Si esa
 * cuenta se pierde, se pierde el volumen entero.
 *
 * Se deja pasar porque es un uso legitimo --cifrar lo que subes a una nube ya vale por si
 * solo-- y porque negarlo obligaba a enlazar una segunda cuenta solo para poder mirar. Lo que
 * **no** se hace es callarlo: `concat` sale en la lista con «No aguanta perder ninguna
 * cuenta: si falla una, se pierde todo» al lado, y el dialogo de confirmar lo repite.
 *
 * Estuvo en 2 un dia. El numero no es lo que protege: lo que protege es el aviso.
 */
private const val MINIMO = 1

/**
 * Crear el volumen desde el telefono.
 *
 * # Por que el movil tambien crea
 *
 * Porque si no, la primera vez hay que ir a buscar un ordenador. Quien instala esto en el
 * telefono ya tiene ahi sus cuentas --son las del propio telefono-- y pedirle un escritorio
 * para empezar es pedirle que no empiece.
 *
 * # Por que la lista se mide antes de ensenarla
 *
 * Porque «aprovechas el 50 %» no dice si te quedan tres gigas o trescientos, y con cuentas
 * desiguales la cifra sorprende: manda la mas apretada, asi que 15 GiB y 3,4 GiB dan 3,4 en
 * espejo y 6,8 en union, no 18,4. Esa sorpresa hay que llevarsela **antes** de crear, porque
 * despues cambiar de topologia es mover todo el volumen.
 *
 * Cuesta una peticion por cuenta, una sola vez al abrir esta pantalla.
 */
class Crear : Activity() {

    private lateinit var estado: TextView
    private lateinit var lista: LinearLayout
    private lateinit var frase: EditText
    private lateinit var otraVez: EditText
    private lateinit var coinciden: TextView
    private lateinit var boton: Button
    private lateinit var avisoFrase: TextView
    private lateinit var pasoCuentas: LinearLayout
    private lateinit var cuentas: TextView
    private lateinit var drive: Button
    private lateinit var onedrive: Button
    private lateinit var seguir: Button
    private lateinit var cancelar: Button
    private lateinit var flotante: TextView
    private lateinit var cajaFlotante: LinearLayout
    private lateinit var conf: File

    /** Las cuentas que ya estaban enlazadas al entrar. Las de aquí no se tocan al salir. */
    private var yaEstaban: Set<String> = emptySet()

    /** Si el volumen llegó a crearse. Mientras sea falso, salir deshace lo enlazado aquí. */
    private var creado = false

    @Volatile
    private var ocupado = false

    private var opciones: List<Opcion> = emptyList()
    private var elegida: Int = -1

    private data class Opcion(
        val spec: String,
        val tolera: Int,
        val sinRed: Boolean,
        val suma: Boolean,
        val libre: Long?,
        val sinMedir: List<String>,
    )

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)

        val caja = LinearLayout(this).comoPantalla()
        caja.addView(cabecera("Crear volumen"))

        estado = TextView(this).apply {
            textSize = 14f
            setTextColor(getColor(R.color.texto))
            setPadding(0, 0, 0, 16)
        }
        caja.addView(estado)

        // --- Paso 1: las cuentas. Un volumen es el reparto entre varias, asi que lo primero
        // es tenerlas. Se enlazan **todas** aqui y no de una en una mas tarde: anadir una
        // cuenta a un volumen ya creado mueve datos, y eso hoy solo esta en el escritorio.
        cuentas = TextView(this).apply {
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 0, 0, 12)
        }
        drive = botonDeProveedor("Conectar Google Drive", R.drawable.ic_drive).apply {
            setOnClickListener { conectar("GDRIVE") }
        }
        onedrive = botonDeProveedor("Conectar OneDrive", R.drawable.ic_onedrive).apply {
            setOnClickListener { conectar("ONEDRIVE") }
        }
        seguir = Button(this).apply {
            text = "Continuar"
            // Separado de los botones de conectar: es un paso distinto, no uno más de la
            // lista. Pegado a ellos se leía como una tercera cuenta que conectar.
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = 56 }
            setOnClickListener { alPasoDos() }
        }
        // La salida mientras se espera al navegador. Sin ella, quien vea que algo se tuerce no
        // tiene mas recurso que el boton atras del navegador, que reenvia la misma peticion y
        // la deja inservible para siempre.
        cancelar = Button(this).apply {
            text = "Cancelar y volver"
            visibility = View.GONE
            setOnClickListener { Nucleo.cancelarConexion() }
        }
        pasoCuentas = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(cuentas)
            addView(drive)
            addView(onedrive)
            addView(seguir)
        }
        caja.addView(pasoCuentas)

        // --- Paso 2: la topologia y la frase.
        lista = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            visibility = View.GONE
        }
        caja.addView(lista)

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
        otraVez = EditText(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            )
            hint = "Escríbela otra vez"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            setTextColor(getColor(R.color.texto))
            visibility = View.GONE
        }
        // Dos veces, y no por costumbre: la frase **es** la llave. No se guarda en ningun sitio
        // y no se puede recuperar, asi que una errata en el teclado del movil no da un error mas
        // tarde: da un volumen que ya nunca se abre, con la cabecera escrita en todas las nubes.
        val aviso = TextView(this).apply {
            text = "La frase de paso no se guarda en ninguna parte y no se puede recuperar. " +
                "Si la pierdes, se pierde todo lo que guardes en el volumen."
            textSize = 13f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 12, 0, 12)
            visibility = View.GONE
        }
        // Lo que pasa con las dos frases, debajo de ellas y no arriba del todo: quien está
        // escribiendo mira el teclado y los campos, no la cabecera de la pantalla.
        coinciden = TextView(this).apply {
            textSize = 13f
            setPadding(0, 10, 0, 0)
            visibility = View.GONE
        }
        boton = Button(this).apply {
            text = "Crear volumen"
            visibility = View.GONE
            isEnabled = false
            setOnClickListener { confirmar() }
        }

        // Se revisa a cada tecla, no al pulsar. Enterarte de que no coinciden **después** de
        // darle al botón es enterarte tarde: para entonces ya creías haber terminado.
        val vigilante = object : TextWatcher {
            override fun afterTextChanged(s: Editable?) = revisarFrases()
            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
        }
        frase.addTextChangedListener(vigilante)
        otraVez.addTextChangedListener(vigilante)

        caja.addView(frase)
        caja.addView(otraVez)
        caja.addView(coinciden)
        caja.addView(aviso)
        caja.addView(boton)
        avisoFrase = aviso

        // El aviso va **fuera** del ScrollView, anclado abajo.
        //
        // El texto de estado vive arriba del todo y los botones estan mas abajo, asi que al
        // pulsar «Crear» el mensaje de «creando, son unos segundos» quedaba fuera de la
        // pantalla: habia que subir para saber si estaba pasando algo. Igual que en la
        // pantalla principal, y por lo mismo.
        flotante = TextView(this).apply {
            textSize = 14f
            setTextColor(getColor(R.color.texto))
        }
        cajaFlotante = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundResource(R.drawable.aviso_flotante)
            setPadding(40, 40, 40, 40)
            elevation = 8f
            visibility = View.GONE
            addView(flotante)
            addView(cancelar)
            setOnClickListener { avisar(null) }
            layoutParams = android.widget.FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply {
                gravity = Gravity.BOTTOM
                setMargins(40, 40, 40, 40)
            }
        }
        setContentView(
            android.widget.FrameLayout(this).apply {
                addView(ScrollView(this@Crear).apply { addView(caja) })
                addView(cajaFlotante)
            }
        )

        conf = File(filesDir, "multidrive.conf")
        // Las que ya estaban **antes** de entrar aquí. Las que aparezcan a partir de ahora se
        // enlazaron para este volumen, y si el volumen no llega a existir no pintan nada.
        yaEstaban = enlazadas().toSet()
        pintarCuentas(null)
    }

    /**
     * Salir de aquí sin haber creado el volumen.
     *
     * Enlazar una cuenta es un paso de crear el volumen, no un fin en sí mismo: quien se echa
     * atrás a mitad volvía a la pantalla principal con cuentas que no había pedido tener. Pero
     * tampoco se tiran sin preguntar: el «atrás» se pulsa sin querer, y cada cuenta enlazada
     * costó una vuelta al navegador.
     *
     * Descartar el diálogo **se queda aquí**, que es lo que no pierde nada.
     */
    private fun salir() {
        val nuevas = enlazadas().filterNot { it in yaEstaban }
        if (creado || nuevas.isEmpty()) {
            finish()
            return
        }

        AlertDialog.Builder(this)
            .setTitle("No has creado el volumen")
            .setMessage(
                "Enlazaste ${nuevas.size} cuenta(s) y no llegaste a crear el volumen:\n" +
                    nuevas.joinToString("") { "  · $it\n" } +
                    "\nSi las quitas, el teléfono queda como estaba. El permiso en Google o " +
                    "en Microsoft no se retira: eso se hace desde tu cuenta."
            )
            .setPositiveButton("Quitarlas y salir") { _, _ ->
                quitar(nuevas)
                finish()
            }
            .setNegativeButton("Dejarlas enlazadas") { _, _ -> finish() }
            .show()
    }

    /**
     * Quita las cuentas, **antes** de salir y no al destruirse la pantalla.
     *
     * El orden importa y costó una pantalla mal pintada: cuando esta actividad termina,
     * Android ejecuta `onResume` de la principal **antes** que su propio `onDestroy`. Haciendo
     * esto en `onDestroy`, la principal leía las cuentas cuando todavía estaban y se quedaba
     * dibujada como si las hubiera, con la tarjeta puesta y ninguna cuenta dentro.
     *
     * Solo las de esta sesión: las que ya estaban al entrar son de quien las puso, y puede
     * haber un volumen abierto que dependa de ellas.
     */
    private fun quitar(nuevas: List<String>) {
        for (n in nuevas) {
            try {
                Nucleo.olvidar(conf.absolutePath, n)
                Log.i(ETIQUETA, "sin crear volumen: se quita la cuenta $n")
            } catch (e: Throwable) {
                // Que no se pueda quitar una no es motivo para dejar las demás: lo que se
                // busca es no dejar cuentas sueltas, y parar a la mitad deja más.
                Log.w(ETIQUETA, "no se pudo quitar $n al salir", e)
            }
        }
    }

    /**
     * Atras vuelve al paso de las cuentas antes de salir.
     *
     * Sin esto, quien llega a la lista de topologias y se da cuenta de que le falta una cuenta
     * tiene que salir de la pantalla entera y empezar otra vez.
     */
    @Deprecated("El reemplazo moderno exige androidx, que esta aplicacion todavia no usa")
    override fun onBackPressed() {
        if (lista.visibility != View.VISIBLE) {
            // Y al salir del todo se pregunta por lo que quede enlazado sin volumen.
            salir()
            return
        }
        lista.visibility = View.GONE
        frase.visibility = View.GONE
        otraVez.visibility = View.GONE
        avisoFrase.visibility = View.GONE
        boton.visibility = View.GONE
        elegida = -1
        pasoCuentas.visibility = View.VISIBLE
        pintarCuentas(null)
    }

    private fun enlazadas(): List<String> = try {
        val a = JSONArray(Nucleo.cuentas(conf.absolutePath))
        (0 until a.length()).map { a.getString(it) }
    } catch (e: Throwable) {
        Log.w(ETIQUETA, "no se pudieron leer las cuentas", e)
        emptyList()
    }

    private fun pintarCuentas(dicho: String?) {
        val hay = enlazadas()
        cuentas.text = if (hay.isEmpty()) {
            "Ninguna todavía."
        } else {
            "${hay.size} enlazada(s): ${hay.joinToString(", ")}"
        }
        // Mientras se espera al navegador se esconde lo demas, por lo mismo que en la pantalla
        // de abrir: ofrecer botones que no responden hace creer que la aplicacion esta rota.
        drive.visibility = if (ocupado) View.GONE else View.VISIBLE
        onedrive.visibility = if (ocupado) View.GONE else View.VISIBLE
        seguir.visibility = if (ocupado) View.GONE else View.VISIBLE
        drive.isEnabled = !ocupado
        onedrive.isEnabled = !ocupado
        seguir.isEnabled = hay.size >= MINIMO && !ocupado
        cancelar.visibility = if (ocupado) View.VISIBLE else View.GONE

        if (ocupado) {
            estado.text = dicho ?: "Esperando a que des permiso en el navegador."
            return
        }
        estado.text = dicho ?: when {
            hay.isEmpty() ->
                "Conecta las cuentas entre las que quieres repartir tus archivos. Cuantas más, " +
                    "más espacio y más caídas aguanta."
            // Con una sola se puede seguir, pero conviene saber a qué: la única topología
            // posible no aguanta perder esa cuenta. Decirlo aquí y no solo en la lista.
            hay.size == 1 ->
                "Con una sola cuenta el volumen se cifra igual, pero no aguanta perderla. " +
                    "Conecta otra si quieres margen, o continúa así."
            else ->
                "Conecta las que falten y continúa."
        }
    }

    private fun conectar(tipo: String) {
        ocupado = true
        pintarCuentas(MIENTRAS_NAVEGADOR)
        thread {
            val dicho = try {
                "Cuenta conectada como «${Nucleo.conectar(tipo, conf.absolutePath, VOLVER_A)}»."
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo conectar", e)
                dicho(e)
            }
            runOnUiThread {
                ocupado = false
                pintarCuentas(dicho)
            }
        }
    }

    private fun alPasoDos() {
        pasoCuentas.visibility = View.GONE
        lista.visibility = View.VISIBLE
        estado.text = "Preguntando a tus cuentas cuánto espacio hay…"
        cargar()
    }

    /** Pregunta a cada cuenta cuanto le queda. Cuesta segundos: nunca en el hilo de la interfaz. */
    private fun cargar() {
        thread {
            val (leidas, fallo) = try {
                leer() to null
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudieron calcular las topologias", e)
                emptyList<Opcion>() to (dicho(e))
            }
            runOnUiThread {
                opciones = leidas
                if (fallo != null) {
                    estado.text = "No se pudo preparar la lista: $fallo"
                    return@runOnUiThread
                }
                if (leidas.isEmpty()) {
                    estado.text = "No hay ninguna cuenta enlazada. Conecta una primero."
                    return@runOnUiThread
                }
                estado.text = "Elige cómo se reparten tus archivos entre las cuentas. " +
                    "Esto no se puede cambiar luego desde el móvil."
                pintarLista()
            }
        }
    }

    private fun leer(): List<Opcion> {
        val json = JSONArray(Nucleo.topologias(conf.absolutePath))
        return (0 until json.length()).map { i ->
            val o: JSONObject = json.getJSONObject(i)
            Opcion(
                spec = o.getString("spec"),
                tolera = o.getInt("tolera"),
                sinRed = o.getBoolean("sin_red"),
                suma = o.getBoolean("suma"),
                libre = if (o.isNull("libre")) null else o.getLong("libre"),
                sinMedir = o.getJSONArray("sin_medir").let { a ->
                    (0 until a.length()).map { a.getString(it) }
                },
            )
        }
    }

    private fun pintarLista() {
        lista.removeAllViews()
        for ((i, o) in opciones.withIndex()) {
            lista.addView(tarjeta(i, o))
        }
    }

    private fun tarjeta(i: Int, o: Opcion): View {
        val elegido = i == elegida
        val caja = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(28, 24, 28, 24)
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ).apply { bottomMargin = 16 }
            setBackgroundColor(getColor(if (elegido) R.color.linea else R.color.panel))
            setOnClickListener {
                elegida = i
                pintarLista()
                frase.visibility = View.VISIBLE
                otraVez.visibility = View.VISIBLE
                avisoFrase.visibility = View.VISIBLE
                boton.visibility = View.VISIBLE
            }
        }

        caja.addView(
            TextView(this).apply {
                text = o.spec
                textSize = 17f
                setTextColor(getColor(if (elegido) R.color.acento else R.color.texto))
            }
        )
        caja.addView(
            TextView(this).apply {
                // El aviso va con todas las letras. Elegir esto sin enterarse es perder el
                // volumen entero el dia que una cuenta caiga o alguien borre una carpeta.
                text = if (o.sinRed) {
                    "No aguanta perder ninguna cuenta: si falla una, se pierde todo."
                } else {
                    "Aguanta perder ${o.tolera} cuenta(s)."
                }
                textSize = 13f
                setTextColor(getColor(if (o.sinRed) R.color.texto else R.color.apagado))
                setPadding(0, 6, 0, 0)
            }
        )
        caja.addView(
            TextView(this).apply {
                text = espacio(o)
                textSize = 13f
                setTextColor(getColor(R.color.apagado))
                setPadding(0, 4, 0, 0)
            }
        )
        return caja
    }

    /**
     * Lo que da esta opcion, **diciendo cuando la cifra no es una medida**.
     *
     * Una cuenta que no declara cupo --un bucket de S3 se factura por lo que ocupa y no se
     * llena-- deja la cifra coja, y de dos maneras contrarias segun la topologia: donde se
     * parte cada trozo entre todas, manda la mas apretada y lo que sale es un **techo**; donde
     * cada trozo va entero a una cuenta, la cifra es la suma y lo que falta se queda **corto**.
     * Dar el mismo numero en los dos casos sin decir cual es seria dar un numero inventado.
     */
    private fun espacio(o: Opcion): String {
        val libre = o.libre ?: return "No se pudo saber cuánto espacio queda."
        if (o.sinMedir.isEmpty()) return "Espacio disponible: ${legible(libre)}."
        val quienes = o.sinMedir.joinToString(", ")
        return if (o.suma) {
            "Espacio disponible: ${legible(libre)} o más ($quienes no dice cuánto tiene)."
        } else {
            "Espacio disponible: ${legible(libre)} como mucho ($quienes no dice cuánto tiene)."
        }
    }

    /**
     * Pregunta antes de escribir en las nubes.
     *
     * Crear no se deshace desde aqui: deja una cabecera en cada cuenta, y el nucleo se niega a
     * poner otra encima porque una sal nueva dejaria inalcanzable todo lo anterior. Asi que lo
     * que se va a hacer se dice **con su nombre** antes de hacerlo.
     */
    /**
     * Mira si las dos frases coinciden, y enciende el botón solo si lo hacen.
     *
     * # Por qué calla mientras vas bien
     *
     * Decir «no coinciden» en cuanto el segundo campo tiene una letra es cierto y es inútil:
     * claro que no coinciden, llevas una letra. Así que mientras lo escrito sea el principio
     * de la primera frase no se dice nada, y en cuanto te desvías **sí**, en esa misma tecla.
     * No hay que terminar de escribirla para enterarte de que te equivocaste al empezar.
     */
    private fun revisarFrases() {
        val a = frase.text.toString()
        val b = otraVez.text.toString()

        val iguales = a.isNotEmpty() && a == b
        boton.isEnabled = iguales

        val texto = when {
            b.isEmpty() -> null
            iguales -> "Las dos coinciden."
            // Va bien hasta donde lleva escrito: todavía no hay nada que decir.
            a.startsWith(b) -> null
            else -> "Las dos frases no coinciden."
        }

        coinciden.text = texto ?: ""
        coinciden.visibility = if (texto == null) View.GONE else View.VISIBLE
        coinciden.setTextColor(getColor(if (iguales) R.color.acento else R.color.aviso))
    }

    private fun confirmar() {
        val o = opciones.getOrNull(elegida) ?: return
        // Las dos frases ya se comprobaron en `revisarFrases`, que es quien enciende este
        // botón. Se vuelve a mirar igualmente: esto escribe la llave del volumen en todas las
        // nubes, y una condición que vive en un solo sitio es la que un día se cae sola.
        val f = frase.text.toString()
        if (f.isEmpty() || f != otraVez.text.toString()) {
            estado.text = "Las dos frases no coinciden. Escríbelas otra vez."
            return
        }

        AlertDialog.Builder(this)
            .setTitle("¿Crear el volumen?")
            .setMessage(
                buildString {
                    append("Se creará con «${o.spec}».\n\n")
                    if (o.sinRed) {
                        append("Esta opción no aguanta perder ninguna cuenta: si falla una, se ")
                        append("pierde todo lo que haya en el volumen.\n\n")
                    }
                    append("Cambiar esto después no se puede desde el móvil.")
                }
            )
            .setNegativeButton("No", null)
            .setPositiveButton("Crear") { _, _ -> crear(o.spec, f) }
            .show()
    }

    /** Enseña algo donde se lea sin desplazarse. `null` lo quita. */
    private fun avisar(texto: String?) {
        if (texto.isNullOrEmpty()) {
            cajaFlotante.visibility = View.GONE
            return
        }
        flotante.text = texto
        cajaFlotante.visibility = View.VISIBLE
    }

    private fun crear(spec: String, f: String) {
        boton.isEnabled = false
        val mientras = "Creando el volumen. Son unos segundos: hay que escribir en cada cuenta…"
        estado.text = mientras
        avisar(mientras)

        thread {
            val resumen = try {
                val r = JSONObject(Nucleo.crear(conf.absolutePath, f, spec))
                Sesion.sinVolumen = false

                // **Se deja abierto.** Antes se volvia con el volumen cerrado y habia que
                // escribir la frase otra vez, en la pantalla de al lado, para usar lo que
                // acababas de crear. Pedir dos veces la misma frase con un minuto de
                // diferencia no protege de nada: quien la acaba de escribir es quien la
                // escribio.
                //
                // Si abrir fallara --no deberia, el volumen acaba de escribirse con esa misma
                // frase-- no se tira la creacion por eso: el volumen **existe**. Se vuelve
                // cerrado, que es lo que pasaba siempre antes, y se dice.
                val abierto = try {
                    Sesion.abrir(conf.absolutePath, f, true)
                    contentResolver.notifyChange(
                        DocumentsContract.buildRootsUri(AUTORIDAD),
                        null,
                    )
                    true
                } catch (e: Throwable) {
                    Log.w(ETIQUETA, "creado pero no se pudo abrir", e)
                    false
                }

                buildString {
                    append("Volumen creado con «${r.getString("topologia")}» en ")
                    append(r.getString("proveedores"))
                    append(".\n\nAguanta perder ${r.getInt("tolera")} cuenta(s).\n\n")
                    append(
                        if (abierto) "Queda abierto: ya sale en la app de Archivos.\n\n"
                        else "El volumen está creado, pero no se pudo abrir ahora. " +
                            "Ábrelo con tu frase de paso.\n\n"
                    )
                    append(r.getString("maleta"))
                }
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo crear el volumen", e)
                runOnUiThread { fallo(dicho(e)) }
                return@thread
            }
            runOnUiThread {
                // Creado: se vuelve a la pantalla principal y el resumen se cuenta alli.
                //
                // Quedarse aqui seria dejar a alguien en una pantalla de «crear volumen» con el
                // volumen ya creado, teniendo que adivinar que lo siguiente es salir. Y el
                // resumen no se pierde por el camino: dice donde quedo la configuracion
                // sellada, que es lo mas util que esta aplicacion llega a decir.
                Log.i(ETIQUETA, resumen)
                Sesion.ultimoAviso = resumen
                // El volumen existe: las cuentas son suyas y salir ya no las toca.
                creado = true
                finish()
            }
        }
    }

    /** No se pudo crear: se queda aqui, que es donde se puede volver a intentar. */
    private fun fallo(dicho: String) {
        boton.isEnabled = true
        estado.text = dicho
        avisar(dicho)
    }

}
