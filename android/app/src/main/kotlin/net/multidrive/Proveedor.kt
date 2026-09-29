package net.multidrive

import android.database.Cursor
import android.database.MatrixCursor
import android.os.Bundle
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
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
     * El volumen, si hay sesion abierta.
     *
     * Cuando no la hay **no se devuelve ninguna raiz**, y entonces MultiDrive no aparece en la
     * app de Archivos. Es deliberado: aparecer y fallar al abrir es peor que no aparecer, y
     * aqui no hay forma de pedir la frase de paso --un `DocumentsProvider` no tiene pantalla--.
     * La pide la aplicacion, y al abrir sesion se notifica para que la raiz aparezca.
     */
    override fun queryRoots(projection: Array<String>?): Cursor {
        val c = MatrixCursor(projection ?: COLUMNAS_RAIZ)
        val s = Sesion.id ?: return c

        c.newRow().apply {
            add(Root.COLUMN_ROOT_ID, RAIZ)
            add(Root.COLUMN_DOCUMENT_ID, "/")
            add(Root.COLUMN_TITLE, "MultiDrive")
            add(Root.COLUMN_SUMMARY, Sesion.resumen)
            // Ni `FLAG_SUPPORTS_CREATE` ni `FLAG_SUPPORTS_RECENTS`: no se crea nada y no hay
            // «recientes» que ofrecer sin un indice local que todavia no existe.
            add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_IS_CHILD)
            add(Root.COLUMN_ICON, android.R.drawable.ic_menu_save)
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
        val s = Sesion.id ?: return c

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
     * Abrir un fichero. **Todavia no.**
     *
     * Aqui va `StorageManager.openProxyFileDescriptor`, que es lo que permite leer por rangos
     * --bajando solo los trozos que el rango toca-- en vez de descargar el fichero entero antes
     * de abrirlo. Es el paso siguiente, y hasta entonces se dice que no se puede en vez de
     * ofrecer algo que no funciona.
     */
    override fun openDocument(
        documentId: String,
        mode: String,
        signal: CancellationSignal?,
    ): ParcelFileDescriptor {
        throw UnsupportedOperationException(
            "leer ficheros todavia no esta: falta openProxyFileDescriptor"
        )
    }

    private fun fila(c: MatrixCursor, id: String, nombre: String, dir: Boolean, bytes: Long) {
        c.newRow().apply {
            add(Document.COLUMN_DOCUMENT_ID, id)
            add(Document.COLUMN_DISPLAY_NAME, nombre)
            add(Document.COLUMN_MIME_TYPE, if (dir) Document.MIME_TYPE_DIR else "application/octet-stream")
            add(Document.COLUMN_SIZE, bytes)
            // Sin banderas: ni escritura, ni borrado, ni renombrado. Ver la nota de la clase.
            add(Document.COLUMN_FLAGS, 0)
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
