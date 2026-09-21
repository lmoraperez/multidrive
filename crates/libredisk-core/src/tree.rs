//! Arbol de metadatos *copy-on-write*.
//!
//! La verdad del volumen vive en los proveedores, no en el disco local. El arbol es una
//! estructura de nodos **inmutables**, y cada nodo es un trozo mas: pasa por el mismo
//! [`ChunkStore`] que los datos, asi que hereda cifrado, reparto RAID, direccionamiento por
//! contenido y deduplicacion sin una linea de codigo propia. Los nombres de fichero quedan
//! cifrados porque el nodo entero lo esta.
//!
//! Modificar algo reescribe los nodos desde la hoja hasta la raiz y termina intercambiando un
//! unico puntero. Eso es lo que da atomicidad sobre almacenes que no tienen ni renombrado
//! atomico ni escritura en sitio, que es exactamente el caso de un bucket S3 o de Drive.
//!
//! Efecto secundario que sale gratis: como nada se sobrescribe, **una raiz vieja sigue siendo
//! legible**. Eso son instantaneas sin codigo adicional; ver el test
//! `una_raiz_vieja_sigue_siendo_legible`.

use std::collections::BTreeMap;

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncRead;

use crate::file::Manifest;
use crate::{ChunkRef, ChunkStore, CoreError};

/// Clave conocida donde vive el puntero a la raiz.
///
/// Es un objeto suelto y no un registro dentro de una base de datos, a proposito: cualquier
/// lector que tenga la frase de paso arranca leyendo esta unica clave, sin entender ningun
/// esquema. Es el **unico objeto mutable del sistema**, y por tanto el unico sitio donde vive
/// la atomicidad.
pub const ROOT_KEY: &str = "root";

/// Que hay al otro lado de una entrada, sin tener que bajarlo para averiguarlo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Dir,
    File,
}

/// Una entrada de directorio.
///
/// Repite `kind` y `len` del nodo hijo a proposito. `read_dir` tiene que poder listar un
/// directorio con sus tamanos **sin bajar un nodo por entrada**: cada bajada es una peticion
/// por proveedor, y un directorio de cien ficheros serian seiscientas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// El nodo hijo, como cualquier otro trozo.
    pub node: ChunkRef,
    pub kind: Kind,
    /// Bytes, si es un fichero. Numero de entradas, si es un directorio.
    pub len: u64,
}

/// Un nodo del arbol. Inmutable, como todo trozo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    Dir(BTreeMap<String, Entry>),
    File(Manifest),
}

/// Puntero a la raiz actual del volumen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    /// Sube de uno en uno con cada publicacion.
    ///
    /// Quien lee se queda con la mas alta que encuentre. Si una publicacion solo llego a
    /// algunos proveedores, la siguiente lectura sigue viendo la version nueva mientras
    /// quede un proveedor que la recibiera.
    pub generation: u64,
    pub node: ChunkRef,
}

/// Un volumen: un [`ChunkStore`] mas la raiz del arbol que vive en el.
pub struct Volume {
    store: ChunkStore,
    root: Root,
}

impl Volume {
    /// Crea un volumen vacio y publica su raiz.
    ///
    /// Falla si ya hay una raiz publicada: sobrescribirla dejaria el arbol anterior
    /// inalcanzable, que es perder el volumen sin decirlo.
    pub async fn create(store: ChunkStore) -> Result<(Self, Vec<String>), CoreError> {
        if leer_raiz(&store).await?.is_some() {
            return Err(CoreError::RootExists);
        }

        let vacia: BTreeMap<String, Entry> = BTreeMap::new();
        let (node, degraded) = poner_nodo(&store, &Node::Dir(vacia)).await?;
        let root = Root {
            generation: 0,
            node,
        };
        publicar(&store, &root).await?;
        Ok((Self { store, root }, degraded))
    }

    /// Abre un volumen existente leyendo el puntero de raiz.
    pub async fn open(store: ChunkStore) -> Result<Self, CoreError> {
        let root = leer_raiz(&store).await?.ok_or(CoreError::NoRoot)?;
        Ok(Self { store, root })
    }

    /// Abre el volumen tal y como era en una raiz concreta.
    ///
    /// Es la lectura de una instantanea: los nodos son inmutables, asi que una raiz vieja
    /// sigue describiendo un arbol completo y coherente mientras sus trozos existan.
    pub fn at(store: ChunkStore, root: Root) -> Self {
        Self { store, root }
    }

