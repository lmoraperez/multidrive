//! Las ordenes que operan sobre un volumen de verdad.
//!
//! Los proveedores salen del entorno, igual que en `check`. La frase de paso sale de
//! `LIBREDISK_PASSPHRASE` y nunca de un argumento: lo que se escribe en la linea de ordenes
//! acaba en el historial del intérprete y en la lista de procesos.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use libredisk_backend::{Backend, live};
use libredisk_core::ChunkId;
use libredisk_core::diskcache::DiskCache;
use libredisk_core::header::{self, HEADER_KEY, HEADER_VERSION, Header};
use libredisk_core::rebalance::{self, Cambio};
use libredisk_core::rescue;
use libredisk_core::tree::{self, Kind, Volume};
use libredisk_core::writeback::{self, WriteQueue};
use libredisk_core::{ChunkStore, CoreError};
use libredisk_crypto::MasterKey;
use libredisk_raid::Layout;

const FRASE: &str = "LIBREDISK_PASSPHRASE";

/// La frase que trajo el lanzador de WinFsp, si vino por ahi.
///
/// Un servicio no tiene entorno que exportar: la frase llega por la entrada estandar y hay
/// que guardarla en algun sitio hasta que se derive la clave. Se guarda aqui y no en una
/// variable de entorno **a proposito**: el entorno de un proceso lo puede leer cualquiera que
/// pueda abrirlo, y esto no.
static FRASE_DEL_LANZADOR: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Fija la frase para el resto del proceso. Solo la usa el arranque desde el lanzador.
pub fn fijar_frase(f: String) {
    let _ = FRASE_DEL_LANZADOR.set(f);
}

fn frase() -> Result<String> {
    if let Some(f) = FRASE_DEL_LANZADOR.get() {
        return Ok(f.clone());
    }
    match std::env::var(FRASE) {
        Ok(f) if !f.trim().is_empty() => Ok(f),
        _ => bail!(
            "falta la frase de paso.\n\n  export {FRASE}='...'\n\n\
             No hay recuperacion: si la pierdes, pierdes el volumen."
        ),
    }
}

fn proveedores() -> Result<Vec<Arc<dyn Backend>>> {
    let configurados = live::configurados()
        .with_context(|| format!("configuracion de proveedores invalida\n\n{}", live::AYUDA))?;
    if configurados.is_empty() {
        bail!("no hay ningun proveedor configurado.\n\n{}", live::AYUDA);
    }
    Ok(configurados
        .into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect())
}

/// Construye el almacen a partir de la cabecera, **sin** abrir el volumen.
///
/// El rescate lo necesita asi: si el puntero de raiz se ha perdido, no hay volumen que
/// abrir, pero los trozos siguen ahi y se pueden leer igual.
/// Cache por defecto. Modesta: en una orden suelta solo ayuda a lo que recorre el arbol,
/// como `scrub` o `rescue`. El montaje pide mucha mas.
const CACHE_POR_DEFECTO: usize = 64 * 1024 * 1024;

/// Directorio por defecto de la cache en disco, **separado por volumen**.
///
/// La separacion no es cosmetica: dos volumenes pueden contener un trozo con el mismo
/// identificador —mismo contenido en claro— pero sellado con claves distintas. Si
/// compartieran directorio, cada uno veria el fichero del otro como corrupto y lo borraria,
/// turnandose para invalidarse mutuamente.
fn dir_base() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("libredisk")
}

fn dir_cache(salt: &[u8]) -> PathBuf {
    let base = dir_base();
    let etiqueta = &blake3_hex(salt)[..16];
    base.join("cache").join(etiqueta)
}

/// La sal no es secreta, pero su hash es un nombre de carpeta mas corto y sin sorpresas.
fn blake3_hex(datos: &[u8]) -> String {
    use libredisk_core::ChunkId;
    ChunkId::of(datos).as_str().to_string()
}

async fn almacen(cache_bytes: usize) -> Result<ChunkStore> {
    almacen_completo(cache_bytes, 0).await
}

/// Igual, anadiendo la cache en disco si se pide.
async fn almacen_completo(cache_bytes: usize, disco_bytes: usize) -> Result<ChunkStore> {
    almacen_todo(cache_bytes, disco_bytes, false).await
}

/// Un almacen sobre unos proveedores concretos, sin volver a leer la configuracion.
///
/// El rebalanceo lo necesita: tiene que leer con **los proveedores que el volumen ya tenia**,
/// no con todos los que hay configurados, porque los nuevos todavia no forman parte de la
/// topologia y `check_backends` los rechazaria con razon.
async fn almacen_con(
    backends: &[Arc<dyn Backend>],
    header: &Header,
    cache_bytes: usize,
) -> Result<ChunkStore> {
    header.check_backends(backends)?;
    let layout: Layout = header.layout.parse().with_context(|| {
        format!(
            "la cabecera declara una topologia invalida: {}",
            header.layout
        )
    })?;
    let key = MasterKey::derive(&frase()?, &header.salt)?;
    Ok(ChunkStore::new(layout, backends.to_vec(), key)?.with_cache(cache_bytes))
}

