//! Cache de trozos en memoria.
//!
//! # Por que esta cache no necesita invalidacion
//!
//! No es una simplificacion ni un atajo: es consecuencia directa del diseno. Los trozos son
//! **inmutables** y su nombre es **su propio contenido** —el BLAKE3 de lo que llevan dentro—,
//! asi que una entrada cacheada no puede quedar obsoleta nunca. Si el identificador es el
//! mismo, los bytes son los mismos; si los bytes cambian, el identificador cambia y es otra
//! entrada.
//!
//! Lo unico mutable del volumen es el puntero de raiz, y ese no pasa por aqui.
//!
//! Eso convierte lo que en otros sistemas es la parte dificil —saber cuando tirar una
//! entrada— en un problema que sencillamente no existe. Solo queda decidir que desalojar
//! cuando no cabe mas, que es una cuestion de memoria, no de correccion.
//!
//! # Por que se acota por bytes y no por numero de entradas
//!
//! Los trozos de datos son de 16 MiB y los nodos del arbol son de unos cientos de bytes: hay
//! cinco ordenes de magnitud entre ellos. Contar entradas daria un consumo de memoria
//! imposible de predecir.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ChunkId;

/// Cuanto ha servido la cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub aciertos: u64,
    pub fallos: u64,
    /// Entradas expulsadas por falta de sitio.
    pub desalojos: u64,
    pub bytes: usize,
    pub entradas: usize,
}

impl Stats {
    /// Proporcion de aciertos, o `None` si todavia no se ha pedido nada.
    pub fn tasa_de_acierto(&self) -> Option<f64> {
        let total = self.aciertos + self.fallos;
        (total > 0).then(|| self.aciertos as f64 / total as f64)
    }
}

struct Entrada {
    datos: Arc<Vec<u8>>,
    /// Momento del ultimo uso, segun el reloj logico de la cache.
    ultimo_uso: u64,
}

struct Interior {
    entradas: HashMap<ChunkId, Entrada>,
    bytes: usize,
    reloj: u64,
    aciertos: u64,
    fallos: u64,
    desalojos: u64,
}

/// Cache de trozos acotada por bytes, con desalojo del menos usado recientemente.
pub struct ChunkCache {
    interior: Mutex<Interior>,
    capacidad: usize,
}

impl ChunkCache {
    /// Cache con `capacidad` bytes. Con 0, no guarda nada.
    pub fn new(capacidad: usize) -> Self {
        Self {
            interior: Mutex::new(Interior {
                entradas: HashMap::new(),
                bytes: 0,
                reloj: 0,
                aciertos: 0,
                fallos: 0,
                desalojos: 0,
            }),
            capacidad,
        }
    }

    pub fn capacidad(&self) -> usize {
        self.capacidad
    }

    /// Devuelve los datos si estan, y cuenta el acierto.
    pub fn get(&self, id: &ChunkId) -> Option<Arc<Vec<u8>>> {
        let mut i = self.lock();
        i.reloj += 1;
        let ahora = i.reloj;

        match i.entradas.get_mut(id) {
            Some(entrada) => {
                entrada.ultimo_uso = ahora;
                let datos = Arc::clone(&entrada.datos);
                i.aciertos += 1;
                Some(datos)
            }
            None => {
                i.fallos += 1;
                None
            }
        }
    }

    /// Guarda unos datos. Si no caben ni vaciando la cache, no se guardan.
    pub fn put(&self, id: ChunkId, datos: Arc<Vec<u8>>) {
        if self.capacidad == 0 || datos.len() > self.capacidad {
            return;
        }

        let mut i = self.lock();
        i.reloj += 1;
        let ahora = i.reloj;

        // Volver a guardar algo que ya esta solo refresca su uso: los bytes son los mismos,
        // porque el identificador es su hash.
        if let Some(entrada) = i.entradas.get_mut(&id) {
            entrada.ultimo_uso = ahora;
            return;
        }

        let tam = datos.len();
        i.entradas.insert(
            id,
            Entrada {
                datos,
                ultimo_uso: ahora,
            },
        );
        i.bytes += tam;

        // Desalojar hasta que quepa. Buscar el minimo es lineal, pero solo ocurre al
        // desbordar y el numero de entradas es pequeno: con trozos de 16 MiB, una cache de
        // 1 GiB son 64.
        while i.bytes > self.capacidad {
            let victima = i
                .entradas
                .iter()
                .min_by_key(|(_, e)| e.ultimo_uso)
                .map(|(id, _)| id.clone());

            match victima {
                Some(id) => {
                    if let Some(e) = i.entradas.remove(&id) {
                        i.bytes -= e.datos.len();
                        i.desalojos += 1;
                    }
                }
                None => break,
            }
        }
    }

