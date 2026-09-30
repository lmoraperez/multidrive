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
import java.io.File
import java.io.RandomAccessFile

private const val ETIQUETA = "multidrive"

/**
 * Recibe lo que otra aplicacion escribe y lo sube **al cerrar**.
 *
 * # Por que un temporal y no subir al vuelo
 *
 * El sistema entrega las escrituras por offsets y en el orden que quiera: una aplicacion puede
 * escribir el final antes que el principio, o volver atras a corregir una cabecera. Un volumen
 * de MultiDrive no admite eso: el fichero se trocea en 16 MiB, cada trozo se cifra, se parte en
 * fragmentos y se reparte entre cuentas. **Hay que verlo entero para colocarlo.**
 *
 * Asi que se junta en un fichero temporal del propio telefono y se sube de una vez en
 * `onRelease`. Lo que eso cuesta, y hay que decirlo: **hace falta sitio libre en el movil para
 * una copia completa** de lo que se esta subiendo, y hasta que se cierra el descriptor no hay
 * nada en la nube.
 *
 * # Lo que pasa si algo falla al subir
 *
 * `onRelease` **no puede devolver un error**: la firma no lo permite y el sistema ya le dijo a
 * la aplicacion que la escritura fue bien. Lo unico honesto que queda es dejarlo en el registro
 * y **no borrar el temporal**, para que lo que se escribio no desaparezca sin dejar rastro.
 */
object Escritor {

    /**
     * El hilo por el que llegan las escrituras.
     *
     * Propio, y distinto del de las lecturas: una subida de varios minutos por el hilo de las
     * lecturas dejaria sin responder a todo el que estuviera leyendo.
     */
    private val hilo = HandlerThread("multidrive-escrituras").apply { start() }
    private val reparto = Handler(hilo.looper)

    fun abrir(
        gestor: StorageManager,
        contexto: android.content.Context,
        sesion: Long,
        ruta: String,
    ): ParcelFileDescriptor {
        val temporal = File.createTempFile("subiendo-", ".tmp", contexto.cacheDir)
        return gestor.openProxyFileDescriptor(
            ParcelFileDescriptor.MODE_READ_WRITE,
            Callback(sesion, ruta, temporal, contexto.applicationContext),
            reparto,
        )
    }

    private class Callback(
        private val sesion: Long,
        private val ruta: String,
        private val temporal: File,
        /** El contexto de la **aplicacion**: guardar el de una actividad la filtraria entera. */
        private val contexto: android.content.Context,
    ) : ProxyFileDescriptorCallback() {

        private val fichero = RandomAccessFile(temporal, "rw")

        override fun onGetSize(): Long = fichero.length()

        override fun onRead(offset: Long, size: Int, data: ByteArray): Int {
            // Leer de lo que se esta escribiendo: hay aplicaciones que releen lo que acaban de
            // poner. Se sirve del temporal, que es donde esta.
            return try {
                fichero.seek(offset)
                val leidos = fichero.read(data, 0, size)
                if (leidos < 0) 0 else leidos
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo releer $ruta", e)
                throw ErrnoException("onRead", OsConstants.EIO)
            }
        }

        override fun onWrite(offset: Long, size: Int, data: ByteArray): Int {
            return try {
                fichero.seek(offset)
                fichero.write(data, 0, size)
                size
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo guardar lo que se escribia en $ruta", e)
                throw ErrnoException("onWrite", OsConstants.EIO)
            }
        }

        override fun onFsync() {
            fichero.fd.sync()
        }

        override fun onRelease() {
            try {
                fichero.close()
            } catch (e: Throwable) {
                Log.w(ETIQUETA, "no se pudo cerrar el temporal de $ruta", e)
            }
            try {
                Nucleo.escribir(sesion, ruta, temporal.absolutePath)
                temporal.delete()
                Cache.olvidar(contexto, ruta)
                Log.i(ETIQUETA, "subido $ruta")
            } catch (e: Throwable) {
                // Aqui ya no se puede avisar a nadie: el sistema dio la escritura por buena. Lo
                // unico honesto es no borrar lo que hay y dejarlo dicho.
                Log.e(
                    ETIQUETA,
                    "NO se pudo subir $ruta. Lo escrito se queda en ${temporal.absolutePath}",
                    e,
                )
            }
        }
    }
}