/// Igual, pudiendo activar la escritura diferida.
async fn almacen_todo(
    cache_bytes: usize,
    disco_bytes: usize,
    diferido: bool,
) -> Result<ChunkStore> {
    let backends = proveedores()?;

    let header = header::read(&backends)
        .await?
        .ok_or(CoreError::NoVolume)
        .context("¿has ejecutado `libredisk volume create`?")?;
    header.check_backends(&backends)?;

    let layout: Layout = header.layout.parse().with_context(|| {
        format!(
            "la cabecera declara una topologia invalida: {}",
            header.layout
        )
    })?;
    let key = MasterKey::derive(&frase()?, &header.salt)?;
    let mut store = ChunkStore::new(layout, backends, key)?.with_cache(cache_bytes);

    if disco_bytes > 0 {
        let dir = dir_cache(&header.salt);
        let disco = DiskCache::open(&dir, disco_bytes)
            .await
            .with_context(|| format!("no se pudo abrir la cache en '{}'", dir.display()))?;
        store = store.with_disk_cache(disco);
    }

    if diferido {
        let base = dir_base();
        let dir = writeback::dir_de_volumen(&base, &header.salt);
        let cola = WriteQueue::open(&dir)
            .await
            .with_context(|| format!("no se pudo abrir la cola en '{}'", dir.display()))?;
        store = store.with_writeback(std::sync::Arc::new(cola));
    }
    Ok(store)
}

/// Abre el volumen que haya en los proveedores del entorno.
pub async fn abrir() -> Result<Volume> {
    abrir_con_cache(CACHE_POR_DEFECTO).await
}

/// Igual, con una cache del tamano que se pida. El montaje la quiere grande.
pub async fn abrir_con_cache(cache_bytes: usize) -> Result<Volume> {
    abrir_completo(cache_bytes, 0).await
}

/// Con los dos escalones de cache: memoria y disco.
pub async fn abrir_completo(cache_bytes: usize, disco_bytes: usize) -> Result<Volume> {
    match Volume::open(almacen_completo(cache_bytes, disco_bytes).await?).await {
        Ok(v) => Ok(v),
        // Una frase de paso equivocada no da un error de descifrado: da un puntero de raiz
        // que no se abre, y por tanto "no hay raiz". Hay que decirlo, o parece corrupcion.
        Err(CoreError::NoRoot) => bail!(
            "hay una cabecera de volumen pero no se pudo leer ninguna raiz.\n\
             Lo mas probable es que {FRASE} no sea la frase con la que se creo."
        ),
        Err(e) => Err(e.into()),
    }
}

/// Crea un volumen sobre los proveedores del entorno.
pub async fn create(spec: &str) -> Result<()> {
    let backends = proveedores()?;
    let layout: Layout = spec
        .parse()
        .with_context(|| format!("topologia invalida: {spec}"))?;

    if backends.len() != layout.shard_count() {
        bail!(
            "la topologia {layout} necesita {} proveedores y hay {} configurados",
            layout.shard_count(),
            backends.len()
        );
    }

    // No se sobrescribe jamas una cabecera. Si una creacion quedo a medias, borrar el objeto
    // a mano es una decision del usuario: generar una sal nueva encima dejaria inalcanzable
    // todo lo que hubiera escrito antes.
    if header::read(&backends).await?.is_some() {
        bail!(
            "ya hay un volumen en estos proveedores.\n\
             Si una creacion anterior quedo a medias, borra '{HEADER_KEY}' de cada proveedor \
             a mano antes de reintentar."
        );
    }

    let salt = libredisk_crypto::generate_salt();
    let header = Header {
        version: HEADER_VERSION,
        layout: spec.to_string(),
        salt: salt.to_vec(),
        backends: backends.iter().map(|b| b.name().to_string()).collect(),
    };

    // La cabecera va primero: sin ella, una raiz publicada seria ilegible para siempre,
    // porque nadie podria volver a derivar la clave.
    header::write(&backends, &header).await?;

    let key = MasterKey::derive(&frase()?, &salt)?;
    let store = ChunkStore::new(layout, backends, key)?;
    let (volumen, degraded) = Volume::create(store).await?;

    println!("volumen creado");
    println!("  topologia     {layout}");
    println!(
        "  proveedores   {}",
        volumen.store().backend_names().join(", ")
    );
    println!(
        "  tolera perder {} proveedor(es)",
        layout.tolerated_failures()
    );
    println!();
    println!("La frase de paso no se guarda en ninguna parte. Si la pierdes, se pierde todo.");
    avisar(&degraded);
    Ok(())
}

/// Lista un directorio.
pub async fn ls(path: &str) -> Result<()> {
    let volumen = abrir().await?;
    let entradas = volumen.read_dir(path).await?;

    if entradas.is_empty() {
        println!("(vacio)");
        return Ok(());
    }
    for (nombre, entrada) in entradas {
        match entrada.kind {
            Kind::Dir => println!("  {:<40} {:>12}", format!("{nombre}/"), "-"),
            Kind::File => println!("  {nombre:<40} {:>12}", tamano(entrada.len)),
        }
    }
    Ok(())
}

/// Sube un fichero local.
pub async fn put(local: &str, remoto: &str) -> Result<()> {
    let mut volumen = abrir().await?;
    let fichero = tokio::fs::File::open(local)
        .await
        .with_context(|| format!("no se pudo abrir '{local}'"))?;
    let bytes = fichero.metadata().await.map(|m| m.len()).unwrap_or(0);

    let degraded = volumen.write_file(remoto, fichero).await?;
    println!("subido {remoto} ({})", tamano(bytes));
    avisar(&degraded);
    Ok(())
}