    pub fn stats(&self) -> Stats {
        let i = self.lock();
        Stats {
            aciertos: i.aciertos,
            fallos: i.fallos,
            desalojos: i.desalojos,
            bytes: i.bytes,
            entradas: i.entradas.len(),
        }
    }

    /// Un mutex envenenado significa que un hilo se cayo teniendo la cache cogida.
    ///
    /// Aqui no importa: dentro solo hay datos ya verificados y contadores, y ninguna
    /// invariante que pueda haber quedado a medias de forma peligrosa. Tirar el volumen
    /// entero por una cache seria peor.
    fn lock(&self) -> std::sync::MutexGuard<'_, Interior> {
        self.interior.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for ChunkCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.stats();
        f.debug_struct("ChunkCache")
            .field("capacidad", &self.capacidad)
            .field("bytes", &s.bytes)
            .field("entradas", &s.entradas)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ChunkId {
        ChunkId::of(&[n])
    }

    fn datos(n: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![0u8; n])
    }

    #[test]
    fn guarda_y_devuelve() {
        let c = ChunkCache::new(1000);
        assert!(c.get(&id(1)).is_none());

        c.put(id(1), datos(100));
        assert_eq!(c.get(&id(1)).unwrap().len(), 100);

        let s = c.stats();
        assert_eq!((s.aciertos, s.fallos), (1, 1));
        assert_eq!(s.bytes, 100);
    }

    #[test]
    fn una_capacidad_de_cero_no_guarda_nada() {
        let c = ChunkCache::new(0);
        c.put(id(1), datos(10));
        assert!(c.get(&id(1)).is_none());
        assert_eq!(c.stats().entradas, 0);
    }

    /// Algo mas grande que la cache entera no se guarda: vaciarla para nada seria peor que
    /// no cachearlo.
    #[test]
    fn lo_que_no_cabe_ni_vaciandola_no_entra() {
        let c = ChunkCache::new(100);
        c.put(id(1), datos(50));
        c.put(id(2), datos(500));

        assert!(c.get(&id(2)).is_none());
        assert!(
            c.get(&id(1)).is_some(),
            "y no se llevo por delante lo que si cabia"
        );
    }

    #[test]
    fn desaloja_el_menos_usado_recientemente() {
        let c = ChunkCache::new(300);
        c.put(id(1), datos(100));
        c.put(id(2), datos(100));
        c.put(id(3), datos(100));

        // Se usan el 1 y el 3, pero no el 2.
        assert!(c.get(&id(1)).is_some());
        assert!(c.get(&id(3)).is_some());

        // Al meter un cuarto, el que sobra es el 2.
        c.put(id(4), datos(100));

        assert!(c.get(&id(2)).is_none(), "el menos usado debia salir");
        assert!(c.get(&id(1)).is_some());
        assert!(c.get(&id(3)).is_some());
        assert!(c.get(&id(4)).is_some());
        assert_eq!(c.stats().desalojos, 1);
    }

    #[test]
    fn nunca_se_pasa_de_la_capacidad() {
        let c = ChunkCache::new(1000);
        for n in 0..50u8 {
            c.put(id(n), datos(100));
            assert!(
                c.stats().bytes <= 1000,
                "se paso de capacidad en la iteracion {n}"
            );
        }
        assert_eq!(c.stats().entradas, 10);
    }

    /// Guardar dos veces lo mismo no puede duplicar la cuenta de bytes.
    #[test]
    fn guardar_lo_mismo_dos_veces_no_ocupa_el_doble() {
        let c = ChunkCache::new(1000);
        c.put(id(1), datos(100));
        c.put(id(1), datos(100));
        assert_eq!(c.stats().bytes, 100);
        assert_eq!(c.stats().entradas, 1);
    }

    #[test]
    fn la_tasa_de_acierto_solo_existe_si_se_pidio_algo() {
        let c = ChunkCache::new(1000);
        assert_eq!(c.stats().tasa_de_acierto(), None);

        c.put(id(1), datos(10));
        c.get(&id(1));
        c.get(&id(2));
        assert_eq!(c.stats().tasa_de_acierto(), Some(0.5));
    }
}

#[cfg(test)]
mod tests_con_almacen {
    use multidrive_raid::Layout;

    use crate::ChunkStore;
    use crate::testutil::{clave, como_backends, contadores, reiniciar, total_gets};

    fn rs() -> Layout {
        Layout::ReedSolomon { k: 4, m: 2 }
    }

