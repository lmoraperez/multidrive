//! Cache de trozos en disco, que sobrevive al reinicio.
//!
//! La cache en memoria solo sirve mientras el proceso vive. Esta es el segundo escalon:
//! memoria, disco, proveedores. Cubre el arranque en frio del montaje y las ordenes sueltas
//! de la linea de ordenes, que son procesos distintos y hoy pagan el precio entero cada vez.
//!
//! # Guarda el blob **sellado**, no el contenido
//!
//! Es la decision que importa. Cachear el texto en claro seria mas rapido —se ahorraria un
//! descifrado por acierto— pero dejaria los datos del usuario **sin cifrar en el disco
//! local**, que es justo lo que este proyecto promete que no pasa. Descifrar 16 MiB cuesta
//! milisegundos; traerlos de la nube, segundos. No hay discusion.
//!
//! Los ficheros se nombran por el identificador del trozo. Eso no filtra nada nuevo: los
//! proveedores ya ven ese mismo identificador en las claves de los fragmentos.
//!
//! # Tampoco necesita invalidacion
//!
//! Por lo mismo que la de memoria: los trozos son inmutables y su nombre es su contenido. Un
//! fichero de cache no puede quedar obsoleto. Lo que si puede es **corromperse**, y por eso
//! se verifica el hash al leer: una cache silenciosamente corrupta seria peor que no tenerla.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::{ChunkId, CoreError};

/// Cuanto ha servido la cache de disco.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub aciertos: u64,
    pub fallos: u64,
    pub desalojos: u64,
    /// Aciertos descartados porque el fichero no cuadraba con su hash.
    pub corruptos: u64,
    pub bytes: usize,
    pub entradas: usize,
}

struct Interior {
    /// Lo que hay en disco: identificador contra tamano y ultimo uso.
    ///
    /// Se mantiene en memoria para no tener que preguntarle al sistema de ficheros en cada
    /// operacion, y se reconstruye al abrir recorriendo el directorio.
    entradas: HashMap<ChunkId, (u64, u64)>,
    bytes: usize,
    reloj: u64,
    aciertos: u64,
    fallos: u64,
    desalojos: u64,
    corruptos: u64,
}

/// Cache de trozos sellados en un directorio local.
pub struct DiskCache {
    raiz: PathBuf,
    capacidad: usize,
    interior: Mutex<Interior>,
}

impl DiskCache {
    /// Abre o crea la cache en `raiz`, con `capacidad` bytes.
    ///
    /// Recorre lo que ya hubiera para reconstruir el indice: eso es lo que hace que la cache
    /// siga sirviendo despues de un reinicio.
    pub async fn open(raiz: impl AsRef<Path>, capacidad: usize) -> Result<Self, CoreError> {
        let raiz = raiz.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&raiz).await?;

        let mut entradas = HashMap::new();
        let mut bytes = 0usize;

        // Dos niveles, como en los proveedores: <raiz>/<2 hex>/<id>.
        let mut nivel1 = tokio::fs::read_dir(&raiz).await?;
        while let Some(dir) = nivel1.next_entry().await? {
            if !dir.file_type().await?.is_dir() {
                continue;
            }
            let mut nivel2 = tokio::fs::read_dir(dir.path()).await?;
            while let Some(fichero) = nivel2.next_entry().await? {
                let nombre = fichero.file_name();
                let Some(id) = nombre.to_str().and_then(ChunkId::parse) else {
                    continue;
                };
                let tam = fichero.metadata().await?.len();
                entradas.insert(id, (tam, 0));
                bytes += tam as usize;
            }
        }

        let cache = Self {
            raiz,
            capacidad,
            interior: Mutex::new(Interior {
                entradas,
                bytes,
                reloj: 0,
                aciertos: 0,
                fallos: 0,
                desalojos: 0,
                corruptos: 0,
            }),
        };

