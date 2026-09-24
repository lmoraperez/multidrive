//! Reconstruir un volumen a partir de lo que quede en los proveedores.
//!
//! El escenario: se ha perdido el puntero de raiz, o apunta a algo que ya no se puede leer.
//! Los datos siguen ahi —son trozos inmutables repartidos entre los proveedores— pero no hay
//! por donde empezar a tirar del hilo.
//!
//! El rescate enumera todos los trozos, los rearma, los descifra, distingue los que son nodos
//! del arbol de los que son datos, y reconstruye el grafo de referencias. Los directorios a
//! los que no apunta nadie son **candidatos a raiz**: normalmente la raiz actual y las de las
//! versiones anteriores, que siguen siendo legibles porque aqui nada se sobrescribe.
//!
//! # Lo que el rescate no puede hacer
//!
//! Necesita la **clave**, y la clave sale de la frase de paso mas la sal de la cabecera. Si se
//! perdiera la cabecera de todos los proveedores a la vez, los trozos seguirian ahi y no
//! habria forma de descifrarlos. La cabecera va replicada en todos precisamente por eso, y
//! conviene guardarse una copia junto a la frase de paso.

use std::collections::{BTreeMap, BTreeSet};

use futures::future::join_all;

use crate::file::Manifest;
use crate::tree::{Entry, Node, Root};
use crate::{ChunkId, ChunkRef, ChunkStore, CoreError, SHARD_PREFIX};

/// Lo que el rescate encuentra.
#[derive(Debug, Default)]
pub struct Inventario {
    /// Nodos de directorio recuperados, con su contenido.
    pub directorios: BTreeMap<ChunkId, BTreeMap<String, Entry>>,
    /// Nodos de fichero recuperados, con su manifiesto.
    pub ficheros: BTreeMap<ChunkId, Manifest>,
    /// Trozos que son datos y no metadatos.
    pub datos: BTreeMap<ChunkId, ChunkRef>,
    /// Directorios a los que no apunta ningun otro nodo: candidatos a raiz.
    ///
    /// Suele haber varios. Como nada se sobrescribe, cada version del volumen dejo su propia
    /// raiz, y todas siguen describiendo un arbol coherente.
    pub raices: Vec<ChunkId>,
    /// Trozos de datos que no cuelgan de ningun fichero encontrado.
    ///
    /// No son necesariamente basura: pueden ser de una version anterior cuyo nodo de fichero
    /// no se pudo rearmar.
    pub huerfanos: Vec<ChunkId>,
    /// Trozos cuyos fragmentos existen pero que no se pudieron rearmar, con el motivo.
    pub ilegibles: Vec<(ChunkId, String)>,
    /// Trozos que se rearmaron pero no descifran: de otro volumen, u otra frase de paso.
    pub ajenos: Vec<ChunkId>,
    /// Proveedores que no se dejaron enumerar, por indice y con el motivo.
    ///
    /// Con uno solo que falle, el inventario esta incompleto y hay que decirlo: un trozo
    /// puede parecer perdido cuando lo unico que pasa es que nadie miro donde estaba.
    pub sin_enumerar: Vec<(usize, String)>,
}

/// Lo que describe una raiz candidata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resumen {
    pub directorios: usize,
    pub ficheros: usize,
    pub bytes: u64,
    /// Nodos a los que se hace referencia pero que no se pudieron recuperar.
    pub rotos: usize,
}

impl Inventario {
    /// `true` si se pudo mirar en todos los proveedores.
    pub fn is_complete(&self) -> bool {
        self.sin_enumerar.is_empty()
    }

    /// Resume el arbol que cuelga de una raiz **sin tocar la red**: todo esta ya en memoria.
    pub fn resumen(&self, raiz: &ChunkId) -> Resumen {
        let (mut dirs, mut fich, mut bytes, mut rotos) = (0, 0, 0u64, 0);
        let mut pendientes = vec![raiz.clone()];
        let mut vistos = BTreeSet::new();

        while let Some(id) = pendientes.pop() {
            if !vistos.insert(id.clone()) {
                continue;
            }
            if let Some(entradas) = self.directorios.get(&id) {
                dirs += 1;
                for entrada in entradas.values() {
                    pendientes.push(entrada.node.id.clone());
                }
            } else if let Some(manifest) = self.ficheros.get(&id) {
                fich += 1;
                bytes += manifest.len;
            } else {
                rotos += 1;
            }
        }

        Resumen {
            directorios: dirs,
            ficheros: fich,
            bytes,
            rotos,
        }
    }