    pub fn store(&self) -> &ChunkStore {
        &self.store
    }

    /// La raiz actual. Guardarla permite volver luego con [`Volume::at`].
    pub fn root(&self) -> &Root {
        &self.root
    }

    /// Resuelve una ruta. `None` si no existe.
    ///
    /// La raiz no tiene entrada que la describa, asi que pedirla es un error de uso, no un
    /// "no existe": use [`Volume::read_dir`].
    pub async fn lookup(&self, path: &str) -> Result<Option<Entry>, CoreError> {
        let segs = segmentos(path)?;
        if segs.is_empty() {
            return Err(CoreError::InvalidPath(
                "la raiz no tiene entrada propia; use read_dir".into(),
            ));
        }
        self.buscar(&segs).await
    }

    /// Lista un directorio. La raiz es `"/"` o `""`.
    pub async fn read_dir(&self, path: &str) -> Result<BTreeMap<String, Entry>, CoreError> {
        let segs = segmentos(path)?;
        if segs.is_empty() {
            return self.leer_dir(&self.root.node.clone()).await;
        }
        match self.buscar(&segs).await? {
            Some(e) if e.kind == Kind::Dir => self.leer_dir(&e.node).await,
            Some(_) => Err(CoreError::NotADirectory(path.to_string())),
            None => Err(CoreError::PathNotFound(path.to_string())),
        }
    }

    /// Escribe un fichero, creando los directorios que falten por el camino.
    pub async fn write_file<R>(&mut self, path: &str, reader: R) -> Result<Vec<String>, CoreError>
    where
        R: AsyncRead + Unpin,
    {
        let segs = segmentos(path)?;
        if segs.is_empty() {
            return Err(CoreError::InvalidPath("la raiz no es un fichero".into()));
        }

        let report = self.store.put_file(reader).await?;
        let len = report.manifest.len;
        let mut degraded = report.degraded;

        let (node, del_nodo) = poner_nodo(&self.store, &Node::File(report.manifest)).await?;
        unir(&mut degraded, del_nodo);

        let del_arbol = self
            .aplicar(
                &segs,
                Some(Entry {
                    node,
                    kind: Kind::File,
                    len,
                }),
            )
            .await?;
        unir(&mut degraded, del_arbol);
        Ok(degraded)
    }

    /// Lee un rango de un fichero, bajando solo los trozos que el rango toca.
    pub async fn read_at(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, CoreError> {
        let manifest = self.manifiesto(path).await?;
        self.store.read_at(&manifest, offset, len).await
    }

    /// Reconstruye un fichero entero y lo escribe en `writer`.
    pub async fn read_file<W>(&self, path: &str, writer: W) -> Result<(), CoreError>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        let manifest = self.manifiesto(path).await?;
        self.store.get_file(&manifest, writer).await
    }

    /// El manifiesto de un fichero, para quien quiera leerlo entero o inspeccionarlo.
    pub async fn manifiesto(&self, path: &str) -> Result<Manifest, CoreError> {
        let entrada = self
            .lookup(path)
            .await?
            .ok_or_else(|| CoreError::PathNotFound(path.to_string()))?;
        if entrada.kind != Kind::File {
            return Err(CoreError::NotAFile(path.to_string()));
        }
        match sacar_nodo(&self.store, &entrada.node).await? {
            Node::File(m) => Ok(m),
            Node::Dir(_) => Err(CoreError::NotAFile(path.to_string())),
        }
    }

    /// Crea un directorio y los que falten por encima. No es error si ya existe.
    pub async fn mkdir_p(&mut self, path: &str) -> Result<Vec<String>, CoreError> {
        let segs = segmentos(path)?;
        if segs.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(e) = self.buscar(&segs).await? {
            return match e.kind {
                Kind::Dir => Ok(Vec::new()),
                Kind::File => Err(CoreError::NotADirectory(path.to_string())),
            };
        }

        let (node, mut degraded) = poner_nodo(&self.store, &Node::Dir(BTreeMap::new())).await?;
        let del_arbol = self
            .aplicar(
                &segs,
                Some(Entry {
                    node,
                    kind: Kind::Dir,
                    len: 0,
                }),
            )
            .await?;
        unir(&mut degraded, del_arbol);
        Ok(degraded)
    }

