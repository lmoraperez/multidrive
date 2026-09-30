package net.multidrive

import android.database.Cursor
import android.database.MatrixCursor
import android.os.Bundle
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.os.storage.StorageManager
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import android.util.Log
import net.multidrive.nucleo.Nucleo
import org.json.JSONArray
import org.json.JSONObject

/**
 * MultiDrive dentro de la app de Archivos y del selector del sistema.
 *
 * No es un montaje y no puede serlo: Android no deja montar sin root. Lo que hay es el Storage
 * Access Framework, donde el volumen aparece como un origen mas y las demas aplicaciones leen
 * por `ContentResolver`. Consecuencia que conviene tener presente: **una aplicacion que use
 * rutas crudas no ve nada**.
 *
 * # Los identificadores son las rutas
 *
 * Para Android un `documentId` es una cadena opaca. Aqui es la ruta dentro del volumen, que ya
 * es unica y estable, en vez de inventar identificadores y mantener una tabla que traduzca.
 *
 * # Solo lectura, y sin disimulo
 *
 * v1 es de un solo escritor, y un movil escribiendo en un volumen que el escritorio tambien
 * escribe *es* el problema de concurrencia que esta fuera de alcance. Asi que no se declaran
 * `FLAG_SUPPORTS_WRITE` ni `FLAG_DIR_SUPPORTS_CREATE`, y el selector del sistema **no ofrece**
 * guardar aqui. Es mejor no aparecer como destino que aparecer y fallar.
 */
class Proveedor : DocumentsProvider() {

    override fun onCreate(): Boolean {
        // El verificador de certificados, antes que cualquier peticion. Sin esto la primera
        // lectura no falla: entra en panico y se lleva el proceso.
        return try {
            Nucleo.iniciar(context!!.applicationContext)
            true
        } catch (e: Throwable) {
            Log.e(ETIQUETA, "no se pudo iniciar el nucleo", e)
            false
        }
    }

    /**
     * Deslizar para actualizar: mira si hay algo nuevo publicado desde otro equipo.
     *
     * Existe porque el movil es un **lector**, y lo que ve se queda congelado en la raiz que
     * leyo al abrir el volumen. Sin esto, un fichero subido desde el escritorio no aparece
     * nunca y la unica salida es cerrar el volumen y volver a abrirlo. Es exactamente lo que le
     * paso a quien lo uso primero.
     *
     * **No se comprueba solo cada tanto**, y es deliberado: cada comprobacion es una vuelta a la
     * red, y sobre datos moviles eso se paga en bateria y en plan de datos. El gesto de
     * actualizar ya existe en el selector, lo hace quien sabe que acaba de subir algo, y no
     * cuesta nada cuando nadie lo pide.
     */
    override fun refresh(uri: android.net.Uri, extras: Bundle?, signal: CancellationSignal?): Boolean {
        val s = Sesion.id ?: return false
        return try {
            val cambio = Nucleo.refrescar(s)
            if (cambio) {
                // Todo lo listado es de la raiz vieja: no vale nada.
                Cache.hijos.clear()
                context!!.contentResolver.notifyChange(uri, null)
            }
            true
        } catch (e: Throwable) {
            Log.w(ETIQUETA, "no se pudo refrescar", e)
            false
        }
    }