    fn datos(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i.wrapping_mul(31) % 251) as u8).collect()
    }

    /// Lo unico que justifica una cache: releer no puede volver a la red.
    #[tokio::test]
    async fn releer_no_vuelve_a_la_red() {
        let layout = rs();
        let cs = contadores(layout.shard_count());
        let s = ChunkStore::new(layout, como_backends(&cs), clave())
            .unwrap()
            .with_cache(64 * 1024 * 1024);

        let d = datos(100_000);
        let chunk = s.put(&d).await.unwrap().chunk;
        reiniciar(&cs);

        assert_eq!(s.get(&chunk).await.unwrap(), d);
        assert_eq!(
            total_gets(&cs),
            layout.shard_count(),
            "la primera lectura si va a la red"
        );

        for _ in 0..10 {
            assert_eq!(s.get(&chunk).await.unwrap(), d);
        }
        assert_eq!(
            total_gets(&cs),
            layout.shard_count(),
            "diez relecturas no pueden costar ni una peticion mas"
        );

        let stats = s.cache_stats().unwrap();
        assert_eq!((stats.aciertos, stats.fallos), (10, 1));
    }

    /// Sin cache, cada lectura vuelve a la red. Es la comparacion que da sentido a la de
    /// arriba: sin esto, aquel test podria estar pasando por otro motivo.
    #[tokio::test]
    async fn sin_cache_cada_lectura_va_a_la_red() {
        let layout = rs();
        let cs = contadores(layout.shard_count());
        let s = ChunkStore::new(layout, como_backends(&cs), clave()).unwrap();

        let chunk = s.put(&datos(100_000)).await.unwrap().chunk;
        reiniciar(&cs);

        for _ in 0..5 {
            s.get(&chunk).await.unwrap();
        }
        assert_eq!(total_gets(&cs), 5 * layout.shard_count());
        assert!(s.cache_stats().is_none());
    }

    /// El caso del montaje: el sistema de ficheros lee de 4 KiB en 4 KiB, y sin cache cada
    /// una de esas lecturas reconstruye el trozo entero desde los proveedores.
    #[tokio::test]
    async fn muchas_lecturas_pequenas_cuestan_un_solo_trozo() {
        const TROZO: usize = 64 * 1024;
        let layout = rs();
        let cs = contadores(layout.shard_count());
        let s = ChunkStore::new(layout, como_backends(&cs), clave())
            .unwrap()
            .with_cache(8 * 1024 * 1024);

        let d = datos(TROZO);
        let m = s.put_file_with(&d[..], TROZO).await.unwrap().manifest;
        assert_eq!(m.chunks.len(), 1);
        reiniciar(&cs);

        // Dieciseis lecturas de 4 KiB sobre el mismo trozo.
        for i in 0..16u64 {
            let trozo = s.read_at(&m, i * 4096, 4096).await.unwrap();
            assert_eq!(trozo, &d[(i as usize) * 4096..(i as usize + 1) * 4096]);
        }

        assert_eq!(
            total_gets(&cs),
            layout.shard_count(),
            "un solo trozo bajado, no dieciseis"
        );
    }

    /// Recorrer el arbol relee los mismos nodos una y otra vez: es donde mas se nota.
    #[tokio::test]
    async fn recorrer_rutas_reaprovecha_los_nodos() {
        use crate::testutil::store_sobre;
        use crate::tree::Volume;

        let layout = rs();
        let cs = contadores(layout.shard_count());
        let backends = como_backends(&cs);

        let mut v = Volume::create(store_sobre(&backends, layout))
            .await
            .unwrap()
            .0;
        for i in 0..5 {
            v.write_file(&format!("/a/b/c/f{i}.txt"), &b"x"[..])
                .await
                .unwrap();
        }
        drop(v);

        let con_cache = Volume::open(
            ChunkStore::new(layout, backends.clone(), clave())
                .unwrap()
                .with_cache(8 * 1024 * 1024),
        )
        .await
        .unwrap();
        reiniciar(&cs);
        for i in 0..5 {
            con_cache
                .lookup(&format!("/a/b/c/f{i}.txt"))
                .await
                .unwrap()
                .unwrap();
        }
        let con = total_gets(&cs);

        let sin_cache = Volume::open(ChunkStore::new(layout, backends, clave()).unwrap())
            .await
            .unwrap();
        reiniciar(&cs);
        for i in 0..5 {
            sin_cache
                .lookup(&format!("/a/b/c/f{i}.txt"))
                .await
                .unwrap()
                .unwrap();
        }
        let sin = total_gets(&cs);

        assert!(
            con * 3 < sin,
            "con cache deberian ser muchas menos peticiones: {con} frente a {sin}"
        );
    }
}