    /// Borra un fichero, o un directorio **vacio**.
    pub async fn remove(&mut self, path: &str) -> Result<Vec<String>, CoreError> {
        let segs = segmentos(path)?;
        if segs.is_empty() {
            return Err(CoreError::InvalidPath("la raiz no se borra".into()));
        }

        let entrada = self
            .buscar(&segs)
            .await?
            .ok_or_else(|| CoreError::PathNotFound(path.to_string()))?;
        if entrada.kind == Kind::Dir && entrada.len != 0 {
            return Err(CoreError::DirNotEmpty(path.to_string()));
        }

        self.aplicar(&segs, None).await
    }

    /// Recorre el arbol y devuelve todos los trozos que lo componen: los de los nodos y los
    /// de los ficheros.
    ///
    /// Lo que no se pueda leer se devuelve aparte en vez de abortar. Un `scrub` que se rinde
    /// en el primer nodo ilegible no sirve para diagnosticar nada, que es justo para lo que
    /// se llama a un scrub.
    pub async fn walk(&self) -> (Vec<ChunkRef>, Vec<(String, String)>) {
        let mut pendientes = vec![("/".to_string(), self.root.node.clone())];
        let mut chunks = Vec::new();
        let mut ilegibles = Vec::new();

        while let Some((ruta, nodo)) = pendientes.pop() {
            chunks.push(nodo.clone());
            match sacar_nodo(&self.store, &nodo).await {
                Ok(Node::Dir(entradas)) => {
                    for (nombre, entrada) in entradas {
                        let hija = if ruta.ends_with('/') {
                            format!("{ruta}{nombre}")
                        } else {
                            format!("{ruta}/{nombre}")
                        };
                        pendientes.push((hija, entrada.node));
                    }
                }
                Ok(Node::File(manifest)) => chunks.extend(manifest.chunks),
                Err(e) => ilegibles.push((ruta, e.to_string())),
            }
        }
        (chunks, ilegibles)
    }

    /// Recorre la ruta desde la raiz.
    async fn buscar(&self, segs: &[String]) -> Result<Option<Entry>, CoreError> {
        let (ultimo, padres) = match segs.split_last() {
            Some(p) => p,
            None => return Ok(None),
        };

        let mut dir = self.leer_dir(&self.root.node.clone()).await?;
        for seg in padres {
            match dir.get(seg) {
                Some(e) if e.kind == Kind::Dir => {
                    let siguiente = e.node.clone();
                    dir = self.leer_dir(&siguiente).await?;
                }
                Some(_) => return Err(CoreError::NotADirectory(seg.clone())),
                None => return Ok(None),
            }
        }
        Ok(dir.get(ultimo).cloned())
    }

    /// Sustituye (o borra) la entrada de `segs` y reescribe la rama hasta la raiz.
    ///
    /// Es el *path copying*: nada se modifica en sitio. Se bajan los directorios del camino,
    /// se aplica el cambio en el mas profundo, y se vuelve a subir escribiendo un nodo nuevo
    /// por nivel. Al final se publica la raiz, que es el unico momento en que el volumen
    /// cambia para quien lo lea.
    async fn aplicar(
        &mut self,
        segs: &[String],
        cambio: Option<Entry>,
    ) -> Result<Vec<String>, CoreError> {
        let Some((nombre, padres)) = segs.split_last() else {
            return Err(CoreError::InvalidPath("no hay nada que cambiar".into()));
        };

        // Al insertar se crean los directorios que falten; al borrar, su ausencia es un error.
        let crear = cambio.is_some();

        let mut cadena: Vec<BTreeMap<String, Entry>> = Vec::with_capacity(padres.len());
        let mut actual = self.leer_dir(&self.root.node.clone()).await?;

        for seg in padres {
            let siguiente = match actual.get(seg) {
                Some(e) if e.kind == Kind::Dir => {
                    let n = e.node.clone();
                    self.leer_dir(&n).await?
                }
                Some(_) => return Err(CoreError::NotADirectory(seg.clone())),
                None if crear => BTreeMap::new(),
                None => return Err(CoreError::PathNotFound(seg.clone())),
            };
            cadena.push(std::mem::replace(&mut actual, siguiente));
        }

        match cambio {
            Some(e) => {
                actual.insert(nombre.clone(), e);
            }
            None => {
                actual
                    .remove(nombre)
                    .ok_or_else(|| CoreError::PathNotFound(nombre.clone()))?;
            }
        }

        // De vuelta hacia arriba, un nodo nuevo por nivel.
        let mut degraded = Vec::new();
        let (mut hijo, d) = poner_nodo(&self.store, &Node::Dir(actual.clone())).await?;
        unir(&mut degraded, d);
        let mut len = actual.len() as u64;
        for (mut padre, seg) in cadena.into_iter().zip(padres).rev() {
            padre.insert(
                seg.clone(),
                Entry {
                    node: hijo,
                    kind: Kind::Dir,
                    len,
                },
            );
            len = padre.len() as u64;
            let (nuevo, d) = poner_nodo(&self.store, &Node::Dir(padre)).await?;
            unir(&mut degraded, d);
            hijo = nuevo;
        }

        let nueva = Root {
            generation: self.root.generation + 1,
            node: hijo,
        };
        publicar(&self.store, &nueva).await?;
        self.root = nueva;
        Ok(degraded)
    }