/// Baja un fichero del volumen.
pub async fn get(remoto: &str, local: Option<&str>) -> Result<()> {
    let volumen = abrir().await?;

    // Sin destino, al nombre del fichero en el directorio actual.
    let destino = match local {
        Some(l) => l.to_string(),
        None => remoto.rsplit('/').next().unwrap_or(remoto).to_string(),
    };

    let mut fichero = tokio::fs::File::create(&destino)
        .await
        .with_context(|| format!("no se pudo crear '{destino}'"))?;
    volumen.read_file(remoto, &mut fichero).await?;

    println!("bajado {remoto} -> {destino}");
    Ok(())
}

/// Lee un trozo suelto de un fichero, sin bajarlo entero.
///
/// Escribe los bytes tal cual, sin interpretarlos como texto: un fragmento de un fichero
/// cifrado o de un binario no es UTF-8, y convertirlo lo destruiria en silencio.
pub async fn cat(remoto: &str, offset: u64, len: usize) -> Result<()> {
    use std::io::Write;

    let volumen = abrir().await?;
    let datos = volumen.read_at(remoto, offset, len).await?;
    std::io::stdout().write_all(&datos)?;
    Ok(())
}

pub async fn mkdir(path: &str) -> Result<()> {
    let degraded = abrir().await?.mkdir_p(path).await?;
    avisar(&degraded);
    Ok(())
}

pub async fn rm(path: &str) -> Result<()> {
    let degraded = abrir().await?.remove(path).await?;
    avisar(&degraded);
    Ok(())
}

/// Estado del volumen.
pub async fn status() -> Result<()> {
    let backends = proveedores()?;
    let header = header::read(&backends)
        .await?
        .ok_or(CoreError::NoVolume)
        .context("¿has ejecutado `libredisk volume create`?")?;

    println!("topologia     {}", header.layout);
    println!("proveedores   {}", header.backends.join(", "));
    println!("formato       version {}", header.version);

    // Lo pendiente no depende de poder abrir el arbol: se lee del diario local.
    let base = dir_base();
    let dir = writeback::dir_de_volumen(&base, &header.salt);
    if let Ok(cola) = WriteQueue::open(&dir).await {
        let p = cola.pendiente();
        if p.a_salvo() {
            println!("pendiente     nada: todo esta en los proveedores");
        } else {
            println!(
                "pendiente     {} trozo(s), {} SOLO EN ESTA MAQUINA",
                p.trozos,
                tamano(p.bytes)
            );
        }
    }

    // Abrir el arbol necesita la frase de paso; lo de arriba no. Si no esta, se dice lo que
    // se sabe en vez de fallar entero.
    match abrir().await {
        Ok(v) => {
            let raiz = v.root();
            println!("generacion    {}", raiz.generation);
            println!("raiz          {}", raiz.node.id);
            println!("entradas      {}", v.read_dir("/").await?.len());
        }
        Err(e) => {
            println!();
            println!("No se pudo abrir el arbol: {e}");
        }
    }
    Ok(())
}

fn tamano(bytes: u64) -> String {
    const UNIDADES: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut valor = bytes as f64;
    let mut i = 0;
    while valor >= 1024.0 && i < UNIDADES.len() - 1 {
        valor /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{valor:.1} {}", UNIDADES[i])
    }
}

/// Avisa de los proveedores que no aceptaron su fragmento.
///
/// Callarselo seria lo peor que puede hacer este programa: el usuario creeria que tiene la
/// redundancia que pidio, y se enteraria el dia que pierda un proveedor.
fn avisar(degraded: &[String]) {
    if degraded.is_empty() {
        return;
    }
    println!();
    println!(
        "AVISO: {} proveedor(es) no aceptaron algun fragmento: {}",
        degraded.len(),
        degraded.join(", ")
    );
    println!("El volumen tiene menos redundancia de la pedida. Ejecuta `libredisk rebuild`.");
}

/// Revisa la salud de todos los trozos del volumen.
pub async fn scrub() -> Result<()> {
    let volumen = abrir().await?;
    let nombres: Vec<String> = volumen
        .store()
        .backend_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let (chunks, ilegibles) = volumen.walk().await;
    println!("trozos          {}", chunks.len());
    for (ruta, motivo) in &ilegibles {
        println!("  ILEGIBLE      {ruta}: {motivo}");
    }

    let informe = volumen.store().scrub_many(&chunks).await;
    for (i, motivo) in &informe.unlistable {
        println!(
            "  AVISO         no se pudo enumerar '{}': {motivo}",
            nombres[*i]
        );
    }

    let (mut completos, mut degradados, mut perdidos) = (0, 0, 0);
    for salud in &informe.health {
        if salud.missing.is_empty() {
            completos += 1;
        } else {
            let faltan: Vec<&str> = salud.missing.iter().map(|i| nombres[*i].as_str()).collect();
            if salud.is_readable() {
                degradados += 1;
                println!(
                    "  DEGRADADO     {} le falta en: {}",
                    salud.id,
                    faltan.join(", ")
                );
            } else {
                perdidos += 1;
                println!(
                    "  PERDIDO       {} le falta en: {}",
                    salud.id,
                    faltan.join(", ")
                );
            }
        }
    }

    println!();
    println!("completos       {completos}");
    println!("degradados      {degradados}   (se leen, pero sin la redundancia pedida)");
    println!("perdidos        {perdidos}   (ya no se pueden leer)");

    if !informe.is_complete() {
        println!();
        println!("Las cifras son un suelo pesimista: algun proveedor no se dejo enumerar.");
    }

    // Un nodo ilegible no es solo un trozo malo: se lleva por delante todo lo que colgaba
    // de el, que no llega ni a contarse. Decir "0 perdidos" despues de eso seria mentir.
    if !ilegibles.is_empty() {
        println!();
        println!(
            "El recorrido quedo INCOMPLETO: {} nodo(s) no se pudieron leer, y nada de lo que",
            ilegibles.len()
        );
        println!("colgaba de ellos se ha revisado. Las cifras de arriba no cubren el volumen.");
    }

    if perdidos > 0 {
        anyhow::bail!("hay {perdidos} trozo(s) que ya no se pueden reconstruir");
    }
    if !ilegibles.is_empty() {
        anyhow::bail!("el scrub no pudo recorrer el volumen entero");
    }
    Ok(())
}