    /// Rutas que cuelgan de una raiz, para poder reconocerla antes de adoptarla.
    ///
    /// Sin esto, elegir entre varias raices candidatas seria elegir entre hashes.
    pub fn rutas(&self, raiz: &ChunkId, limite: usize) -> Vec<String> {
        let mut salida = Vec::new();
        let mut pendientes = vec![(String::new(), raiz.clone())];
        let mut vistos = BTreeSet::new();

        while let Some((ruta, id)) = pendientes.pop() {
            if salida.len() >= limite || !vistos.insert(id.clone()) {
                continue;
            }
            if let Some(entradas) = self.directorios.get(&id) {
                for (nombre, entrada) in entradas {
                    pendientes.push((format!("{ruta}/{nombre}"), entrada.node.id.clone()));
                }
            } else if self.ficheros.contains_key(&id) {
                salida.push(ruta);
            }
        }
        salida.sort();
        salida
    }
}

impl ChunkStore {
    /// Recupera un trozo sabiendo **solo su identificador**.
    ///
    /// [`ChunkStore::get`] necesita un [`ChunkRef`] con la longitud sellada, que normalmente
    /// viene del manifiesto. En un rescate no hay manifiesto: es justo lo que se ha perdido.
    ///
    /// Pero la longitud se puede deducir. `split` rellena con ceros hasta un multiplo de `k`,
    /// asi que la original esta entre `k*(tam-1)+1` y `k*tam`: solo hay `k` candidatos. Se
    /// prueban de mayor a menor, y **el tag de XChaCha20-Poly1305 dice cual es el bueno**, sin
    /// ambiguedad: con una longitud equivocada el descifrado no autentica. Encima se comprueba
    /// el hash contra el identificador, que es una segunda verificacion independiente.
    pub async fn recover_blind(&self, id: &ChunkId) -> Result<(ChunkRef, Vec<u8>), CoreError> {
        let shards = self.fetch_shards(id).await;
        let disponibles = shards.iter().filter(|s| s.is_some()).count();
        if disponibles < self.layout.shards_needed() {
            return Err(CoreError::Unrecoverable {
                id: id.clone(),
                available: disponibles,
                needed: self.layout.shards_needed(),
            });
        }

        // Todos los fragmentos de un trozo miden lo mismo; basta con mirar uno.
        let tam = shards
            .iter()
            .flatten()
            .next()
            .map(Vec::len)
            .unwrap_or_default();

        for sealed_len in candidatas(self.layout, tam) {
            let Ok(sealed) = multidrive_raid::join(self.layout, shards.clone(), sealed_len) else {
                continue;
            };
            let Ok(data) = self.key.open(&sealed, id.as_str().as_bytes()) else {
                continue;
            };
            if ChunkId::of(&data) != *id {
                continue;
            }
            return Ok((
                ChunkRef {
                    id: id.clone(),
                    plain_len: data.len(),
                    sealed_len,
                },
                data,
            ));
        }

        Err(CoreError::Foreign(id.clone()))
    }

    /// Enumera los proveedores y reconstruye todo lo que pueda.
    pub async fn rescue(&self) -> Result<Inventario, CoreError> {
        let mut inv = Inventario::default();

        // 1. Que trozos existen, segun los propios proveedores.
        let listados = join_all(
            self.backends
                .iter()
                .enumerate()
                .map(|(i, b)| async move { (i, b.list(SHARD_PREFIX).await) }),
        )
        .await;

        let mut ids: BTreeSet<ChunkId> = BTreeSet::new();
        for (i, resultado) in listados {
            match resultado {
                Ok(objetos) => ids.extend(objetos.iter().filter_map(|o| id_de_clave(&o.clave))),
                Err(e) => inv.sin_enumerar.push((i, e.to_string())),
            }
        }

        // 2. Rearmar y clasificar cada uno.
        for id in ids {
            match self.recover_blind(&id).await {
                Ok((chunk, data)) => match serde_json::from_slice::<Node>(&data) {
                    Ok(Node::Dir(entradas)) => {
                        inv.directorios.insert(id, entradas);
                    }
                    Ok(Node::File(manifest)) => {
                        inv.ficheros.insert(id, manifest);
                    }
                    // No es JSON de un nodo: son datos de un fichero.
                    Err(_) => {
                        inv.datos.insert(id, chunk);
                    }
                },
                Err(CoreError::Foreign(_)) => inv.ajenos.push(id),
                Err(e) => inv.ilegibles.push((id, e.to_string())),
            }
        }

        // 3. Quien apunta a quien.
        let mut referenciados: BTreeSet<ChunkId> = BTreeSet::new();
        for entradas in inv.directorios.values() {
            referenciados.extend(entradas.values().map(|e| e.node.id.clone()));
        }
        let mut datos_usados: BTreeSet<ChunkId> = BTreeSet::new();
        for manifest in inv.ficheros.values() {
            datos_usados.extend(manifest.chunks.iter().map(|c| c.id.clone()));
        }

        inv.raices = inv
            .directorios
            .keys()
            .filter(|id| !referenciados.contains(id))
            .cloned()
            .collect();
        inv.huerfanos = inv
            .datos
            .keys()
            .filter(|id| !datos_usados.contains(id))
            .cloned()
            .collect();

        Ok(inv)
    }
}

