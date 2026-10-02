package net.multidrive

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import android.util.Log
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

private const val ETIQUETA = "multidrive"

/**
 * La frase de paso del volumen, guardada para poder subir sin nadie delante.
 *
 * # Por qué existe esto, que va contra la costumbre de la casa
 *
 * Hasta aquí, MultiDrive no guardaba la frase en ninguna parte y presumía de ello. Sigue siendo
 * lo correcto por defecto, y lo es: esta clase **no se usa** salvo que alguien active la carga
 * automática a sabiendas.
 *
 * Pero subir sin el usuario delante exige poder abrir el volumen sin el usuario delante, y eso
 * exige la frase. No hay una tercera opción: o se guarda, o la carga automática es una promesa
 * que no se cumple. Entre prometer y no cumplir, y cumplir diciendo el precio, se elige lo
 * segundo.
 *
 * # Cómo se guarda
 *
 * Cifrada con una clave AES que **vive dentro del Keystore de Android** y que no sale de ahí:
 * esta aplicación puede pedirle que cifre y descifre, pero no puede leerla ni copiarla a otro
 * teléfono. Lo que queda en disco es el resultado cifrado, que sin esa clave no es nada.
 *
 * `setUnlockedDeviceRequired` ata el descifrado a la pantalla desbloqueada. Con el teléfono
 * bloqueado, ni la propia aplicación puede abrirlo.
 *
 * # Lo que no hace
 *
 * No protege de quien tenga el teléfono **desbloqueado**. Eso se dice en la pantalla antes de
 * activarlo, porque es exactamente el trato.
 */
object Llave {

    private const val ALIAS = "multidrive-volumen"
    private const val FICHERO = "llave"
    private const val CLAVE = "sellada"
    /** 12 bytes es el tamaño que GCM espera; con otro, el descifrado falla sin decir por qué. */
    private const val NONCE = 12

    private fun prefs(c: Context) = c.getSharedPreferences(FICHERO, Context.MODE_PRIVATE)

    /** Si hay una frase guardada. No la abre: solo mira si está. */
    fun hay(c: Context): Boolean = prefs(c).contains(CLAVE)

    /**
     * Guarda la frase de la sesión abierta. `false` si no se pudo.
     *
     * Devuelve `false` en vez de reventar porque hay teléfonos sin almacén protegido por
     * hardware, y ahí lo correcto es no activar la función y decirlo, no fingir que se activó.
     */
    /// La frase se pasa, no se saca de la sesión abierta: la sesión **no la guarda**, y
    /// guardarla ahí para esto habría dejado una copia en memoria durante horas a cambio de
    /// ahorrar teclearla una vez.
    fun guardar(c: Context, frase: String): Boolean = try {
        val cifrador = Cipher.getInstance("AES/GCM/NoPadding")
        cifrador.init(Cipher.ENCRYPT_MODE, clave())
        val sellado = cifrador.doFinal(frase.toByteArray(Charsets.UTF_8))
        // El nonce delante del texto cifrado: hace falta para descifrar y no es secreto. Juntos
        // en una sola cadena, para que no puedan separarse ni quedar descolocados.
        val todo = cifrador.iv + sellado
        prefs(c).edit().putString(CLAVE, Base64.encodeToString(todo, Base64.NO_WRAP)).apply()
        true
    } catch (e: Throwable) {
        Log.w(ETIQUETA, "no se pudo guardar la llave", e)
        false
    }

    /** La frase guardada, o `null` si no hay, no se puede abrir, o la pantalla está bloqueada. */
    fun abrir(c: Context): String? = try {
        val todo = Base64.decode(prefs(c).getString(CLAVE, null), Base64.NO_WRAP)
        val cifrador = Cipher.getInstance("AES/GCM/NoPadding")
        cifrador.init(
            Cipher.DECRYPT_MODE,
            clave(),
            GCMParameterSpec(128, todo, 0, NONCE),
        )
        String(cifrador.doFinal(todo, NONCE, todo.size - NONCE), Charsets.UTF_8)
    } catch (e: Throwable) {
        // Con la pantalla bloqueada esto falla, y es lo correcto: no es un error que haya que
        // enseñar, es la protección haciendo su trabajo. El trabajo se reintentará más tarde.
        Log.i(ETIQUETA, "no se pudo abrir la llave: ${e.javaClass.simpleName}")
        null
    }

    /**
     * Borra lo guardado **y la clave del Keystore**.
     *
     * Las dos cosas: dejar la clave del Keystore sin nada que descifrar no hace daño, pero lo
     * que no hace falta no se guarda, y una clave que sigue ahí invita a pensar que algo sigue
     * guardado.
     */
    fun borrar(c: Context) {
        prefs(c).edit().remove(CLAVE).apply()
        try {
            KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.deleteEntry(ALIAS)
        } catch (e: Throwable) {
            Log.w(ETIQUETA, "no se pudo borrar la clave del keystore", e)
        }
    }

    /** La clave del Keystore, creándola la primera vez. Nunca sale de ahí. */
    private fun clave(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }

        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        val spec = KeyGenParameterSpec.Builder(
            ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .apply {
                // Desde Android 9. Donde no existe, la llave se guarda igual pero sin esta
                // atadura; se prefiere eso a no ofrecer la función en teléfonos más viejos.
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) setUnlockedDeviceRequired(true)
            }
            .build()
        gen.init(spec)
        return gen.generateKey()
    }
}