/// Regenera los fragmentos que falten, donde todavia se pueda.
pub async fn rebuild() -> Result<()> {
    let volumen = abrir().await?;
    let (chunks, _) = volumen.walk().await;
    let informe = volumen.store().scrub_many(&chunks).await;

    let (mut reparados, mut irreparables) = (0, 0);
    for (salud, chunk) in informe.health.iter().zip(&chunks) {
        if salud.missing.is_empty() {
            continue;
        }
        if !salud.is_readable() {
            irreparables += 1;
            println!("  IRREPARABLE   {}", salud.id);
            continue;
        }
        let vueltos = volumen.store().repair(chunk).await?;
        if !vueltos.is_empty() {
            reparados += 1;
            println!(
                "  reparado      {} ({} fragmento(s))",
                salud.id,
                vueltos.len()
            );
        }
    }

    println!();
    println!("trozos reparados  {reparados}");
    if irreparables > 0 {
        anyhow::bail!("{irreparables} trozo(s) ya no tienen fragmentos suficientes");
    }
    Ok(())
}

/// Reconstruye el volumen a partir de lo que quede en los proveedores.
pub async fn rescue(adoptar: Option<&str>) -> Result<()> {
    let store = almacen(CACHE_POR_DEFECTO).await?;
    let publicada = tree::raiz_publicada(&store).await?;

    let inv = store.rescue().await?;

    println!("directorios     {}", inv.directorios.len());
    println!("ficheros        {}", inv.ficheros.len());
    println!("trozos de datos {}", inv.datos.len());
    if !inv.huerfanos.is_empty() {
        println!(
            "huerfanos       {}   (no cuelgan de ningun fichero encontrado)",
            inv.huerfanos.len()
        );
    }
    if !inv.ajenos.is_empty() {
        println!(
            "ajenos          {}   (no descifran: otro volumen, u otra frase)",
            inv.ajenos.len()
        );
    }
    for (id, motivo) in &inv.ilegibles {
        println!("  ILEGIBLE      {id}: {motivo}");
    }
    for (i, motivo) in &inv.sin_enumerar {
        println!("  AVISO         el proveedor {i} no se dejo enumerar: {motivo}");
    }

    if let Some(id) = adoptar {
        return adoptar_raiz(&store, id, publicada.as_ref()).await;
    }

    println!();
    if inv.raices.is_empty() {
        println!("No se encontro ninguna raiz candidata.");
        if !inv.is_complete() {
            println!("Pero el inventario esta incompleto: arregla el acceso y repite.");
        }
        return Ok(());
    }

    println!("raices candidatas ({}):", inv.raices.len());
    for id in &inv.raices {
        let r = inv.resumen(id);
        let actual = match &publicada {
            Some(p) if p.node.id == *id => "  <- la publicada ahora",
            _ => "",
        };
        println!();
        println!("  {id}{actual}");
        println!(
            "    {} fichero(s), {} en {} directorio(s){}",
            r.ficheros,
            tamano(r.bytes),
            r.directorios,
            if r.rotos > 0 {
                format!(", {} nodo(s) ROTOS", r.rotos)
            } else {
                String::new()
            }
        );
        for ruta in inv.rutas(id, 6) {
            println!("    {ruta}");
        }
        if r.ficheros > 6 {
            println!("    ... y {} mas", r.ficheros - 6);
        }
    }

    println!();
    println!("Para volver a una de ellas:");
    println!("  libredisk rescue --adoptar <id>");
    println!();
    println!("Adoptar no borra nada: las demas raices siguen ahi y se puede volver.");
    if !inv.is_complete() {
        println!();
        println!("AVISO: algun proveedor no se dejo enumerar, asi que falta por mirar.");
    }
    Ok(())
}

async fn adoptar_raiz(
    store: &ChunkStore,
    id: &str,
    publicada: Option<&libredisk_core::tree::Root>,
) -> Result<()> {
    let id = ChunkId::parse(id)
        .ok_or_else(|| anyhow::anyhow!("'{id}' no tiene forma de identificador de trozo"))?;

    let (chunk, _) = store
        .recover_blind(&id)
        .await
        .with_context(|| format!("no se pudo recuperar el trozo {id}"))?;

    // Por encima de la que hubiera, para que cualquier lector prefiera esta.
    let generacion = publicada.map(|r| r.generation + 1).unwrap_or(1);
    rescue::adoptar(store, chunk, generacion).await?;

    println!();
    println!("raiz adoptada: {id}");
    println!("generacion:    {generacion}");
    Ok(())
}