        // Lo que hubiera de antes puede pasarse de la capacidad de ahora.
        cache.desalojar_hasta_caber().await;
        Ok(cache)
    }

    fn ruta(&self, id: &ChunkId) -> PathBuf {
        self.raiz.join(&id.as_str()[..2]).join(id.as_str())
    }

    /// Devuelve lo que `abrir` saque del blob sellado, si esta y cuadra.
    ///
    /// `abrir` devuelve el **valor ya abierto** en vez de un simple si/no a proposito:
    /// verificar exige descifrar, y devolver un booleano obligaria a descifrar otra vez para
    /// aprovecharlo. Con trozos de 16 MiB eso es el doble de trabajo en cada acierto.
    pub async fn get<T>(&self, id: &ChunkId, abrir: impl FnOnce(&[u8]) -> Option<T>) -> Option<T> {
        let presente = {
            let mut i = self.lock();
            i.reloj += 1;
            let ahora = i.reloj;
            match i.entradas.get_mut(id) {
                Some((_, ultimo_uso)) => {
                    *ultimo_uso = ahora;
                    true
                }
                None => false,
            }
        };

        if !presente {
            self.lock().fallos += 1;
            return None;
        }

        // Que el indice diga que esta no garantiza que se pueda leer: alguien puede haber
        // borrado el directorio por debajo.
        let Ok(datos) = tokio::fs::read(self.ruta(id)).await else {
            self.olvidar(id).await;
            self.lock().fallos += 1;
            return None;
        };

        let Some(valor) = abrir(&datos) else {
            // Un fichero que no cuadra con su hash es basura, no un acierto. Se tira.
            self.olvidar(id).await;
            let mut i = self.lock();
            i.corruptos += 1;
            i.fallos += 1;
            return None;
        };

        self.lock().aciertos += 1;
        Some(valor)
    }

    /// Guarda un blob sellado. Los fallos se ignoran: una cache que no puede escribir sigue
    /// siendo una cache, solo que inutil.
    pub async fn put(&self, id: &ChunkId, sellado: &[u8]) {
        if self.capacidad == 0 || sellado.len() > self.capacidad {
            return;
        }
        if self.lock().entradas.contains_key(id) {
            return;
        }

        let destino = self.ruta(id);
        let Some(padre) = destino.parent() else {
            return;
        };
        if tokio::fs::create_dir_all(padre).await.is_err() {
            return;
        }

        // Escribir y renombrar, para que nadie llegue a ver un fichero a medias. Un corte de
        // corriente en mitad de esto deja un `.tmp` suelto, no una entrada corrupta.
        let temporal = destino.with_extension("tmp");
        if tokio::fs::write(&temporal, sellado).await.is_err() {
            return;
        }
        if tokio::fs::rename(&temporal, &destino).await.is_err() {
            let _ = tokio::fs::remove_file(&temporal).await;
            return;
        }

        {
            let mut i = self.lock();
            i.reloj += 1;
            let ahora = i.reloj;
            if i.entradas
                .insert(id.clone(), (sellado.len() as u64, ahora))
                .is_none()
            {
                i.bytes += sellado.len();
            }
        }

        self.desalojar_hasta_caber().await;
    }

    async fn olvidar(&self, id: &ChunkId) {
        // Ojo con el mutex: es de `std` y no es reentrante, asi que todo lo que toque el
        // interior va en un solo bloqueo. Cogerlo dos veces en la misma expresion se cuelga.
        let tam = {
            let mut i = self.lock();
            match i.entradas.remove(id) {
                Some((tam, _)) => {
                    i.bytes = i.bytes.saturating_sub(tam as usize);
                    Some(tam)
                }
                None => None,
            }
        };
        if tam.is_some() {
            let _ = tokio::fs::remove_file(self.ruta(id)).await;
        }
    }

    async fn desalojar_hasta_caber(&self) {
        loop {
            let victima = {
                let i = self.lock();
                if i.bytes <= self.capacidad {
                    return;
                }
                i.entradas
                    .iter()
                    .min_by_key(|(_, (_, ultimo_uso))| *ultimo_uso)
                    .map(|(id, _)| id.clone())
            };

            match victima {
                Some(id) => {
                    self.olvidar(&id).await;
                    self.lock().desalojos += 1;
                }
                None => return,
            }
        }
    }

    pub fn stats(&self) -> Stats {
        let i = self.lock();
        Stats {
            aciertos: i.aciertos,
            fallos: i.fallos,
            desalojos: i.desalojos,
            corruptos: i.corruptos,
            bytes: i.bytes,
            entradas: i.entradas.len(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Interior> {
        self.interior.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for DiskCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.stats();
        f.debug_struct("DiskCache")
            .field("raiz", &self.raiz)
            .field("capacidad", &self.capacidad)
            .field("entradas", &s.entradas)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(nombre: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "libredisk-dc-{}-{nombre}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn id(n: u8) -> ChunkId {
        ChunkId::of(&[n])
    }

    /// Devuelve el blob tal cual: para los tests que no van del verificado.
    fn tal_cual(b: &[u8]) -> Option<Vec<u8>> {
        Some(b.to_vec())
    }

    #[tokio::test]
    async fn guarda_y_devuelve() {
        let d = dir("basico");
        let c = DiskCache::open(&d, 1_000_000).await.unwrap();

        assert!(c.get(&id(1), tal_cual).await.is_none());
        c.put(&id(1), b"contenido sellado").await;
        assert_eq!(c.get(&id(1), tal_cual).await.unwrap(), b"contenido sellado");

        let s = c.stats();
        assert_eq!((s.aciertos, s.fallos), (1, 1));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// La razon de existir de esta cache: seguir sirviendo despues de reiniciar.
    #[tokio::test]
    async fn sobrevive_a_cerrar_y_volver_a_abrir() {
        let d = dir("persiste");
        {
            let c = DiskCache::open(&d, 1_000_000).await.unwrap();
            c.put(&id(1), b"datos que persisten").await;
            c.put(&id(2), b"otros mas").await;
        }

        let c = DiskCache::open(&d, 1_000_000).await.unwrap();
        assert_eq!(c.stats().entradas, 2, "el indice se reconstruye del disco");
        assert_eq!(
            c.get(&id(1), tal_cual).await.unwrap(),
            b"datos que persisten"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Una entrada corrupta no puede pasar por buena: seria peor que no tener cache.
    #[tokio::test]
    async fn una_entrada_corrupta_se_descarta_y_se_borra() {
        let d = dir("corrupto");
        let c = DiskCache::open(&d, 1_000_000).await.unwrap();
        c.put(&id(1), b"bueno").await;

        // El verificador dice que no cuadra, como haria un hash que no coincide.
        assert!(c.get(&id(1), |_| None::<Vec<u8>>).await.is_none());

        let s = c.stats();
        assert_eq!(s.corruptos, 1);
        assert_eq!(s.entradas, 0, "y se quita del indice");
        assert!(!c.ruta(&id(1)).exists(), "y del disco");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn desaloja_el_menos_usado_y_no_se_pasa_de_capacidad() {
        let d = dir("desaloja");
        let c = DiskCache::open(&d, 30).await.unwrap();

        c.put(&id(1), &[0u8; 10]).await;
        c.put(&id(2), &[0u8; 10]).await;
        c.put(&id(3), &[0u8; 10]).await;
        // Se usan el 1 y el 3; el 2 queda como el mas antiguo.
        assert!(c.get(&id(1), tal_cual).await.is_some());
        assert!(c.get(&id(3), tal_cual).await.is_some());

        c.put(&id(4), &[0u8; 10]).await;

        assert!(c.stats().bytes <= 30);
        assert!(
            c.get(&id(2), tal_cual).await.is_none(),
            "el menos usado sale"
        );
        assert!(c.get(&id(4), tal_cual).await.is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Al abrir con menos capacidad que la que ya ocupaba, hay que recortar.
    #[tokio::test]
    async fn abrir_con_menos_capacidad_recorta() {
        let d = dir("recorta");
        {
            let c = DiskCache::open(&d, 1000).await.unwrap();
            for n in 0..10u8 {
                c.put(&id(n), &[0u8; 50]).await;
            }
            assert_eq!(c.stats().entradas, 10);
        }

        let c = DiskCache::open(&d, 200).await.unwrap();
        assert!(c.stats().bytes <= 200);
        assert!(c.stats().entradas <= 4);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Si alguien borra los ficheros por debajo, la cache falla, no revienta.
    #[tokio::test]
    async fn un_fichero_que_desaparece_es_un_fallo_no_un_panico() {
        let d = dir("desaparece");
        let c = DiskCache::open(&d, 1_000_000).await.unwrap();
        c.put(&id(1), b"aqui estoy").await;

        std::fs::remove_file(c.ruta(&id(1))).unwrap();

        assert!(c.get(&id(1), tal_cual).await.is_none());
        assert_eq!(c.stats().entradas, 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn capacidad_cero_no_guarda_nada() {
        let d = dir("cero");
        let c = DiskCache::open(&d, 0).await.unwrap();
        c.put(&id(1), b"x").await;
        assert!(c.get(&id(1), tal_cual).await.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod tests_con_almacen {
    use libredisk_raid::Layout;

    use super::*;
    use crate::ChunkStore;
    use crate::testutil::{clave, como_backends, contadores, reiniciar, total_gets};

    fn rs() -> Layout {
        Layout::ReedSolomon { k: 4, m: 2 }
    }

    fn dir(nombre: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "libredisk-dc2-{}-{nombre}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// El caso que justifica la cache en disco: **otro proceso** no vuelve a los proveedores.
    ///
    /// Se simula con dos `ChunkStore` distintos sobre los mismos proveedores y el mismo
    /// directorio de cache, que es exactamente lo que pasa entre dos ejecuciones.
    #[tokio::test]
    async fn un_almacen_nuevo_no_vuelve_a_los_proveedores() {
        let d = dir("frio");
        let layout = rs();
        let cs = contadores(layout.shard_count());
        let datos: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();

        // Primera "ejecucion": escribe y lee, llenando la cache de disco.
        let chunk = {
            let s = ChunkStore::new(layout, como_backends(&cs), clave())
                .unwrap()
                .with_cache(8 * 1024 * 1024)
                .with_disk_cache(DiskCache::open(&d, 64 * 1024 * 1024).await.unwrap());

            let chunk = s.put(&datos).await.unwrap().chunk;
            assert_eq!(s.get(&chunk).await.unwrap(), datos);
            chunk
        };

        // Segunda "ejecucion": almacen nuevo, memoria vacia, mismo directorio de cache.
        let s2 = ChunkStore::new(layout, como_backends(&cs), clave())
            .unwrap()
            .with_cache(8 * 1024 * 1024)
            .with_disk_cache(DiskCache::open(&d, 64 * 1024 * 1024).await.unwrap());

        reiniciar(&cs);
        assert_eq!(s2.get(&chunk).await.unwrap(), datos);
        assert_eq!(
            total_gets(&cs),
            0,
            "en frio, el disco tiene que bastar: cero peticiones a los proveedores"
        );
        assert_eq!(s2.disk_stats().unwrap().aciertos, 1);

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Lo que se deja en disco tiene que estar **cifrado**. Es la promesa del proyecto.
    #[tokio::test]
    async fn lo_que_queda_en_disco_no_esta_en_claro() {
        let d = dir("cifrado");
        let layout = rs();
        let s = ChunkStore::new(layout, crate::testutil::backends(6), clave())
            .unwrap()
            .with_disk_cache(DiskCache::open(&d, 64 * 1024 * 1024).await.unwrap());

        let secreto = b"NUMERO DE CUENTA 123456789".repeat(40);
        let chunk = s.put(&secreto).await.unwrap().chunk;
        s.get(&chunk).await.unwrap();

        let fichero = s.disco.as_ref().unwrap().ruta(&chunk.id);
        let bytes = std::fs::read(&fichero).unwrap();
        assert!(
            !bytes
                .windows(26)
                .any(|w| w == b"NUMERO DE CUENTA 123456789"),
            "el contenido aparece en claro en la cache de disco"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Un fichero de cache manipulado no puede colarse como bueno.
    #[tokio::test]
    async fn una_cache_manipulada_no_envenena_la_lectura() {
        let d = dir("manipulado");
        let layout = rs();
        let cs = contadores(layout.shard_count());
        let datos: Vec<u8> = (0..50_000).map(|i| (i % 251) as u8).collect();

        let s = ChunkStore::new(layout, como_backends(&cs), clave())
            .unwrap()
            .with_disk_cache(DiskCache::open(&d, 64 * 1024 * 1024).await.unwrap());

        let chunk = s.put(&datos).await.unwrap().chunk;
        s.get(&chunk).await.unwrap();

        // Alguien cambia un byte del fichero de cache.
        let fichero = s.disco.as_ref().unwrap().ruta(&chunk.id);
        let mut bytes = std::fs::read(&fichero).unwrap();
        bytes[100] ^= 0xFF;
        std::fs::write(&fichero, &bytes).unwrap();

        // La lectura sigue siendo correcta: se detecta, se tira, y se va a los proveedores.
        reiniciar(&cs);
        assert_eq!(s.get(&chunk).await.unwrap(), datos);
        assert_eq!(s.disk_stats().unwrap().corruptos, 1);
        assert!(
            total_gets(&cs) > 0,
            "al descartar la cache hay que ir a los proveedores"
        );

        let _ = std::fs::remove_dir_all(&d);
    }
}
