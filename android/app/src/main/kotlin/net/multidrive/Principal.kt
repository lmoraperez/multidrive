package net.multidrive

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.provider.DocumentsContract
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import net.multidrive.nucleo.Nucleo
import org.json.JSONArray
import java.io.File
import kotlin.concurrent.thread

private const val ETIQUETA = "multidrive"

/**
 * La pantalla de MultiDrive.
 *
 * # Dos pantallas en una
 *
 * - **Entrada**, cuando el telefono no tiene ninguna cuenta enlazada: dos caminos y nada mas,
 *   crear un volumen desde cero o abrir uno que ya existe.
 * - **Casa**, cuando ya hay cuentas: «Tus cuentas» y «El volumen», que es lo que se usa a
 *   diario.
 *
 * El reparto es por **hay cuentas o no**, no por volumen abierto o cerrado. Quien ya paso por
 * aqui una vez no tiene que volver a elegir camino cada vez que abre la aplicacion, y la
 * seccion del volumen ya dice si esta abierto.
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
    private lateinit var frase: EditText
    private lateinit var conf: File

    // Entrada.
    private lateinit var entrada: View
    private lateinit var nuevo: Button
    private lateinit var existente: Button

    // Tus cuentas.
    private lateinit var tituloCuentas: TextView
    private lateinit var tarjetaCuentas: View
    private lateinit var listaCuentas: LinearLayout
    private lateinit var notaCuentas: TextView
    private lateinit var drive: Button
    private lateinit var onedrive: Button

    // El volumen.
    private lateinit var tituloVolumen: TextView
    private lateinit var tarjetaVolumen: View
    private lateinit var notaFrase: TextView
    private lateinit var abrir: Button
    private lateinit var crear: Button
    private lateinit var ver: Button
    private lateinit var cerrar: Button

    /** Lo que se esta haciendo ahora mismo, si es que se esta haciendo algo. */
    @Volatile
    private var ocupado = false

    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)
        setContentView(R.layout.principal)

        estado = findViewById(R.id.estado)
        frase = findViewById(R.id.frase)

        entrada = findViewById(R.id.entrada)
        nuevo = findViewById(R.id.nuevo)
        existente = findViewById(R.id.existente)

        tituloCuentas = findViewById(R.id.titulo_cuentas)
        tarjetaCuentas = findViewById(R.id.tarjeta_cuentas)
        listaCuentas = findViewById(R.id.lista_cuentas)
        notaCuentas = findViewById(R.id.nota_cuentas)
        drive = findViewById(R.id.drive)
        onedrive = findViewById(R.id.onedrive)

        tituloVolumen = findViewById(R.id.titulo_volumen)
        tarjetaVolumen = findViewById(R.id.tarjeta_volumen)
        notaFrase = findViewById(R.id.nota_frase)
        abrir = findViewById(R.id.abrir)
        crear = findViewById(R.id.crear)
        ver = findViewById(R.id.ver)
        cerrar = findViewById(R.id.cerrar)

        conf = File(filesDir, "multidrive.conf")

        try {
            Nucleo.iniciar(applicationContext)
        } catch (e: Throwable) {
            estado.text = "No se pudo cargar el núcleo:\n\n$e"
            return
        }

        nuevo.setOnClickListener { startActivity(Intent(this, Crear::class.java)) }
        existente.setOnClickListener { startActivity(Intent(this, Abrir::class.java)) }
        crear.setOnClickListener { startActivity(Intent(this, Crear::class.java)) }

        drive.setOnClickListener { conectar("GDRIVE") }
        onedrive.setOnClickListener { conectar("ONEDRIVE") }
        abrir.setOnClickListener { abrirVolumen() }
        cerrar.setOnClickListener {
            Sesion.cerrar()
            avisarAlSistema()
            pintar("Volumen cerrado. Ya no se ve en Archivos.")
        }
        ver.setOnClickListener { abrirEnArchivos() }
    }

    /**
     * Abre el volumen en la app de Archivos, **para navegar**.
     *
     * # Por que no vale `ACTION_OPEN_DOCUMENT`
     *
     * Es lo que habia aqui antes, y por eso desde este boton no se podia borrar nada. Son dos
     * modos distintos de la misma aplicacion y desde fuera se parecen mucho:
     *
     * - `ACTION_OPEN_DOCUMENT` abre el **selector**. Su trabajo es que elijas un fichero y
     *   devolverselo a quien pregunto. Mientras esta en ese modo no ofrece borrar, ni renombrar,
     *   ni mover, **por muchas banderas que declare el proveedor**: no es que no nos crea, es que
     *   ahi esas acciones no existen.
     * - `ACTION_VIEW` sobre el `Uri` de la raiz la abre en modo **navegador**, que es la app de
     *   Archivos de verdad, y ahi si se miran las banderas de cada documento.
     *
     * # Por que se elige la aplicacion en vez de preguntar
     *
     * Porque no todas las que se ofrecen sirven. En el telefono de pruebas atienden dos: el
     * navegador de documentos del sistema --`documentsui`, el que implementa esto de verdad y
     * mira nuestras banderas-- y «Files» de Google, que se apunta al intent y luego enseña sus
     * propias categorias. Un dialogo donde una opcion funciona y la otra no es peor que no
     * preguntar: quien elige mal concluye que MultiDrive no deja borrar.
     *
     * Si no esta, se deja que Android pregunte, que es mejor que no hacer nada.
     *
     * # Si el telefono no sabe navegar por origenes
     *
     * No todos los fabricantes traen esto. Si nadie atiende el intent, se lleva a nuestra propia
     * pantalla y se dice por que, en vez de dejar un boton que no hace nada.
     */
    private fun abrirEnArchivos() {
        val i = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(
                DocumentsContract.buildRootUri(AUTORIDAD, RAIZ),
                "vnd.android.document/root",
            )
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        }
        packageManager.queryIntentActivities(i, 0)
            .firstOrNull { it.activityInfo.packageName.endsWith("documentsui") }
            ?.let { i.setPackage(it.activityInfo.packageName) }
        try {
            startActivity(i)
        } catch (e: android.content.ActivityNotFoundException) {
            Log.w(ETIQUETA, "este telefono no sabe navegar por origenes", e)
            Toast.makeText(
                this,
                "Este teléfono no trae navegador de archivos del sistema. Se abre el de MultiDrive.",
                Toast.LENGTH_LONG,
            ).show()
            startActivity(Intent(this, Archivos::class.java))
        }
    }

    override fun onResume() {
        super.onResume()
        // Al volver de otra pantalla --el navegador, crear, abrir, la app de Archivos-- el
        // estado puede ser otro: una cuenta recien conectada, un volumen recien creado, o un
        // proceso que Android mato por el camino.
        if (!ocupado) pintar(null)
    }

    /**
     * Deja la pantalla entera coherente con lo que hay. **Todo, siempre.**
     *
     * @param dicho qué contar arriba. `null` para que lo deduzca del estado.
     */
    private fun pintar(dicho: String?) {
        val nombres = cuentasEnlazadas()
        val hayCuentas = nombres.isNotEmpty()
        val abierto = Sesion.id != null

        // Entrada: solo cuando no hay nada. En cuanto hay una cuenta sobra, porque los dos
        // caminos que ofrece estan ya los dos disponibles mas abajo.
        entrada.visibility = if (hayCuentas) View.GONE else View.VISIBLE
        nuevo.isEnabled = !ocupado
        existente.isEnabled = !ocupado

        tituloCuentas.visibility = if (hayCuentas) View.VISIBLE else View.GONE
        tarjetaCuentas.visibility = if (hayCuentas) View.VISIBLE else View.GONE
        if (hayCuentas) pintarCuentas(nombres, abierto)

        drive.isEnabled = !ocupado
        onedrive.isEnabled = !ocupado
        notaCuentas.text = if (abierto) {
            // Conectar una cuenta mas con el volumen abierto no la mete en el volumen: el
            // volumen es el de las cuentas con las que se creo, y anadir otra exige reescribir
            // su cabecera y mover trozos. Decirlo aqui evita que alguien conecte una cuarta
            // cuenta esperando mas espacio y se quede esperandolo.
            "Conectar otra cuenta no la añade al volumen abierto: eso mueve datos y hoy solo " +
                "se hace desde el escritorio."
        } else {
            "¿Ya usas MultiDrive en otro equipo? Con una cuenta enlazada y tu frase de paso se " +
                "traen las demás desde «Abrir un volumen que ya existe»."
        }

        tituloVolumen.visibility = if (hayCuentas) View.VISIBLE else View.GONE
        tarjetaVolumen.visibility = if (hayCuentas) View.VISIBLE else View.GONE

        frase.visibility = if (!abierto) View.VISIBLE else View.GONE
        notaFrase.visibility = if (!abierto) View.VISIBLE else View.GONE
        abrir.visibility = if (!abierto) View.VISIBLE else View.GONE
        crear.visibility = if (!abierto) View.VISIBLE else View.GONE
        ver.visibility = if (abierto) View.VISIBLE else View.GONE
        cerrar.visibility = if (abierto) View.VISIBLE else View.GONE
        abrir.isEnabled = !ocupado
        crear.isEnabled = !ocupado

        estado.text = dicho ?: when {
            !hayCuentas -> "Empieza por uno de los dos caminos."
            abierto -> "Volumen abierto. Búscalo en la app de Archivos."
            else -> "Escribe tu frase de paso y abre el volumen."
        }
    }

    /** Una fila por cuenta, con su botón de quitar. */
    private fun pintarCuentas(nombres: List<String>, abierto: Boolean) {
        listaCuentas.removeAllViews()
        for (n in nombres) {
            val fila = LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(0, 10, 0, 10)
            }
            fila.addView(
                TextView(this).apply {
                    text = n
                    textSize = 15f
                    setTextColor(getColor(R.color.texto))
                    layoutParams =
                        LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
                }
            )
            // Con el volumen abierto no se ofrece quitar ninguna: el núcleo se negaría con las
            // que lo forman, y las que no lo forman no se distinguen desde aquí. Un botón que
            // casi siempre va a decir que no es peor que no tenerlo.
            if (!abierto) {
                fila.addView(
                    Button(this).apply {
                        text = "Quitar"
                        textSize = 13f
                        background = null
                        minWidth = 0
                        setTextColor(getColor(R.color.apagado))
                        setPadding(16, 0, 0, 0)
                        isEnabled = !ocupado
                        setOnClickListener { confirmarQuitar(n) }
                    }
                )
            }
            listaCuentas.addView(fila)
        }
    }

    /**
     * Pregunta antes de quitar, y dice **lo que quitar no hace**.
     *
     * Quitar una cuenta de aquí no la saca del volumen: solo borra unas claves de este teléfono.
     * Si esa cuenta formaba parte de un volumen, lo que se consigue es que ese volumen ya no
     * abra. Con el volumen cerrado no hay forma de saber si es el caso --habría que abrirlo--
     * así que lo honesto es decirlo y dejar decidir.
     */
    private fun confirmarQuitar(nombre: String) {
        AlertDialog.Builder(this)
            .setTitle("¿Quitar «$nombre» de este teléfono?")
            .setMessage(
                "Se borran sus claves de este teléfono. La cuenta y lo que hay en ella no se " +
                    "tocan.\n\nPero si forma parte de un volumen, ese volumen dejará de abrirse " +
                    "aquí hasta que la vuelvas a conectar."
            )
            .setNegativeButton("No", null)
            .setPositiveButton("Quitar") { _, _ ->
                enSegundoPlano("Quitando «$nombre»…") { Nucleo.olvidar(conf.absolutePath, nombre) }
            }
            .show()
    }

    private fun cuentasEnlazadas(): List<String> = try {
        val lista = JSONArray(Nucleo.cuentas(conf.absolutePath))
        (0 until lista.length()).map { lista.getString(it) }
    } catch (e: Throwable) {
        Log.w(ETIQUETA, "no se pudieron leer las cuentas", e)
        emptyList()
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

    private fun abrirVolumen() {
        val f = tomarFrase() ?: return
        enSegundoPlano("Abriendo. Son unos segundos: hay que preguntarle a cada cuenta…") {
            // Siempre con escritura. Preguntarlo no aportaba nada: desde que el testigo avisa
            // en vez de bloquear, abrir para escribir no le quita nada a nadie, y una casilla
            // que casi todo el mundo marca es una casilla de mas.
            Sesion.abrir(conf.absolutePath, f, true)
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