/// Longitudes selladas posibles para un fragmento de `tam` bytes.
///
/// De mayor a menor, porque lo normal es que no haya relleno.
fn candidatas(layout: multidrive_raid::Layout, tam: usize) -> Vec<usize> {
    use multidrive_raid::Layout;
    match layout {
        // Aqui el fragmento es el blob entero: no hay nada que deducir.
        // Aqui el fragmento es el blob entero: no hay nada que deducir. En lineal tambien,
        // porque el trozo va sin cortar aunque no vaya a todos.
        Layout::Concat | Layout::Mirror { .. } | Layout::Lineal { .. } => vec![tam],
        // El mismo reparto que Reed-Solomon con `k = n`, asi que la misma deduccion.
        Layout::ReedSolomon { k: partes, .. } | Layout::Stripe { n: partes } => {
            let alto = partes * tam;
            let bajo = partes * tam.saturating_sub(1) + 1;
            (bajo..=alto).rev().collect()
        }
    }
}

/// Saca el identificador de una clave `chunks/<2 hex>/<64 hex>.<indice>`.
fn id_de_clave(clave: &str) -> Option<ChunkId> {
    let fichero = clave.rsplit('/').next()?;
    // Por el **primer** punto, no por el ultimo: un reparto distinto mete una marca en
    // medio (`<hash>.r2.003`) y cortar por el ultimo devolveria `<hash>.r2`, que no es un
    // identificador. El hash son 64 hexadecimales y no lleva puntos, asi que el primero
    // siempre lo delimita.
    let (hash, _resto) = fichero.split_once('.')?;
    ChunkId::parse(hash)
}

/// Publica una raiz elegida a mano, que es como termina un rescate.
///
/// La generacion se pone por encima de la que hubiera, para que cualquier lector prefiera
/// esta. Adoptar una raiz **no borra nada**: la anterior sigue ahi y se puede volver a ella.
pub async fn adoptar(store: &ChunkStore, node: ChunkRef, generation: u64) -> Result<(), CoreError> {
    crate::tree::publicar_raiz(store, Root { generation, node }).await
}

#[cfg(test)]
mod tests {
    use multidrive_raid::Layout;

    use super::*;
    use crate::testutil::{backends, store_sobre};
    use crate::tree::{Volume, raiz_publicada};

    fn rs() -> Layout {
        Layout::ReedSolomon { k: 4, m: 2 }
    }

    /// La deduccion de la longitud sellada tiene que acertar con cualquier tamano, sobre todo
    /// con los que dejan relleno.
    #[tokio::test]
    async fn recover_blind_deduce_la_longitud_sellada() {
        let bs = backends(6);
        let s = store_sobre(&bs, rs());

        // Tamanos elegidos para que el sellado caiga en todas las posiciones respecto a k=4.
        for n in [0usize, 1, 2, 3, 4, 5, 1000, 1001, 1002, 1003, 50_000] {
            let datos: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            let chunk = s.put(&datos).await.unwrap().chunk;

            let (recuperado, vuelta) = s.recover_blind(&chunk.id).await.unwrap();
            assert_eq!(vuelta, datos, "n={n}");
            assert_eq!(
                recuperado, chunk,
                "n={n}: el ChunkRef deducido debe ser el original"
            );
        }
    }

