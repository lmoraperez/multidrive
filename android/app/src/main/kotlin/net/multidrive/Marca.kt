package net.multidrive

import android.app.Activity
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Las piezas de marca para las pantallas que se construyen en codigo.
 *
 * # Por que hacen falta
 *
 * La pantalla principal se describe en un XML y tiene su cabecera, sus titulos de seccion y sus
 * botones azules. Las demas --crear, abrir, eliminar, configuracion-- se montan en codigo, y ahi
 * cada una se pintaba a su manera: un `TextView` cualquiera de titulo, botones grises del
 * sistema y ningun rastro del logotipo. La de eliminar un volumen parecia de otra aplicacion, y
 * es justo la pantalla donde menos conviene dudar de donde estas.
 *
 * Esto no es una capa de adorno: es el mismo rombo, la misma tipografia y los mismos colores que
 * el XML, en funciones que una pantalla nueva puede llamar sin tener que acordarse de nada.
 */

/**
 * La cabecera: el rombo, el nombre, el distintivo de pre-alfa y el titulo de la pantalla.
 *
 * El distintivo va aqui y no solo en la principal **a proposito**. Alguien puede entrar
 * directamente a eliminar un volumen desde una notificacion o desde atras, y enterarse de que
 * esto es pre-alfa en la pantalla que borra cosas vale mas que en la primera.
 */
fun Activity.cabecera(titulo: String): View = LinearLayout(this).apply {
    orientation = LinearLayout.VERTICAL
    layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
    )

    addView(
        LinearLayout(this@cabecera).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(
                ImageView(this@cabecera).apply {
                    setImageResource(R.drawable.icono)
                    contentDescription = "MultiDrive"
                    layoutParams = LinearLayout.LayoutParams(72, 72)
                }
            )
            addView(
                TextView(this@cabecera).apply {
                    text = "MultiDrive"
                    textSize = 18f
                    letterSpacing = -0.03f
                    setTypeface(typeface, android.graphics.Typeface.BOLD)
                    setTextColor(getColor(R.color.texto))
                    setPadding(24, 0, 0, 0)
                }
            )
            addView(
                TextView(this@cabecera).apply {
                    text = "pre-alfa"
                    textSize = 10f
                    setTextColor(getColor(R.color.aviso))
                    setPadding(20, 0, 0, 0)
                }
            )
        }
    )

    addView(tituloDeSeccion(titulo))
}

/**
 * Un titulo de seccion: pequeno, en mayusculas, muy espaciado y del color de acento.
 *
 * Es la misma forma que el `@style/Seccion` del XML, el sitio web y la ventana del escritorio.
 * Que las tres cosas se reconozcan como el mismo programa es lo que hace esto.
 */
fun Activity.tituloDeSeccion(texto: String): TextView = TextView(this).apply {
    text = texto.uppercase()
    textSize = 12f
    letterSpacing = 0.13f
    setTypeface(typeface, android.graphics.Typeface.BOLD)
    setTextColor(getColor(R.color.acento))
    setPadding(0, 44, 0, 16)
}

/**
 * El boton de **la** accion de la pantalla, en azul.
 *
 * El tema da el suave por defecto, que es lo correcto: si todos los botones gritan, ninguno lo
 * hace. Este se pide a mano, y solo deberia haber uno por pantalla.
 *
 * Se construye envolviendo el tema porque en Android un estilo se aplica **al crear** la vista;
 * ponerselo despues no hace nada, que es la clase de fallo que se ve solo mirando la pantalla.
 */
fun Activity.botonPrincipal(texto: String): Button =
    Button(android.view.ContextThemeWrapper(this, R.style.Boton), null, 0, R.style.Boton).apply {
        text = texto
        layoutParams = LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.WRAP_CONTENT,
        ).apply { topMargin = 28 }
    }

/** Un botón normal, con el mismo ancho y separación que el principal. */
fun Activity.boton(texto: String): Button = Button(this).apply {
    text = texto
    layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
    ).apply { topMargin = 16 }
}

/** Texto secundario: el que explica, no el que titula. */
fun Activity.nota(texto: String): TextView = TextView(this).apply {
    text = texto
    textSize = 13f
    setTextColor(getColor(R.color.apagado))
    setPadding(0, 8, 0, 8)
}

/** El relleno que llevan las pantallas hechas en código, para que todas respiren igual. */
fun LinearLayout.comoPantalla(): LinearLayout = apply {
    orientation = LinearLayout.VERTICAL
    setPadding(56, 72, 56, 56)
}

/**
 * La barra de espacio de una cuenta, con lo usado y lo libre debajo.
 *
 * Tres estados, y los tres dicen cosas distintas a propósito:
 *
 * - **Sin respuesta todavía**: no hay barra, solo «Preguntando…». Una barra al 0 % es una
 *   afirmación, y mientras no ha contestado nadie no se sabe nada que afirmar.
 * - **Sin cupo que declarar**, o un fallo al preguntar: tampoco hay barra. Dibujar una al 0 %
 *   para un bucket S3 sería inventarse un total que ese proveedor no tiene.
 * - **Con cifras**: la barra, y debajo lo usado y lo libre.
 *
 * Quien la use tiene que decir en algún sitio que **lo ocupado es de la cuenta entera**, no
 * solo de MultiDrive. Esta función no lo repite por fila: saldría tres veces en tres líneas.
 */
fun Activity.barraDeEspacio(q: Cuota?, pidiendo: Boolean): View {
    val caja = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(0, 8, 0, 0)
    }

    val pie = { texto: String ->
        TextView(this).apply {
            text = texto
            textSize = 12.5f
            setTextColor(getColor(R.color.apagado))
            setPadding(0, 6, 0, 0)
        }
    }

    if (q == null) {
        if (pidiendo) caja.addView(pie("Preguntando…"))
        return caja
    }
    if (q.fallo != null || q.total == null || q.usado == null) {
        caja.addView(pie(q.fallo ?: "No dice cuánto espacio tiene."))
        return caja
    }

    val total = q.total
    val usado = q.usado.coerceAtMost(total)
    val fraccion = if (total == 0L) 0f else usado.toFloat() / total.toFloat()

    caja.addView(
        LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                (resources.displayMetrics.density * 6).toInt(),
            )
            addView(View(this@barraDeEspacio).apply {
                setBackgroundColor(getColor(R.color.acento))
                layoutParams =
                    LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, fraccion)
            })
            addView(View(this@barraDeEspacio).apply {
                setBackgroundColor(getColor(R.color.linea))
                layoutParams = LinearLayout.LayoutParams(
                    0, ViewGroup.LayoutParams.MATCH_PARENT, 1f - fraccion
                )
            })
        }
    )
    caja.addView(
        pie("${legible(usado)} usados · ${legible(total - usado)} libres de ${legible(total)}")
    )
    return caja
}