    async fn leer_dir(&self, node: &ChunkRef) -> Result<BTreeMap<String, Entry>, CoreError> {
        match sacar_nodo(&self.store, node).await? {
            Node::Dir(d) => Ok(d),
            Node::File(_) => Err(CoreError::NotADirectory(node.id.to_string())),
        }
    }
}

/// Publica el puntero de raiz en **todos** los proveedores.
///
/// Se replica en espejo y no se reparte con RAID, por dos razones: tiene que poder leerse
/// aunque solo sobreviva un proveedor, y ocupa unos cientos de bytes, asi que repartirlo no
/// ahorra nada y solo anade formas de fallar.
///
/// Va cifrado como cualquier otra cosa: en claro revelaria el identificador de la raiz, que
/// es el hash de su contenido.
async fn publicar(store: &ChunkStore, root: &Root) -> Result<(), CoreError> {
    let claro = serde_json::to_vec(root)?;
    let sellado = store.key.seal(&claro, ROOT_KEY.as_bytes())?;

    let escrituras = store.backends.iter().map(|b| {
        let datos = sellado.clone();
        async move { b.put(ROOT_KEY, datos).await }
    });

    let logradas = join_all(escrituras)
        .await
        .into_iter()
        .filter(Result::is_ok)
        .count();
    if logradas == 0 {
        return Err(CoreError::RootNotPublished);
    }
    Ok(())
}

/// Lee el puntero de raiz y se queda con la generacion mas alta.
///
/// Un puntero que no se abre o no se deserializa se ignora en silencio: puede ser una
/// publicacion a medias de un proveedor que fallo. Mientras quede uno bueno, el volumen abre.
async fn leer_raiz(store: &ChunkStore) -> Result<Option<Root>, CoreError> {
    let lecturas = store
        .backends
        .iter()
        .map(|b| async move { b.get(ROOT_KEY).await.ok() });

    let mut mejor: Option<Root> = None;
    for sellado in join_all(lecturas).await.into_iter().flatten() {
        let Ok(claro) = store.key.open(&sellado, ROOT_KEY.as_bytes()) else {
            continue;
        };
        let Ok(root) = serde_json::from_slice::<Root>(&claro) else {
            continue;
        };
        if mejor
            .as_ref()
            .is_none_or(|m| root.generation > m.generation)
        {
            mejor = Some(root);
        }
    }
    Ok(mejor)
}

/// Guarda un nodo como un trozo mas.
///
/// Devuelve tambien los proveedores que no aceptaron su fragmento. Tirar esa lista seria
/// prometer una redundancia que no se tiene, que es peor que no tenerla.
async fn poner_nodo(store: &ChunkStore, node: &Node) -> Result<(ChunkRef, Vec<String>), CoreError> {
    let bytes = serde_json::to_vec(node)?;
    let report = store.put(&bytes).await?;
    Ok((report.chunk, report.degraded))
}

/// Acumula nombres de proveedores degradados sin repetirlos.
fn unir(destino: &mut Vec<String>, nuevos: Vec<String>) {
    for n in nuevos {
        if !destino.contains(&n) {
            destino.push(n);
        }
    }
}

async fn sacar_nodo(store: &ChunkStore, node: &ChunkRef) -> Result<Node, CoreError> {
    let bytes = store.get(node).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Parte una ruta en segmentos.
///
/// Rechaza `..` en vez de resolverlo. El arbol no tiene punteros al padre, asi que
/// resolverlo exigiria recordar el camino, y una ruta que sube no significa lo mismo para
/// todos los llamantes. Un sistema de ficheros resuelve `..` antes de llegar aqui.
fn segmentos(path: &str) -> Result<Vec<String>, CoreError> {
    let mut out = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                return Err(CoreError::InvalidPath(format!("'{path}' contiene '..'")));
            }
            s => out.push(s.to_string()),
        }
    }
    Ok(out)
}

