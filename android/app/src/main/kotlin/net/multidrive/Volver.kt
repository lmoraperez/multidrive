package net.multidrive

import android.app.Activity
import android.os.Bundle

/**
 * La puerta por la que el navegador devuelve a la aplicación al terminar de enlazar una cuenta.
 *
 * # Qué hace, que es casi nada
 *
 * Nacer y cerrarse. No pinta nada ni decide nada: lo único que importa es **el efecto de
 * lanzarla**, que es traer la tarea de MultiDrive al frente. Al cerrarse enseguida, debajo
 * aparece la pantalla que estaba esperando --`Crear`, `Ampliar`, la que fuera--, con su hilo
 * todavía parado en el navegador y a punto de recibir el código.
 *
 * # Por qué hace falta
 *
 * Porque la aplicación **no puede ponerse delante sola**. Desde Android 10 una aplicación en
 * segundo plano no arranca actividades, y mientras das permiso la que está delante es el
 * navegador. El salto lo tiene que dar quien está delante, y por eso la página que sirve el
 * bucle local lleva un enlace a aquí.
 *
 * # Por qué no se llega aquí por accidente
 *
 * El esquema es nuestro y la actividad no hace nada observable: quien la abra a mano desde
 * otra aplicación consigue exactamente lo mismo que abrir MultiDrive desde el lanzador. No
 * recibe el código ni lo toca --eso viaja por el puerto local, que es otro camino-- así que no
 * hay nada que pueda empujarse desde fuera.
 */
class Volver : Activity() {
    override fun onCreate(guardado: Bundle?) {
        super.onCreate(guardado)
        finish()
        // Sin animación: con ella se ve un parpadeo de una pantalla vacía que no es ninguna
        // pantalla, justo cuando el usuario espera volver a donde estaba.
        overridePendingTransition(0, 0)
    }
}