/// Vuelca la cabecera a un fichero local.
///
/// Es el respaldo que de verdad importa. La cabecera lleva la sal, y sin sal no hay clave
/// con la que descifrar nada: si se perdiera de todos los proveedores a la vez, los trozos
/// seguirian ahi y no habria forma de leerlos. Guardala junto a la frase de paso.
pub async fn header_export(destino: &str) -> Result<()> {
    let backends = proveedores()?;
    let header = header::read(&backends)
        .await?
        .ok_or(CoreError::NoVolume)
        .context("no hay ningun volumen del que exportar la cabecera")?;

    let json = serde_json::to_vec_pretty(&header)?;
    tokio::fs::write(destino, &json)
        .await
        .with_context(|| format!("no se pudo escribir '{destino}'"))?;

    println!("cabecera exportada a {destino}");
    println!();
    println!("No es un secreto —la sal no lo es— pero sin ella el volumen es ilegible.");
    println!("Guardala donde guardes la frase de paso, no en la misma maquina del volumen.");
    Ok(())
}

/// Restaura una cabecera en los proveedores desde un fichero local.
pub async fn header_import(origen: &str) -> Result<()> {
    let backends = proveedores()?;
    let bytes = tokio::fs::read(origen)
        .await
        .with_context(|| format!("no se pudo leer '{origen}'"))?;
    let nueva: header::Header = serde_json::from_slice(&bytes)
        .with_context(|| format!("'{origen}' no contiene una cabecera valida"))?;

    // Restaurar encima de una cabecera distinta mezclaria dos volumenes.
    if let Some(actual) = header::read(&backends).await? {
        if actual != nueva {
            bail!(
                "ya hay una cabecera distinta en estos proveedores.\n\
                 Restaurar encima mezclaria dos volumenes; borra la actual a mano si estas seguro."
            );
        }
        println!("la cabecera que ya habia es identica; no hay nada que cambiar");
        return Ok(());
    }

    nueva.check_backends(&backends)?;
    header::write(&backends, &nueva).await?;
    println!("cabecera restaurada en {} proveedor(es)", backends.len());
    Ok(())
}

