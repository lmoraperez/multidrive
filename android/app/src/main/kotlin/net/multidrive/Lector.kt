package net.multidrive

import android.os.Handler
import android.os.HandlerThread
import android.os.ParcelFileDescriptor
import android.os.ProxyFileDescriptorCallback
import android.os.storage.StorageManager
import android.system.ErrnoException
import android.system.OsConstants
import android.util.Log
import net.multidrive.nucleo.Nucleo

private const val ETIQUETA = "multidrive"

/**
 * Entrega ficheros del volumen **sin bajarlos enteros**.
 *
 * Es lo que separa esto de un descargador con mas pasos. La alternativa --lo que hace la
 * aplicacion de Nextcloud-- es traerse el fichero completo a un temporal y devolver un
 * descriptor sobre el; con troceado de 16 MiB y cuatro cuentas detras, abrir un video de 2 GiB
 * significaria bajarse 2 GiB antes de ver el primer fotograma.
 *
 * `StorageManager.openProxyFileDescriptor` devuelve un descriptor cuyas lecturas llegan aqui
 * como llamadas, y cada una se traduce en un `read_at` que baja **solo los trozos que el rango
 * toca**. Existe desde API 26, y por eso el `minSdk` de la aplicacion es 26: sin esto, no hay
 * aplicacion que merezca la pena.
 */
object Lector {

    /**
     * El hilo por el que llegan las lecturas.
     *
     * `openProxyFileDescriptor` las entrega por el `Handler` que se le pase, asi que hace falta
     * uno propio: usar el de la interfaz seria bloquearla en cada lectura, y las lecturas aqui
     * van a la red.
     *
     * Uno solo para todo el proceso. Eso **serializa** las lecturas de todos los ficheros
     * abiertos a la vez, que es una limitacion de verdad y conviene saberla; a cambio, las de
     * un mismo fichero llegan en orden y el lado de Rust no necesita candados. Cuando haya un
     * caso que lo pida --dos aplicaciones leyendo a la vez-- se pasa a uno por descriptor.
     */
    private val hilo = HandlerThread("multidrive-lecturas").apply { start() }
    private val reparto = Handler(hilo.looper)

    fun abrir(gestor: StorageManager, sesion: Long, ruta: String, bytes: Long): ParcelFileDescriptor =
        gestor.openProxyFileDescriptor(
            ParcelFileDescriptor.MODE_READ_ONLY,
            Callback(sesion, ruta, bytes),
            reparto,
        )

    private class Callback(
        private val sesion: Long,
        private val ruta: String,
        private val bytes: Long,
    ) : ProxyFileDescriptorCallback() {

        override fun onGetSize(): Long = bytes

        override fun onRead(offset: Long, size: Int, data: ByteArray): Int {
            // Mas alla del final no es un error: es cero bytes, que es lo que `read(2)` hace.
            if (offset >= bytes) return 0
            return try {
                val trozo = Nucleo.leer(sesion, ruta, offset, size)
                System.arraycopy(trozo, 0, data, 0, trozo.size)
                trozo.size
            } catch (e: Throwable) {
                // Los errores se comunican con `ErrnoException`, no devolviendo codigos. Se
                // anota el motivo de verdad: `EIO` a secas no dice si fue la red, la frase de
                // paso o un fragmento que falta, y eso solo se sabe mirando el registro.
                Log.w(ETIQUETA, "no se pudo leer $ruta desde $offset", e)
                throw ErrnoException("onRead", OsConstants.EIO)
            }
        }

        override fun onRelease() {
            // Nada que soltar: el volumen es de la sesion y sigue abierto para el siguiente.
        }
    }
}