    /**
     * El volumen. **Siempre**, haya sesion o no.
     *
     * Antes no se publicaba ninguna raiz mientras no hubiera sesion, con el argumento de que
     * aparecer y fallar es peor que no aparecer. Estaba mal, y se vio en cuanto alguien lo uso:
     * si MultiDrive no sale en la lista, no hay forma de saber si falta abrir el volumen, si la
     * aplicacion no funciona o si uno esta mirando donde no es. «No aparece» y «no funciona» se
     * confunden, y ninguna de las dos se puede arreglar a ciegas.
     *
     * Ahora sale siempre, y **el resumen dice en que estado esta**. Si no hay sesion, entrar
     * enseña un mensaje que explica que hay que abrir el volumen en la aplicacion, que es algo
     * que se puede hacer.
     */
    override fun queryRoots(projection: Array<String>?): Cursor {
        val c = MatrixCursor(projection ?: COLUMNAS_RAIZ)
        val s = Sesion.id

        c.newRow().apply {
            add(Root.COLUMN_ROOT_ID, RAIZ)
            add(Root.COLUMN_DOCUMENT_ID, "/")
            add(Root.COLUMN_TITLE, "MultiDrive")
            add(
                Root.COLUMN_SUMMARY,
                if (s != null) Sesion.resumen else "Cerrado: abre el volumen en MultiDrive",
            )
            // Ni `FLAG_SUPPORTS_CREATE` ni `FLAG_SUPPORTS_RECENTS`: no se crea nada y no hay
            // «recientes» que ofrecer sin un indice local que todavia no existe.
            add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_IS_CHILD)
            // El icono tiene que ser un recurso **nuestro**: la app de Archivos lo carga por su
            // identificador y dentro de nuestro paquete, asi que uno de `android.R` no lo puede
            // resolver, y una raiz cuyo icono no se resuelve puede acabar sin aparecer.
            add(Root.COLUMN_ICON, R.drawable.icono)
        }
        Log.i(ETIQUETA, "raiz publicada, sesion $s")
        return c
    }

    override fun queryDocument(documentId: String, projection: Array<String>?): Cursor {
        val c = MatrixCursor(projection ?: COLUMNAS_DOC)
        val s = Sesion.id ?: return c

        if (documentId == "/") {
            fila(c, "/", "MultiDrive", true, 0)
            return c
        }
        val json = Nucleo.mirar(s, documentId)
        if (json == "null") return c
        val o = JSONObject(json)
        fila(c, documentId, o.getString("nombre"), o.getBoolean("dir"), o.getLong("bytes"))
        return c
    }

    override fun queryChildDocuments(
        parentDocumentId: String,
        projection: Array<String>?,
        sortOrder: String?,
    ): Cursor {
        val c = MatrixCursor(projection ?: COLUMNAS_DOC)
        if (Sesion.id == null) {
            // `EXTRA_ERROR` es la forma que Android tiene de decir algo sin inventarse ficheros:
            // el selector enseña este texto en vez de una carpeta vacia. Una carpeta vacia
            // significaria «aqui no hay nada», que es mentira, y encima no dice que hacer.
            c.extras = Bundle().apply {
                putString(
                    android.provider.DocumentsContract.EXTRA_ERROR,
                    "El volumen esta cerrado. Abrelo en la aplicacion MultiDrive: hace falta la " +
                        "frase de paso, y no se guarda en ningun sitio.",
                )
            }
            return c
        }

        // Esto tarda **segundos** contra cuatro cuentas: 1229 ms medidos con la raiz vacia, y
        // un directorio con contenido paga mas. Android llama desde cualquier hilo y espera
        // respuesta pronto, asi que se contesta al momento con la lista vacia y un aviso de
        // que se esta cargando; cuando llegue, se notifica y el selector vuelve a preguntar.
        //
        // Sin esto, la app de Archivos se queda quieta ese rato y cualquiera lo lee como una
        // aplicacion rota.
        val uri = android.provider.DocumentsContract.buildChildDocumentsUri(AUTORIDAD, parentDocumentId)
        val cacheado = Cache.hijos[parentDocumentId]
        if (cacheado == null) {
            c.extras = Bundle().apply { putBoolean(android.provider.DocumentsContract.EXTRA_LOADING, true) }
            c.setNotificationUri(context!!.contentResolver, uri)
            Cargador.pedir(parentDocumentId) {
                context!!.contentResolver.notifyChange(uri, null)
            }
            return c
        }

        val lista = JSONArray(cacheado)
        for (i in 0 until lista.length()) {
            val o = lista.getJSONObject(i)
            val nombre = o.getString("nombre")
            val hijo = if (parentDocumentId == "/") "/$nombre" else "$parentDocumentId/$nombre"
            fila(c, hijo, nombre, o.getBoolean("dir"), o.getLong("bytes"))
        }
        c.setNotificationUri(context!!.contentResolver, uri)
        return c
    }

    /**
     * Abrir un fichero, **sin bajarlo entero**.
     *
     * Lo hace `openProxyFileDescriptor`: las lecturas llegan a [Lector] y cada una se traduce en
     * un `read_at`, que baja solo los trozos que el rango toca. Es lo que hace posible abrir un
     * video de 2 GiB sin esperar a 2 GiB.
     *
     * Solo lectura. `mode` se comprueba en vez de darse por bueno: el sistema no deberia pedir
     * escritura --no se declaran las banderas-- y si lo hace, es mejor decir que no que abrir
     * algo que luego no escribe.
     */
    override fun openDocument(
        documentId: String,
        mode: String,
        signal: CancellationSignal?,
    ): ParcelFileDescriptor {
        val s = Sesion.id ?: throw IllegalStateException(
            "el volumen esta cerrado: abrelo en la aplicacion MultiDrive"
        )
        val gestor = context!!.getSystemService(StorageManager::class.java)

        // Escribir: se junta en un temporal y se sube al cerrar. No se puede subir al vuelo
        // porque el sistema escribe por offsets y en el orden que quiera, y un volumen de
        // MultiDrive necesita ver el fichero entero para trocearlo y repartirlo.
        if (mode.contains("w")) {
            if (!Sesion.escribe) {
                throw UnsupportedOperationException(
                    "este volumen esta abierto solo para leer. Abrelo con escritura en la " +
                        "aplicacion MultiDrive."
                )
            }
            return Escritor.abrir(gestor, context!!.cacheDir, s, documentId)
        }

        val json = Nucleo.mirar(s, documentId)
        if (json == "null") throw java.io.FileNotFoundException(documentId)
        val o = JSONObject(json)
        if (o.getBoolean("dir")) throw java.io.FileNotFoundException("$documentId es un directorio")

        return Lector.abrir(gestor, s, documentId, o.getLong("bytes"))
    }

    /**
     * Crea un fichero vacio o un directorio. Lo llama el selector al «guardar aqui».
     *
     * El fichero se crea **vacio de verdad**: el sistema llama a esto y despues abre el
     * documento para escribir en el. Crearlo ya con contenido no tendria sentido, y no crearlo
     * dejaria al selector sin nada a lo que escribir.
     */
    override fun createDocument(
        parentDocumentId: String,
        mimeType: String,
        displayName: String,
    ): String {
        val s = Sesion.id ?: throw IllegalStateException("el volumen esta cerrado")
        if (!Sesion.escribe) {
            throw UnsupportedOperationException("este volumen esta abierto solo para leer")
        }
        val hijo =
            if (parentDocumentId == "/") "/$displayName" else "$parentDocumentId/$displayName"

        if (mimeType == Document.MIME_TYPE_DIR) {
            Nucleo.crearDirectorio(s, hijo)
        } else {
            // Un temporal vacio, que es lo que significa «crear»: el contenido llega despues
            // por `openDocument`.
            val vacio = java.io.File.createTempFile("vacio-", ".tmp", context!!.cacheDir)
            try {
                Nucleo.escribir(s, hijo, vacio.absolutePath)
            } finally {
                vacio.delete()
            }
        }
        olvidar(parentDocumentId)
        return hijo
    }

    override fun deleteDocument(documentId: String) {
        val s = Sesion.id ?: throw IllegalStateException("el volumen esta cerrado")
        if (!Sesion.escribe) {
            throw UnsupportedOperationException("este volumen esta abierto solo para leer")
        }
        Nucleo.borrar(s, documentId)
        olvidar(documentId.substringBeforeLast('/').ifEmpty { "/" })
    }

    /**
     * Tira lo listado de un directorio y avisa al selector.
     *
     * Sin esto, lo que se acaba de crear o borrar no aparece --ni desaparece-- hasta refrescar
     * a mano: el listado que hay en la cache es el de antes del cambio.
     */
    private fun olvidar(ruta: String) {
        Cache.hijos.remove(ruta)
        val uri = android.provider.DocumentsContract.buildChildDocumentsUri(AUTORIDAD, ruta)
        context!!.contentResolver.notifyChange(uri, null)
    }

    private fun fila(c: MatrixCursor, id: String, nombre: String, dir: Boolean, bytes: Long) {
        c.newRow().apply {
            add(Document.COLUMN_DOCUMENT_ID, id)
            add(Document.COLUMN_DISPLAY_NAME, nombre)
            add(Document.COLUMN_MIME_TYPE, if (dir) Document.MIME_TYPE_DIR else "application/octet-stream")
            add(Document.COLUMN_SIZE, bytes)
            // Las banderas dependen del permiso que haya **ahora**: con el volumen abierto solo
            // para leer, el selector no ofrece guardar aqui ni borrar. Es mejor no aparecer como
            // destino que aparecer y fallar.
            var banderas = 0
            if (Sesion.escribe) {
                banderas = banderas or Document.FLAG_SUPPORTS_DELETE
                if (dir) banderas = banderas or Document.FLAG_DIR_SUPPORTS_CREATE
                else banderas = banderas or Document.FLAG_SUPPORTS_WRITE
            }
            add(Document.COLUMN_FLAGS, banderas)
            // Sin fecha: el arbol no la guarda todavia, y poner la de ahora seria inventarla.
        }
    }

    private companion object {
        const val ETIQUETA = "multidrive"
        const val AUTORIDAD = "net.multidrive.documentos"
        const val RAIZ = "multidrive"

        val COLUMNAS_RAIZ = arrayOf(
            Root.COLUMN_ROOT_ID,
            Root.COLUMN_DOCUMENT_ID,
            Root.COLUMN_TITLE,
            Root.COLUMN_SUMMARY,
            Root.COLUMN_FLAGS,
            Root.COLUMN_ICON,
        )
        val COLUMNAS_DOC = arrayOf(
            Document.COLUMN_DOCUMENT_ID,
            Document.COLUMN_DISPLAY_NAME,
            Document.COLUMN_MIME_TYPE,
            Document.COLUMN_SIZE,
            Document.COLUMN_FLAGS,
        )
    }
}