/// Monta el volumen como una unidad del sistema.
///
/// Con `desde_lanzador`, la salida estandar **no es una consola**: es una tuberia por la que
/// el lanzador de WinFsp espera exactamente `OK` y que cierra justo despues. Escribir ahi mas
/// tarde acaba en una tuberia rota, y `println!` entra en panico cuando eso pasa, asi que el
/// montaje se moriria por el delito de contar como va. Todo lo informativo se desvia a la
/// salida de error, que el lanzador si redirige a un fichero.
pub async fn mount(
    punto: &str,
    cache_mb: usize,
    disco_mb: usize,
    escritura: bool,
    sync_cada: u64,
    desde_lanzador: bool,
) -> Result<()> {
    macro_rules! di {
        ($($t:tt)*) => {
            if desde_lanzador { eprintln!($($t)*) } else { println!($($t)*) }
        };
    }

    // Montar en escritura difiere siempre: cada `write` del sistema no puede ser una subida
    // sincrona a varias nubes. Lo pendiente lo sube el sincronizador de fondo.
    let store = almacen_todo(cache_mb * 1024 * 1024, disco_mb * 1024 * 1024, escritura).await?;
    // Una copia del almacen para el sincronizador: comparte cola y caches con el volumen,
    // pero no pasa por su cerrojo, asi que subir no bloquea las escrituras.
    let para_sync = store.clone();
    // Se traduce el error igual que en `abrir_completo`: una frase equivocada no da un fallo
    // de descifrado, da un puntero de raiz que no se abre. Bajo el lanzador esto importa mas
    // que en ningun otro sitio, porque el registro de errores es la unica pista que queda.
    let v = match Volume::open(store).await {
        Ok(v) => v,
        Err(CoreError::NoRoot) => bail!(
            "hay una cabecera de volumen pero no se pudo leer ninguna raiz.
\n             Lo mas probable es que la frase de paso no sea la que se uso al crearlo."
        ),
        Err(e) => return Err(e.into()),
    };
    let volumen = Arc::new(std::sync::RwLock::new(v));
    let montaje = libredisk_mount::montar(Arc::clone(&volumen), punto, !escritura)?;

    // El `OK` va despues de montar, no despues de abrir el volumen. La frase podria ser
    // buena y la letra de unidad estar ocupada: decir que si y morirse a continuacion le
    // dejaria a Windows una unidad que no existe.
    if desde_lanzador {
        crate::servicio::confirmar_al_lanzador()?;
    }

    let sincronizador = escritura.then(|| {
        Sincronizador::arrancar(
            para_sync,
            std::time::Duration::from_secs(sync_cada),
            desde_lanzador,
        )
    });

    di!("montado en {punto}");
    // Quien pone el controlador que hace aparecer la unidad es WinFsp, y su excepcion FLOSS
    // pide este aviso y este enlace en la interfaz. Va aqui porque es justo el momento en que
    // se esta usando, no escondido en un `--version` que nadie ejecuta.
    #[cfg(windows)]
    di!("controlador   WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos");
    #[cfg(windows)]
    di!("              https://github.com/winfsp/winfsp");
    di!(
        "modo          {}",
        if escritura {
            "lectura y escritura (diferida)"
        } else {
            "solo lectura"
        }
    );
    di!("cache memoria {cache_mb} MiB");
    if disco_mb > 0 {
        di!("cache disco   {disco_mb} MiB (cifrada, sobrevive al reinicio)");
    }
    di!();
    if escritura {
        di!("Lo que escribas queda SOLO EN ESTA MAQUINA hasta que suba, cada {sync_cada} s.");
        di!("Al desmontar se sube lo que quede antes de salir.");
    } else {
        di!("La primera lectura de cada trozo va a la nube; las siguientes no.");
    }
    di!("Ctrl-C para desmontar.");

    tokio::signal::ctrl_c().await?;
    drop(montaje);
    drop(sincronizador);

    di!();
    di!("desmontado");

    // Una ultima subida antes de salir: dejar cosas a medias justo al desmontar seria la
    // peor forma de perderlas, porque el usuario cree que ha terminado.
    if escritura {
        let queda = volumen
            .read()
            .expect("el montaje ya termino")
            .store()
            .pendiente();
        if !queda.a_salvo() {
            di!();
            di!(
                "subiendo lo que queda: {} trozo(s), {}...",
                queda.trozos,
                tamano(queda.bytes)
            );
            let store = volumen
                .read()
                .expect("el montaje ya termino")
                .store()
                .clone();
            match store.sync().await {
                Ok(i) if i.todo_arriba() => di!("todo a salvo en los proveedores"),
                Ok(i) => di!(
                    "AVISO: quedan {} trozo(s) sin subir. Ejecuta `libredisk sync`.",
                    i.fallidos.len()
                ),
                Err(e) => di!("AVISO: no se pudo subir: {e}. Ejecuta `libredisk sync`."),
            }
        }
    }
    let volumen = volumen.read().expect("el montaje ya termino");
    if let Some(s) = volumen.store().cache_stats() {
        let tasa = s
            .tasa_de_acierto()
            .map(|t| format!("{:.0}%", t * 100.0))
            .unwrap_or_else(|| "-".into());
        di!();
        di!(
            "cache: {} aciertos, {} fallos ({tasa})",
            s.aciertos,
            s.fallos
        );
        di!(
            "       {} trozos, {}, {} desalojos",
            s.entradas,
            tamano(s.bytes as u64),
            s.desalojos
        );
    }
    if let Some(s) = volumen.store().disk_stats() {
        di!(
            "disco: {} aciertos, {} fallos, {} trozos, {}",
            s.aciertos,
            s.fallos,
            s.entradas,
            tamano(s.bytes as u64)
        );
        if s.corruptos > 0 {
            di!(
                "       {} entradas descartadas por no cuadrar con su hash",
                s.corruptos
            );
        }
    }
    let p = volumen.store().pendiente();
    if !p.a_salvo() {
        di!();
        di!(
            "PENDIENTE: {} trozo(s), {} SOLO EN ESTA MAQUINA. Ejecuta `libredisk sync`.",
            p.trozos,
            tamano(p.bytes)
        );
    }
    Ok(())
}

/// Sube un fichero local difiriendo la subida: queda en esta maquina hasta `sync`.
pub async fn put_diferido(local: &str, remoto: &str) -> Result<()> {
    let store = almacen_todo(CACHE_POR_DEFECTO, 0, true).await?;
    let mut volumen = Volume::open(store).await?;

    let fichero = tokio::fs::File::open(local)
        .await
        .with_context(|| format!("no se pudo abrir '{local}'"))?;
    let bytes = fichero.metadata().await.map(|m| m.len()).unwrap_or(0);

    volumen.write_file(remoto, fichero).await?;
    let p = volumen.store().pendiente();

    println!("preparado {remoto} ({})", tamano(bytes));
    println!();
    println!("GUARDADO SOLO EN ESTA MAQUINA. Todavia no esta en ningun proveedor:");
    println!(
        "  {} trozo(s), {} pendientes de subir",
        p.trozos,
        tamano(p.bytes)
    );
    println!("  Si esta maquina se rompe ahora, se pierde.");
    println!();
    println!("  libredisk sync");
    Ok(())
}

/// Sube lo que haya quedado pendiente.
pub async fn sync() -> Result<()> {
    let store = almacen_todo(CACHE_POR_DEFECTO, 0, true).await?;

    let antes = store.pendiente();
    if antes.a_salvo() {
        println!("no hay nada pendiente: todo esta en los proveedores");
        return Ok(());
    }
    println!(
        "subiendo {} trozo(s), {}...",
        antes.trozos,
        tamano(antes.bytes)
    );

    let informe = store.sync().await?;

    println!();
    println!(
        "subidos       {} ({})",
        informe.subidos,
        tamano(informe.bytes)
    );
    if informe.descartados > 0 {
        println!(
            "descartados   {} ({}) por no colgar ya de la raiz final",
            informe.descartados,
            tamano(informe.bytes_descartados)
        );
    }
    if informe.raiz_publicada {
        println!("raiz          publicada");
    }
    avisar(&informe.degraded);

    if !informe.fallidos.is_empty() {
        println!();
        for (id, motivo) in &informe.fallidos {
            println!("  FALLO  {id}: {motivo}");
        }
        println!();
        println!(
            "La raiz NO se ha publicado: el volumen sigue viendose como antes de estas\n\
             escrituras, que es un estado viejo pero integro. Arregla el acceso y repite\n\
             `libredisk sync`; lo pendiente sigue guardado aqui."
        );
        anyhow::bail!("{} trozo(s) sin subir", informe.fallidos.len());
    }

    let despues = store.pendiente();
    if despues.a_salvo() {
        println!();
        println!("Todo a salvo en los proveedores.");
    }
    Ok(())
}

