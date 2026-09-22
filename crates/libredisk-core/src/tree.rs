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

/// Un cambio en el arbol: poner una entrada en una ruta, o quitarla.
type Cambio = (Vec<String>, Option<Entry>);

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

    /// Crea un fichero vacio. Falla si ya existe algo en esa ruta.
    pub async fn create_file(&mut self, path: &str) -> Result<Vec<String>, CoreError> {
        let segs = segmentos(path)?;
        if segs.is_empty() {
            return Err(CoreError::InvalidPath("la raiz no es un fichero".into()));
        }
        if self.buscar(&segs).await?.is_some() {
            return Err(CoreError::InvalidPath(format!("'{path}' ya existe")));
        }
        self.publicar_fichero(&segs, Manifest::empty(crate::file::CHUNK_SIZE))
            .await
    }

    /// Escribe `datos` en `offset`, creando el fichero si no existe.
    ///
    /// Ojo con el coste: escribir dentro de un fichero reescribe los trozos que el rango
    /// toque, enteros. Ver [`ChunkStore::write_at`]. Quien llame muchas veces seguidas sobre
    /// el mismo trozo deberia acumular antes en un buffer.
    pub async fn write_at(
        &mut self,
        path: &str,
        offset: u64,
        datos: &[u8],
    ) -> Result<Vec<String>, CoreError> {
        let segs = segmentos(path)?;
        if segs.is_empty() {
            return Err(CoreError::InvalidPath("la raiz no es un fichero".into()));
        }

        let actual = match self.buscar(&segs).await? {
            Some(e) if e.kind == Kind::File => self.manifiesto_de(&e).await?,
            Some(_) => return Err(CoreError::NotAFile(path.to_string())),
            None => Manifest::empty(crate::file::CHUNK_SIZE),
        };

        let (nuevo, mut degraded) = self.store.write_at(&actual, offset, datos).await?;
        let del_arbol = self.publicar_fichero(&segs, nuevo).await?;
        unir(&mut degraded, del_arbol);
        Ok(degraded)
    }

    /// Cambia la longitud de un fichero: recorta, o extiende con ceros.
    pub async fn truncate(&mut self, path: &str, nueva_len: u64) -> Result<Vec<String>, CoreError> {
        let segs = segmentos(path)?;
        let entrada = self
            .buscar(&segs)
            .await?
            .ok_or_else(|| CoreError::PathNotFound(path.to_string()))?;
        if entrada.kind != Kind::File {
            return Err(CoreError::NotAFile(path.to_string()));
        }

        let actual = self.manifiesto_de(&entrada).await?;
        let (nuevo, mut degraded) = self.store.truncate(&actual, nueva_len).await?;
        let del_arbol = self.publicar_fichero(&segs, nuevo).await?;
        unir(&mut degraded, del_arbol);
        Ok(degraded)
    }

    /// Guarda un manifiesto como nodo y lo cuelga de la ruta.
    async fn publicar_fichero(
        &mut self,
        segs: &[String],
        manifest: Manifest,
    ) -> Result<Vec<String>, CoreError> {
        let len = manifest.len;
        let (node, mut degraded) = poner_nodo(&self.store, &Node::File(manifest)).await?;
        let del_arbol = self
            .aplicar(
                segs,
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

    /// El manifiesto que hay detras de una entrada ya resuelta.
    async fn manifiesto_de(&self, entrada: &Entry) -> Result<Manifest, CoreError> {
        match sacar_nodo(&self.store, &entrada.node).await? {
            Node::File(m) => Ok(m),
            Node::Dir(_) => Err(CoreError::NotAFile(entrada.node.id.to_string())),
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
        self.aplicar_varios(vec![(segs.to_vec(), cambio)]).await
    }

    /// Aplica varios cambios y publica **una sola raiz**.
    ///
    /// Existe por `rename`: mover un fichero es quitarlo de un sitio y ponerlo en otro, y
    /// hacerlo en dos publicaciones dejaria una ventana en la que el fichero no esta en
    /// ninguno de los dos. Un sistema de ficheros no puede permitirse eso.
    ///
    /// Es el *path copying* de siempre, pero sobre la union de las ramas tocadas: se cargan
    /// los directorios por los que pasan todos los cambios, se aplican en memoria, y se
    /// escriben de abajo arriba para que cada padre vea ya a sus hijos nuevos.
    async fn aplicar_varios(&mut self, cambios: Vec<Cambio>) -> Result<Vec<String>, CoreError> {
        if cambios.is_empty() {
            return Ok(Vec::new());
        }
        for (segs, _) in &cambios {
            if segs.is_empty() {
                return Err(CoreError::InvalidPath("la raiz no se puede cambiar".into()));
            }
        }

        let mut ramas = self.cargar_ramas(&cambios).await?;

        // Aplicar en memoria. A estas alturas todos los directorios implicados estan cargados.
        for (segs, cambio) in &cambios {
            let (nombre, padres) = segs.split_last().expect("comprobado arriba");
            let dir = ramas
                .get_mut(padres)
                .expect("cargar_ramas dejo cargado todo el camino");
            match cambio {
                Some(entrada) => {
                    dir.insert(nombre.clone(), entrada.clone());
                }
                None => {
                    dir.remove(nombre)
                        .ok_or_else(|| CoreError::PathNotFound(nombre.clone()))?;
                }
            }
        }

        // Escribir de abajo arriba: un padre tiene que escribirse cuando sus hijos ya tienen
        // identificador nuevo, o apuntaria a la version vieja.
        let mut rutas: Vec<Vec<String>> = ramas.keys().cloned().collect();
        rutas.sort_by_key(|r| std::cmp::Reverse(r.len()));

        let mut degraded = Vec::new();
        let mut raiz_nueva = None;

        for ruta in rutas {
            let contenido = ramas.get(&ruta).expect("esta en el mapa").clone();
            let len = contenido.len() as u64;
            let (node, d) = poner_nodo(&self.store, &Node::Dir(contenido)).await?;
            unir(&mut degraded, d);

            match ruta.split_last() {
                Some((nombre, padre)) => {
                    ramas
                        .get_mut(padre)
                        .expect("el padre se cargo con el hijo")
                        .insert(
                            nombre.clone(),
                            Entry {
                                node,
                                kind: Kind::Dir,
                                len,
                            },
                        );
                }
                None => raiz_nueva = Some(node),
            }
        }

        let nueva = Root {
            generation: self.root.generation + 1,
            node: raiz_nueva.expect("la raiz siempre esta en el mapa"),
        };
        publicar(&self.store, &nueva).await?;
        self.root = nueva;
        Ok(degraded)
    }

    /// Carga los directorios por los que pasan los cambios, indexados por su ruta.
    ///
    /// Se hace en una pasada aparte para no mezclar lecturas de red con la manipulacion del
    /// arbol en memoria: asi la parte delicada —aplicar los cambios— no tiene esperas dentro.
    async fn cargar_ramas(
        &self,
        cambios: &[Cambio],
    ) -> Result<BTreeMap<Vec<String>, BTreeMap<String, Entry>>, CoreError> {
        let mut ramas = BTreeMap::new();
        ramas.insert(Vec::new(), self.leer_dir(&self.root.node.clone()).await?);

        for (segs, cambio) in cambios {
            let (_, padres) = segs.split_last().expect("comprobado por el llamante");
            let mut prefijo: Vec<String> = Vec::new();

            for seg in padres {
                let mut siguiente = prefijo.clone();
                siguiente.push(seg.clone());

                if !ramas.contains_key(&siguiente) {
                    // Se clona la entrada para soltar el prestamo del mapa antes de esperar.
                    let entrada = ramas.get(&prefijo).and_then(|p| p.get(seg)).cloned();
                    let contenido = match entrada {
                        Some(e) if e.kind == Kind::Dir => self.leer_dir(&e.node).await?,
                        Some(_) => return Err(CoreError::NotADirectory(seg.clone())),
                        // Al insertar se crean los directorios que falten; al borrar, su
                        // ausencia es un error.
                        None if cambio.is_some() => BTreeMap::new(),
                        None => return Err(CoreError::PathNotFound(seg.clone())),
                    };
                    ramas.insert(siguiente.clone(), contenido);
                }
                prefijo = siguiente;
            }
        }
        Ok(ramas)
    }

    /// Mueve o renombra. `reemplazar` decide si puede pisar algo que ya exista en el destino.
    ///
    /// Los dos cambios —quitar del origen, poner en el destino— salen en **una sola raiz**,
    /// asi que para cualquier lector el fichero nunca deja de estar en alguno de los dos
    /// sitios.
    pub async fn rename(
        &mut self,
        origen: &str,
        destino: &str,
        reemplazar: bool,
    ) -> Result<Vec<String>, CoreError> {
        let de = segmentos(origen)?;
        let a = segmentos(destino)?;

        if de.is_empty() || a.is_empty() {
            return Err(CoreError::InvalidPath("la raiz no se mueve".into()));
        }
        if de == a {
            return Ok(Vec::new());
        }
        // Meter un directorio dentro de si mismo dejaria una rama inalcanzable y un ciclo.
        if a.len() > de.len() && a[..de.len()] == de[..] {
            return Err(CoreError::InvalidPath(format!(
                "'{destino}' esta dentro de '{origen}'"
            )));
        }

        let entrada = self
            .buscar(&de)
            .await?
            .ok_or_else(|| CoreError::PathNotFound(origen.to_string()))?;

        // El directorio de destino tiene que existir ya. Crearlo al vuelo, como hace
        // `write_file`, aqui esconderia errores: fue asi como un `rename` con la ruta en
        // mayusculas creo un directorio duplicado en vez de fallar.
        if let Some((_, padres)) = a.split_last()
            && !padres.is_empty()
            && !matches!(self.buscar(padres).await?, Some(e) if e.kind == Kind::Dir)
        {
            return Err(CoreError::PathNotFound(format!(
                "el directorio de destino de '{destino}' no existe"
            )));
        }

        if let Some(ya) = self.buscar(&a).await? {
            if !reemplazar {
                return Err(CoreError::InvalidPath(format!("'{destino}' ya existe")));
            }
            if ya.kind == Kind::Dir && ya.len != 0 {
                return Err(CoreError::DirNotEmpty(destino.to_string()));
            }
        }

        self.aplicar_varios(vec![(de, None), (a, Some(entrada))])
            .await
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
    // Con escritura diferida, la raiz **no se publica**: se encola. Publicarla ahora dejaria
    // el volumen apuntando a trozos que siguen sin subir, que es justo el estado que este
    // diseno no puede permitirse.
    if let Some(cola) = store.writeback() {
        return cola.stage_root(root.clone()).await;
    }
    publicar_ahora(store, root).await
}

/// Publica de verdad, sin mirar la cola. Es lo que usa `sync` cuando ya no queda nada arriba.
pub(crate) async fn publicar_ahora(store: &ChunkStore, root: &Root) -> Result<(), CoreError> {
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
    // `get_shared` en vez de `get`: los nodos se releen constantemente al recorrer rutas, y
    // con cache el acierto no cuesta ni una copia.
    let bytes = store.get_shared(node).await?;
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
            for objeto in b.list("").await.unwrap() {
                let clave = objeto.clave;
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

#[cfg(test)]
mod tests_escritura {
    use libredisk_raid::Layout;

    use super::*;
    use crate::file::Manifest;
    use crate::testutil::{backends, store, store_sobre};

    const TAM: usize = 256;

    fn rs() -> Layout {
        Layout::ReedSolomon { k: 4, m: 2 }
    }

    fn datos(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i.wrapping_mul(31) % 251) as u8).collect()
    }

    async fn volumen() -> Volume {
        let bs = backends(6);
        Volume::create(store_sobre(&bs, rs())).await.unwrap().0
    }

    #[tokio::test]
    async fn escribir_en_un_fichero_que_no_existe_lo_crea() {
        let mut v = volumen().await;
        v.write_at("/nuevo.txt", 0, b"hola").await.unwrap();

        assert_eq!(v.read_at("/nuevo.txt", 0, 100).await.unwrap(), b"hola");
        assert_eq!(v.lookup("/nuevo.txt").await.unwrap().unwrap().len, 4);
    }

    #[tokio::test]
    async fn sobrescribir_en_medio() {
        let mut v = volumen().await;
        v.write_at("/a.txt", 0, b"0123456789").await.unwrap();
        v.write_at("/a.txt", 3, b"XYZ").await.unwrap();

        assert_eq!(v.read_at("/a.txt", 0, 100).await.unwrap(), b"012XYZ6789");
        assert_eq!(v.lookup("/a.txt").await.unwrap().unwrap().len, 10);
    }

    #[tokio::test]
    async fn escribir_mas_alla_del_final_extiende() {
        let mut v = volumen().await;
        v.write_at("/a.txt", 0, b"abc").await.unwrap();
        v.write_at("/a.txt", 10, b"xyz").await.unwrap();

        let todo = v.read_at("/a.txt", 0, 100).await.unwrap();
        assert_eq!(todo.len(), 13);
        assert_eq!(&todo[..3], b"abc");
        assert_eq!(
            &todo[3..10],
            &[0u8; 7],
            "el hueco se rellena con ceros, como en cualquier sistema de ficheros"
        );
        assert_eq!(&todo[10..], b"xyz");
    }

    /// Los limites de trozo son donde esto se rompe si esta mal.
    #[tokio::test]
    async fn escribir_cruzando_fronteras_de_trozo() {
        let bs = backends(6);
        let s = store_sobre(&bs, rs());

        let original = datos(1000);
        let m = s.put_file_with(&original[..], TAM).await.unwrap().manifest;

        // Un parche que empieza en el trozo 0 y acaba en el 2.
        let parche = vec![0xAAu8; 400];
        let (nuevo, _) = s.write_at(&m, 200, &parche).await.unwrap();

        let mut esperado = original.clone();
        esperado[200..600].copy_from_slice(&parche);

        let mut salida = Vec::new();
        s.get_file(&nuevo, &mut salida).await.unwrap();
        assert_eq!(salida, esperado);
        assert_eq!(nuevo.len, 1000);
        nuevo.validate().unwrap();
    }

    #[tokio::test]
    async fn truncar_recorta_y_extiende() {
        let bs = backends(6);
        let s = store_sobre(&bs, rs());
        let original = datos(1000);
        let m = s.put_file_with(&original[..], TAM).await.unwrap().manifest;

        // Recortar.
        let (corto, _) = s.truncate(&m, 300).await.unwrap();
        assert_eq!(corto.len, 300);
        corto.validate().unwrap();
        let mut salida = Vec::new();
        s.get_file(&corto, &mut salida).await.unwrap();
        assert_eq!(salida, &original[..300]);

        // Extender: lo nuevo son ceros.
        let (largo, _) = s.truncate(&corto, 700).await.unwrap();
        assert_eq!(largo.len, 700);
        largo.validate().unwrap();
        let mut salida = Vec::new();
        s.get_file(&largo, &mut salida).await.unwrap();
        assert_eq!(&salida[..300], &original[..300]);
        assert_eq!(&salida[300..], &vec![0u8; 400][..]);
    }

    #[tokio::test]
    async fn truncar_a_cero_deja_un_fichero_vacio() {
        let mut v = volumen().await;
        v.write_at("/a.txt", 0, b"algo").await.unwrap();
        v.truncate("/a.txt", 0).await.unwrap();

        assert_eq!(v.lookup("/a.txt").await.unwrap().unwrap().len, 0);
        assert!(v.read_at("/a.txt", 0, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn crear_un_fichero_vacio() {
        let mut v = volumen().await;
        v.create_file("/vacio.txt").await.unwrap();

        assert_eq!(v.lookup("/vacio.txt").await.unwrap().unwrap().len, 0);
        assert!(v.create_file("/vacio.txt").await.is_err(), "no se pisa");
    }

    /// **El coste, medido.** Escribir dentro reescribe trozos enteros, y eso tiene que estar
    /// a la vista en vez de descubrirse en produccion.
    #[tokio::test]
    async fn escribir_dentro_reescribe_el_trozo_entero() {
        let s = store(rs());
        let original = datos(4 * TAM);
        let m = s.put_file_with(&original[..], TAM).await.unwrap().manifest;
        assert_eq!(m.chunks.len(), 4);

        // Un byte en el trozo 1.
        let (nuevo, _) = s.write_at(&m, TAM as u64 + 5, b"X").await.unwrap();

        assert_eq!(
            nuevo.chunks[0], m.chunks[0],
            "los trozos que no toca se reaprovechan tal cual"
        );
        assert_eq!(nuevo.chunks[2], m.chunks[2]);
        assert_eq!(nuevo.chunks[3], m.chunks[3]);
        assert_ne!(
            nuevo.chunks[1], m.chunks[1],
            "y el que toca es un trozo nuevo entero: 256 bytes subidos por 1 byte escrito"
        );
    }

    /// Extender por el final no puede reescribir lo anterior.
    #[tokio::test]
    async fn anadir_al_final_no_toca_los_trozos_llenos() {
        let s = store(rs());
        let m = s
            .put_file_with(&datos(2 * TAM)[..], TAM)
            .await
            .unwrap()
            .manifest;

        let (nuevo, _) = s.write_at(&m, (2 * TAM) as u64, b"cola").await.unwrap();

        assert_eq!(nuevo.chunks.len(), 3);
        assert_eq!(nuevo.chunks[0], m.chunks[0]);
        assert_eq!(nuevo.chunks[1], m.chunks[1]);
        assert_eq!(nuevo.chunks[2].plain_len, 4);
    }

    #[tokio::test]
    async fn no_se_puede_escribir_sobre_un_directorio() {
        let mut v = volumen().await;
        v.mkdir_p("/dir").await.unwrap();
        assert!(matches!(
            v.write_at("/dir", 0, b"x").await,
            Err(CoreError::NotAFile(_))
        ));
    }

    /// Escribir en un fichero vacio recien creado, que es el caso que hace un `open` con
    /// creacion seguido de un `write`.
    #[tokio::test]
    async fn crear_y_luego_escribir() {
        let mut v = volumen().await;
        v.create_file("/a.bin").await.unwrap();
        v.write_at("/a.bin", 0, &datos(3 * TAM)).await.unwrap();

        let leido = v.read_at("/a.bin", 0, 10_000).await.unwrap();
        assert_eq!(leido, datos(3 * TAM));
    }

    #[tokio::test]
    async fn un_manifiesto_vacio_admite_escritura_en_cualquier_offset() {
        let s = store(rs());
        let vacio = Manifest::empty(TAM);
        let (nuevo, _) = s.write_at(&vacio, 500, b"lejos").await.unwrap();

        assert_eq!(nuevo.len, 505);
        nuevo.validate().unwrap();
        let mut salida = Vec::new();
        s.get_file(&nuevo, &mut salida).await.unwrap();
        assert_eq!(&salida[500..], b"lejos");
        assert_eq!(&salida[..500], &vec![0u8; 500][..]);
    }
}

#[cfg(test)]
mod tests_rename {
    use libredisk_raid::Layout;

    use super::*;
    use crate::testutil::{backends, store_sobre};

    fn rs() -> Layout {
        Layout::ReedSolomon { k: 4, m: 2 }
    }

    async fn volumen() -> Volume {
        let bs = backends(6);
        Volume::create(store_sobre(&bs, rs())).await.unwrap().0
    }

    #[tokio::test]
    async fn renombrar_en_el_mismo_directorio() {
        let mut v = volumen().await;
        v.write_file("/viejo.txt", &b"contenido"[..]).await.unwrap();

        v.rename("/viejo.txt", "/nuevo.txt", false).await.unwrap();

        assert!(v.lookup("/viejo.txt").await.unwrap().is_none());
        assert_eq!(v.read_at("/nuevo.txt", 0, 50).await.unwrap(), b"contenido");
    }

    #[tokio::test]
    async fn mover_a_otro_directorio() {
        let mut v = volumen().await;
        v.write_file("/a/uno.txt", &b"datos"[..]).await.unwrap();
        v.mkdir_p("/b").await.unwrap();

        v.rename("/a/uno.txt", "/b/dos.txt", false).await.unwrap();

        assert!(v.read_dir("/a").await.unwrap().is_empty());
        assert_eq!(v.read_at("/b/dos.txt", 0, 50).await.unwrap(), b"datos");
    }

    /// **La razon de que `aplicar_varios` exista.** Los dos cambios tienen que salir en una
    /// sola raiz: si fueran dos, habria una generacion intermedia en la que el fichero no
    /// esta en ninguno de los dos sitios.
    #[tokio::test]
    async fn mover_gasta_una_sola_generacion() {
        let mut v = volumen().await;
        v.write_file("/a/uno.txt", &b"datos"[..]).await.unwrap();
        v.mkdir_p("/b").await.unwrap();

        let antes = v.root().generation;
        v.rename("/a/uno.txt", "/b/uno.txt", false).await.unwrap();

        assert_eq!(
            v.root().generation,
            antes + 1,
            "mover no puede publicar dos raices"
        );
    }

    /// Y visto desde fuera: ningun lector puede pillar el estado intermedio, porque no
    /// existe. Se comprueba que la raiz anterior tiene el fichero en el origen y la nueva en
    /// el destino, sin nada en medio.
    #[tokio::test]
    async fn no_hay_estado_intermedio_visible() {
        let bs = backends(6);
        let mut v = Volume::create(store_sobre(&bs, rs())).await.unwrap().0;
        v.write_file("/a/uno.txt", &b"datos"[..]).await.unwrap();
        v.mkdir_p("/b").await.unwrap();
        let raiz_antes = v.root().clone();

        v.rename("/a/uno.txt", "/b/uno.txt", false).await.unwrap();

        // La raiz vieja: el fichero sigue en el origen.
        let viejo = Volume::at(store_sobre(&bs, rs()), raiz_antes);
        assert!(viejo.lookup("/a/uno.txt").await.unwrap().is_some());
        assert!(viejo.lookup("/b/uno.txt").await.unwrap().is_none());

        // La nueva: esta en el destino. No hay ninguna en la que falte en las dos.
        assert!(v.lookup("/a/uno.txt").await.unwrap().is_none());
        assert!(v.lookup("/b/uno.txt").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn mover_un_directorio_se_lleva_su_contenido() {
        let mut v = volumen().await;
        v.write_file("/origen/hondo/f.txt", &b"x"[..])
            .await
            .unwrap();
        v.mkdir_p("/destino").await.unwrap();

        v.rename("/origen/hondo", "/destino/movido", false)
            .await
            .unwrap();

        assert_eq!(
            v.read_at("/destino/movido/f.txt", 0, 10).await.unwrap(),
            b"x"
        );
        assert!(v.read_dir("/origen").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_se_pisa_el_destino_sin_permiso() {
        let mut v = volumen().await;
        v.write_file("/a.txt", &b"uno"[..]).await.unwrap();
        v.write_file("/b.txt", &b"dos"[..]).await.unwrap();

        assert!(v.rename("/a.txt", "/b.txt", false).await.is_err());
        assert_eq!(v.read_at("/b.txt", 0, 10).await.unwrap(), b"dos");

        // Con permiso, si.
        v.rename("/a.txt", "/b.txt", true).await.unwrap();
        assert_eq!(v.read_at("/b.txt", 0, 10).await.unwrap(), b"uno");
        assert!(v.lookup("/a.txt").await.unwrap().is_none());
    }

    /// Meter un directorio dentro de si mismo dejaria una rama inalcanzable y un ciclo.
    #[tokio::test]
    async fn no_se_puede_mover_un_directorio_dentro_de_si_mismo() {
        let mut v = volumen().await;
        v.mkdir_p("/dir/sub").await.unwrap();

        assert!(matches!(
            v.rename("/dir", "/dir/sub/dir", false).await,
            Err(CoreError::InvalidPath(_))
        ));
        // Y el arbol sigue intacto.
        assert!(v.lookup("/dir/sub").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn mover_lo_que_no_existe_falla() {
        let mut v = volumen().await;
        assert!(matches!(
            v.rename("/fantasma.txt", "/otro.txt", false).await,
            Err(CoreError::PathNotFound(_))
        ));
    }

    #[tokio::test]
    async fn mover_a_la_misma_ruta_no_hace_nada() {
        let mut v = volumen().await;
        v.write_file("/a.txt", &b"x"[..]).await.unwrap();
        let antes = v.root().generation;

        v.rename("/a.txt", "/a.txt", false).await.unwrap();

        assert_eq!(v.root().generation, antes, "no publica una raiz para nada");
        assert!(v.lookup("/a.txt").await.unwrap().is_some());
    }

    /// Mover a un directorio que no existe tiene que fallar, no crearlo.
    ///
    /// `write_file` si crea los directorios que faltan, pero en un `rename` eso esconde
    /// errores: fue asi como una ruta llegada en mayusculas creo un directorio duplicado en
    /// vez de dar error.
    #[tokio::test]
    async fn mover_a_un_directorio_inexistente_falla() {
        let mut v = volumen().await;
        v.write_file("/a.txt", &b"x"[..]).await.unwrap();

        assert!(matches!(
            v.rename("/a.txt", "/nuevo/hondo/a.txt", false).await,
            Err(CoreError::PathNotFound(_))
        ));

        // Y no se ha tocado nada.
        assert_eq!(v.read_at("/a.txt", 0, 10).await.unwrap(), b"x");
        assert!(v.lookup("/nuevo").await.unwrap().is_none());
    }
}