/// Publica una raiz concreta, saltandose el camino normal.
///
/// Existe para el rescate: cuando el puntero se ha perdido y hay que volver a apuntar a un
/// nodo que se encontro rebuscando. Fuera de ese caso, la raiz la publica [`Volume`] sola.
pub async fn publicar_raiz(store: &ChunkStore, root: Root) -> Result<(), CoreError> {
    publicar(store, &root).await
}

/// La raiz publicada ahora mismo, si la hay. Tambien para el rescate.
pub async fn raiz_publicada(store: &ChunkStore) -> Result<Option<Root>, CoreError> {
    leer_raiz(store).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use libredisk_backend::Backend;
    use libredisk_raid::Layout;

    use super::*;
    use crate::testutil::{
        backends, como_backends, contadores, reiniciar, store_sobre, total_gets,
    };

    fn rs() -> Layout {
        Layout::ReedSolomon { k: 4, m: 2 }
    }

    async fn nuevo(bs: &[Arc<dyn Backend>]) -> Volume {
        Volume::create(store_sobre(bs, rs())).await.unwrap().0
    }

    #[tokio::test]
    async fn crear_escribir_y_reabrir() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        v.write_file("/docs/notas.txt", &b"hola"[..]).await.unwrap();
        drop(v);

        // Otro almacen sobre los mismos proveedores: la raiz persiste fuera del proceso.
        let v = Volume::open(store_sobre(&bs, rs())).await.unwrap();
        assert_eq!(v.read_at("/docs/notas.txt", 0, 100).await.unwrap(), b"hola");
    }

    #[tokio::test]
    async fn los_directorios_intermedios_se_crean_solos() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        v.write_file("/a/b/c/hondo.txt", &b"abajo"[..])
            .await
            .unwrap();

        assert_eq!(v.read_dir("/").await.unwrap().len(), 1);
        assert_eq!(v.read_dir("/a/b").await.unwrap().len(), 1);
        assert_eq!(
            v.read_at("/a/b/c/hondo.txt", 0, 10).await.unwrap(),
            b"abajo"
        );
    }

    #[tokio::test]
    async fn read_dir_lista_con_tipo_y_tamano() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        v.write_file("/a.txt", &b"12345"[..]).await.unwrap();
        v.write_file("/dir/b.txt", &b"xy"[..]).await.unwrap();

        let raiz = v.read_dir("/").await.unwrap();
        assert_eq!(raiz.len(), 2);
        assert_eq!(raiz["a.txt"].kind, Kind::File);
        assert_eq!(raiz["a.txt"].len, 5);
        assert_eq!(raiz["dir"].kind, Kind::Dir);
        assert_eq!(
            raiz["dir"].len, 1,
            "un directorio declara cuantas entradas tiene"
        );
    }

    /// `read_dir` no puede bajar un nodo por entrada: seria una peticion por proveedor y por
    /// fichero. Para eso `Entry` repite el tipo y el tamano del hijo.
    #[tokio::test]
    async fn read_dir_no_baja_un_nodo_por_entrada() {
        let cs = contadores(6);
        let mut v = Volume::create(store_sobre(&como_backends(&cs), rs()))
            .await
            .unwrap()
            .0;
        for i in 0..8 {
            v.write_file(&format!("/dir/f{i}.txt"), &b"x"[..])
                .await
                .unwrap();
        }

        reiniciar(&cs);
        let entradas = v.read_dir("/dir").await.unwrap();

        assert_eq!(entradas.len(), 8);
        assert_eq!(
            total_gets(&cs),
            2 * 6,
            "la raiz y el directorio, y ni un nodo mas por los ocho ficheros"
        );
    }

    /// Como nada se sobrescribe, una raiz vieja sigue describiendo un arbol completo. Eso son
    /// instantaneas sin una linea de codigo dedicada.
    #[tokio::test]
    async fn una_raiz_vieja_sigue_siendo_legible() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        v.write_file("/a.txt", &b"primera"[..]).await.unwrap();
        let antigua = v.root().clone();

        v.write_file("/a.txt", &b"segunda version"[..])
            .await
            .unwrap();
        v.write_file("/b.txt", &b"nueva"[..]).await.unwrap();
        assert_eq!(
            v.read_at("/a.txt", 0, 100).await.unwrap(),
            b"segunda version"
        );

        let viejo = Volume::at(store_sobre(&bs, rs()), antigua);
        assert_eq!(viejo.read_at("/a.txt", 0, 100).await.unwrap(), b"primera");
        assert!(
            viejo.lookup("/b.txt").await.unwrap().is_none(),
            "la raiz vieja no puede ver lo que se escribio despues"
        );
    }

    #[tokio::test]
    async fn la_generacion_sube_con_cada_cambio() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        assert_eq!(v.root().generation, 0);

        v.write_file("/a", &b"1"[..]).await.unwrap();
        assert_eq!(v.root().generation, 1);
        v.write_file("/b", &b"2"[..]).await.unwrap();
        assert_eq!(v.root().generation, 2);
        v.remove("/a").await.unwrap();
        assert_eq!(v.root().generation, 3);
    }

    #[tokio::test]
    async fn borrar_un_directorio_con_cosas_dentro_falla() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        v.write_file("/dir/a.txt", &b"x"[..]).await.unwrap();

        assert!(matches!(
            v.remove("/dir").await,
            Err(CoreError::DirNotEmpty(_))
        ));

        v.remove("/dir/a.txt").await.unwrap();
        v.remove("/dir").await.unwrap();
        assert!(v.read_dir("/").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn mkdir_p_es_idempotente_y_no_pisa_ficheros() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;

        v.mkdir_p("/x/y").await.unwrap();
        v.mkdir_p("/x/y").await.unwrap();
        assert_eq!(v.read_dir("/x/y").await.unwrap().len(), 0);

        v.write_file("/x/fichero", &b"x"[..]).await.unwrap();
        assert!(matches!(
            v.mkdir_p("/x/fichero").await,
            Err(CoreError::NotADirectory(_))
        ));
    }

    #[tokio::test]
    async fn las_rutas_que_suben_se_rechazan() {
        let bs = backends(6);
        let v = nuevo(&bs).await;

        assert!(matches!(
            v.lookup("/a/../b").await,
            Err(CoreError::InvalidPath(_))
        ));
        // Las barras de mas y los "." son inofensivos.
        v.read_dir("//.//").await.unwrap();
    }

    #[tokio::test]
    async fn lookup_de_lo_que_no_existe_es_none() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        v.write_file("/a.txt", &b"x"[..]).await.unwrap();

        assert!(v.lookup("/b.txt").await.unwrap().is_none());
        assert!(
            v.lookup("/no/existe/nada").await.unwrap().is_none(),
            "un directorio intermedio que falta es 'no existe', no un error"
        );
    }

    #[tokio::test]
    async fn crear_encima_de_un_volumen_existente_falla() {
        let bs = backends(6);
        nuevo(&bs).await;

        assert!(
            matches!(
                Volume::create(store_sobre(&bs, rs())).await,
                Err(CoreError::RootExists)
            ),
            "sobrescribir la raiz dejaria el arbol anterior inalcanzable"
        );
    }

    #[tokio::test]
    async fn abrir_donde_no_hay_nada_falla() {
        let bs = backends(6);
        assert!(matches!(
            Volume::open(store_sobre(&bs, rs())).await,
            Err(CoreError::NoRoot)
        ));
    }

    /// El puntero va en espejo y no repartido con RAID: tiene que poder leerse aunque solo
    /// sobreviva un proveedor.
    #[tokio::test]
    async fn la_raiz_se_lee_aunque_solo_quede_un_proveedor() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        v.write_file("/a.txt", &b"contenido"[..]).await.unwrap();
        drop(v);

        for b in bs.iter().take(5) {
            b.delete(ROOT_KEY).await.unwrap();
        }

        let v = Volume::open(store_sobre(&bs, rs())).await.unwrap();
        assert_eq!(v.read_at("/a.txt", 0, 100).await.unwrap(), b"contenido");
    }

    /// ARQUITECTURA promete que los nombres tambien van cifrados. Como un nodo es un trozo
    /// mas, sale gratis: el proveedor solo ve un blob opaco.
    #[tokio::test]
    async fn los_nombres_de_fichero_no_se_ven_en_los_proveedores() {
        let bs = backends(6);
        let mut v = nuevo(&bs).await;
        v.write_file("/nominas-2026.txt", &b"x"[..]).await.unwrap();

        for b in &bs {
            for clave in b.list("").await.unwrap() {
                let bytes = b.get(&clave).await.unwrap();
                assert!(
                    !bytes.windows(8).any(|w| w == b"nominas-"),
                    "el nombre aparece en claro en {} bajo '{clave}'",
                    b.name()
                );
            }
        }
    }
}