/// Sincronizador de fondo para el montaje en escritura.
///
/// Trabaja sobre una **copia del almacen**, no sobre el volumen. Eso no es un rodeo: subir
/// puede tardar minutos, y si sostuviera el cerrojo del volumen durante ese rato bloquearia
/// todas las escrituras del sistema de ficheros. La copia comparte caches y cola con el
/// original, asi que ve lo mismo sin estorbar.
///
/// Corre en un hilo propio, fuera del runtime, por el mismo motivo que los metodos de WinFsp:
/// bloquea sobre el, y hacerlo desde un hilo del runtime seria un panico.
struct Sincronizador {
    parar: Arc<std::sync::atomic::AtomicBool>,
    hilo: Option<std::thread::JoinHandle<()>>,
}

impl Sincronizador {
    /// `a_stderr` va donde va el resto de la salida del montaje: bajo el lanzador de WinFsp
    /// la salida estandar es una tuberia cerrada, y escribir ahi mataria el hilo de subida.
    fn arrancar(store: ChunkStore, cada: std::time::Duration, a_stderr: bool) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};

        macro_rules! di {
            ($($t:tt)*) => {
                if a_stderr { eprintln!($($t)*) } else { println!($($t)*) }
            };
        }

        let parar = Arc::new(AtomicBool::new(false));
        let rt = tokio::runtime::Handle::current();

        let hilo = std::thread::spawn({
            let parar = Arc::clone(&parar);
            move || {
                while !parar.load(Ordering::Relaxed) {
                    // Se duerme a trocitos para poder parar enseguida al desmontar, en vez
                    // de tener que esperar a que se cumpla el intervalo entero.
                    let mut queda = cada;
                    let paso = std::time::Duration::from_millis(200);
                    while queda > std::time::Duration::ZERO && !parar.load(Ordering::Relaxed) {
                        let dormir = paso.min(queda);
                        std::thread::sleep(dormir);
                        queda -= dormir;
                    }
                    if parar.load(Ordering::Relaxed) {
                        return;
                    }

                    if store.pendiente().a_salvo() {
                        continue;
                    }
                    match rt.block_on(store.sync()) {
                        Ok(i) if i.subidos > 0 || i.descartados > 0 => {
                            di!(
                                "  [sync] {} subidos ({}), {} descartados{}",
                                i.subidos,
                                tamano(i.bytes),
                                i.descartados,
                                if i.raiz_publicada {
                                    ", raiz publicada"
                                } else {
                                    ""
                                }
                            );
                        }
                        Ok(_) => {}
                        // Un fallo no para el montaje: lo pendiente sigue en disco y el
                        // siguiente intento lo recoge. Callarselo si seria grave.
                        Err(e) => di!("  [sync] fallo, se reintentara: {e}"),
                    }
                }
            }
        });

        Self {
            parar,
            hilo: Some(hilo),
        }
    }
}

impl Drop for Sincronizador {
    fn drop(&mut self) {
        self.parar.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.hilo.take() {
            let _ = h.join();
        }
    }
}

/// Los proveedores del volumen frente a los que hay configurados.
///
/// Separarlos importa porque el orden es parte del formato: los que ya estan tienen que seguir
/// exactamente donde estaban, y los nuevos solo pueden ir detras.
struct Reparto {
    /// Los que la cabecera declara, en su orden.
    del_volumen: Vec<Arc<dyn Backend>>,
    /// Los configurados que la cabecera no menciona, en el orden en que se configuraron.
    nuevos: Vec<Arc<dyn Backend>>,
}

fn repartir(header: &Header, configurados: Vec<Arc<dyn Backend>>) -> Result<Reparto> {
    let nombres: Vec<&str> = configurados.iter().map(|b| b.name()).collect();

    // Los del volumen tienen que estar, y en la misma posicion. Si alguien reordena su
    // configuracion, cada fragmento se leeria del sitio equivocado.
    for (i, esperado) in header.backends.iter().enumerate() {
        match nombres.get(i) {
            Some(actual) if actual == esperado => {}
            Some(actual) => bail!(
                "en la posicion {i} el volumen tiene '{esperado}' y ahora hay '{actual}'.\n\
                 El orden es parte del formato: los proveedores que ya estaban no se pueden \
                 mover ni quitar, solo se pueden anadir otros detras."
            ),
            None => bail!(
                "falta el proveedor '{esperado}', que es el numero {i} del volumen.\n\
                 Para anadir hace falta tener configurados todos los que ya habia."
            ),
        }
    }

    let (del_volumen, nuevos) = configurados.split_at(header.backends.len());
    Ok(Reparto {
        del_volumen: del_volumen.to_vec(),
        nuevos: nuevos.to_vec(),
    })
}