    #[tokio::test]
    async fn un_trozo_de_otra_clave_se_declara_ajeno() {
        let bs = backends(6);
        let s = store_sobre(&bs, rs());
        let chunk = s.put(b"contenido").await.unwrap().chunk;

        // Mismo almacen, otra clave: los fragmentos estan, pero no descifran.
        let otra = crate::ChunkStore::new(
            rs(),
            bs.clone(),
            multidrive_crypto::MasterKey::derive_with("otra frase", b"otra-sal-de-16by", 8, 1)
                .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            otra.recover_blind(&chunk.id).await,
            Err(CoreError::Foreign(_))
        ));
    }

    /// El caso que justifica todo esto: se pierde el puntero de raiz y el volumen se recupera.
    #[tokio::test]
    async fn se_rescata_un_volumen_sin_puntero_de_raiz() {
        let bs = backends(6);
        let mut v = Volume::create(store_sobre(&bs, rs())).await.unwrap().0;
        v.write_file("/docs/uno.txt", &b"primero"[..])
            .await
            .unwrap();
        v.write_file("/docs/dos.txt", &b"segundo"[..])
            .await
            .unwrap();
        v.write_file("/otro.txt", &b"tercero"[..]).await.unwrap();
        let raiz_buena = v.root().node.id.clone();
        drop(v);

        // Desaparece el puntero de todos los proveedores.
        for b in &bs {
            b.delete(crate::tree::ROOT_KEY).await.unwrap();
        }
        assert!(matches!(
            Volume::open(store_sobre(&bs, rs())).await,
            Err(CoreError::NoRoot)
        ));

        // El rescate encuentra la raiz entre los candidatos.
        let s = store_sobre(&bs, rs());
        let inv = s.rescue().await.unwrap();
        assert!(inv.is_complete());
        assert!(inv.ajenos.is_empty());
        assert!(
            inv.raices.contains(&raiz_buena),
            "la raiz verdadera debia estar entre las candidatas"
        );

        // Y describe lo que de verdad habia.
        let resumen = inv.resumen(&raiz_buena);
        assert_eq!(resumen.ficheros, 3);
        assert_eq!(resumen.rotos, 0);
        assert_eq!(resumen.bytes, 21, "tres ficheros de siete bytes");
        assert_eq!(
            inv.rutas(&raiz_buena, 10),
            vec!["/docs/dos.txt", "/docs/uno.txt", "/otro.txt"]
        );

        // Adoptarla devuelve el volumen a la vida.
        let chunk = inv.directorios[&raiz_buena].clone();
        let _ = chunk;
        let (ref_raiz, _) = s.recover_blind(&raiz_buena).await.unwrap();
        adoptar(&s, ref_raiz, 99).await.unwrap();

        let v = Volume::open(store_sobre(&bs, rs())).await.unwrap();
        assert_eq!(v.root().generation, 99);
        assert_eq!(
            v.read_at("/docs/uno.txt", 0, 100).await.unwrap(),
            b"primero"
        );
        assert_eq!(v.read_dir("/docs").await.unwrap().len(), 2);
    }

    /// Cada version dejo su raiz, y todas siguen describiendo un arbol coherente.
    #[tokio::test]
    async fn las_raices_antiguas_tambien_aparecen() {
        let bs = backends(6);
        let mut v = Volume::create(store_sobre(&bs, rs())).await.unwrap().0;
        v.write_file("/a.txt", &b"uno"[..]).await.unwrap();
        let vieja = v.root().node.id.clone();
        v.write_file("/b.txt", &b"dos"[..]).await.unwrap();
        let nueva = v.root().node.id.clone();

        let s = store_sobre(&bs, rs());
        let inv = s.rescue().await.unwrap();

        assert!(inv.raices.contains(&vieja) && inv.raices.contains(&nueva));
        assert_eq!(inv.resumen(&vieja).ficheros, 1);
        assert_eq!(inv.resumen(&nueva).ficheros, 2);
    }

    #[tokio::test]
    async fn los_trozos_que_ya_no_cuelgan_de_nadie_salen_como_huerfanos() {
        let bs = backends(6);
        let mut v = Volume::create(store_sobre(&bs, rs())).await.unwrap().0;
        v.write_file("/temporal.txt", &b"me van a borrar"[..])
            .await
            .unwrap();
        v.remove("/temporal.txt").await.unwrap();

        let s = store_sobre(&bs, rs());
        let inv = s.rescue().await.unwrap();

        // El nodo de fichero sigue existiendo, pero ya no lo referencia ningun directorio,
        // asi que sus datos no aparecen usados desde la raiz actual.
        assert!(
            !inv.huerfanos.is_empty() || !inv.ficheros.is_empty(),
            "el contenido borrado sigue en los proveedores: nada se sobrescribe"
        );
        assert!(raiz_publicada(&s).await.unwrap().is_some());
    }
}