/// Que proveedores tiene el volumen, y cuales hay configurados de mas.
pub async fn providers_list() -> Result<()> {
    let configurados = proveedores()?;
    let header = header::read(&configurados)
        .await?
        .ok_or(CoreError::NoVolume)
        .context("¿has ejecutado `libredisk volume create`?")?;
    let layout: Layout = header.layout.parse()?;

    println!("topologia     {layout}");
    println!(
        "tolera perder {} proveedor(es)",
        layout.shard_count() - layout.shards_needed()
    );
    println!();
    println!("Proveedores del volumen, en su orden:");
    for (i, n) in header.backends.iter().enumerate() {
        println!("  {i:>2}  {n}");
    }

    let extra: Vec<&str> = configurados
        .iter()
        .map(|b| b.name())
        .filter(|n| !header.backends.iter().any(|h| h == n))
        .collect();
    println!();
    if extra.is_empty() {
        println!("No hay ningun proveedor configurado de mas.");
        println!("Para anadir uno, configuralo y vuelve a ejecutar esto.");
    } else {
        println!("Configurados y todavia sin usar ({}):", extra.len());
        for n in &extra {
            println!("      {n}");
        }
        println!();
        println!("  libredisk providers add --mas-seguridad   # mas margen de caidas");
        println!("  libredisk providers add --mas-espacio     # mas capacidad (caro)");
    }
    Ok(())
}

/// Incorpora al volumen los proveedores configurados de mas.
pub async fn providers_add(cambio: Cambio) -> Result<()> {
    let configurados = proveedores()?;
    let header = header::read(&configurados)
        .await?
        .ok_or(CoreError::NoVolume)
        .context("¿has ejecutado `libredisk volume create`?")?;
    let antes: Layout = header.layout.parse()?;
    let reparto = repartir(&header, configurados)?;

    if reparto.nuevos.is_empty() {
        bail!(
            "no hay ningun proveedor configurado que el volumen no tenga ya.\n\
             Configura el nuevo y vuelve a intentarlo: `libredisk providers list`."
        );
    }
    let destino = rebalance::planear(antes, reparto.nuevos.len(), cambio)?;

    println!("topologia     {antes}  ->  {destino}");
    println!(
        "tolera perder {}  ->  {}",
        antes.shard_count() - antes.shards_needed(),
        destino.shard_count() - destino.shards_needed()
    );
    println!(
        "sobrecoste    {:.2}x  ->  {:.2}x",
        antes.shard_count() as f64 / antes.shards_needed() as f64,
        destino.shard_count() as f64 / destino.shards_needed() as f64
    );
    println!("anadiendo     {}", reparto.nuevos.len());
    for b in &reparto.nuevos {
        println!("              {}", b.name());
    }
    println!();

    if !rebalance::conserva_lo_escrito(antes, destino) {
        bail!(
            "pasar de {antes} a {destino} mueve todos los fragmentos de todos los trozos, asi \
             que hay que releer y volver a subir el volumen entero.\n\
             Eso todavia no esta implementado: hace falta un esquema de dos generaciones para \
             poder escribir lo nuevo sin destruir lo viejo mientras el volumen se sigue \
             leyendo.\n\n\
             Por ahora, `--mas-seguridad` si funciona: no mueve nada de lo que ya hay."
        );
    }

    // Se lee con la topologia vieja, que es la que describe lo que hay escrito.
    let store = almacen_con(&reparto.del_volumen, &header, CACHE_POR_DEFECTO).await?;
    let volumen = Volume::open(store).await?;

    let (trozos, ilegibles) = volumen.walk().await;
    if !ilegibles.is_empty() {
        println!("AVISO: {} ruta(s) ilegibles, se omiten:", ilegibles.len());
        for (ruta, e) in ilegibles.iter().take(5) {
            println!("  {ruta}: {e}");
        }
        println!();
    }
    println!("ampliando {} referencia(s) de trozo...", trozos.len());

    let informe = volumen
        .store()
        .ampliar_redundancia(destino, &reparto.nuevos, &trozos)
        .await?;

    println!();
    println!("trozos        {}", informe.trozos);
    println!("ya estaban    {}", informe.ya_estaban);
    println!(
        "subidos       {} fragmento(s), {}",
        informe.fragmentos,
        tamano(informe.bytes)
    );

    if !informe.completo() {
        println!("fallidos      {}", informe.fallidos.len());
        for (id, e) in informe.fallidos.iter().take(5) {
            println!("  {id}: {e}");
        }
        println!();
        bail!(
            "no se publica la topologia nueva con trozos sin ampliar: la cabecera es la que \
             promete la redundancia, y prometer una que no esta es peor que no ampliar.\n\
             Vuelve a ejecutarlo: lo ya subido no se repite."
        );
    }

    // El unico punto de compromiso. Hasta aqui el volumen se leia con la topologia vieja.
    let nuevo = Header {
        version: header.version,
        layout: destino.to_string(),
        salt: header.salt.clone(),
        backends: reparto
            .del_volumen
            .iter()
            .chain(&reparto.nuevos)
            .map(|b| b.name().to_string())
            .collect(),
    };
    let todos: Vec<Arc<dyn Backend>> = reparto
        .del_volumen
        .iter()
        .chain(&reparto.nuevos)
        .cloned()
        .collect();
    header::write(&todos, &nuevo).await?;

    println!();
    println!("hecho: el volumen es {destino}");
    println!("Comprueba con `libredisk scrub` que todos los fragmentos estan donde toca.");
    Ok(())
}
