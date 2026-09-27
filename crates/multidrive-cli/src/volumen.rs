//! Las ordenes que operan sobre un volumen de verdad.
//!
//! Los proveedores salen del entorno, igual que en `check`. La frase de paso sale de
//! `MULTIDRIVE_PASSPHRASE` y nunca de un argumento: lo que se escribe en la linea de ordenes
//! acaba en el historial del intérprete y en la lista de procesos.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use multidrive_backend::{Backend, live};
use multidrive_core::ChunkId;
use multidrive_core::diskcache::DiskCache;
use multidrive_core::header::{self, HEADER_KEY, Header};
use multidrive_core::pesos::{pesos_ampliados, pesos_para};
use multidrive_core::rebalance::{self, Cambio, Sacrificio};
use multidrive_core::rescue;
use multidrive_core::sincronizador::{Resultado, Sincronizador};
use multidrive_core::tree::{self, Kind, Volume};
use multidrive_core::writeback::{self, WriteQueue};
use multidrive_core::{ChunkStore, CoreError};
use multidrive_raid::Layout;

const FRASE: &str = "MULTIDRIVE_PASSPHRASE";

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

pub(crate) fn frase() -> Result<String> {
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

pub(crate) fn proveedores() -> Result<Vec<Arc<dyn Backend>>> {
    let configurados = live::configurados()
        .with_context(|| format!("configuracion de proveedores invalida\n\n{}", live::ayuda()))?;
    if configurados.is_empty() {
        bail!("no hay ningun proveedor configurado.\n\n{}", live::ayuda());
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
fn dir_cache(salt: &[u8]) -> PathBuf {
    let base = writeback::dir_base();
    let etiqueta = &blake3_hex(salt)[..16];
    base.join("cache").join(etiqueta)
}

/// La sal no es secreta, pero su hash es un nombre de carpeta mas corto y sin sorpresas.
fn blake3_hex(datos: &[u8]) -> String {
    use multidrive_core::ChunkId;
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
    Ok(header.almacen(backends.to_vec(), &frase()?, cache_bytes)?)
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
        .context("¿has ejecutado `multidrive volume create`?")?;
    let mut store = header.almacen(backends, &frase()?, cache_bytes)?;

    if disco_bytes > 0 {
        let dir = dir_cache(&header.salt);
        let disco = DiskCache::open(&dir, disco_bytes)
            .await
            .with_context(|| format!("no se pudo abrir la cache en '{}'", dir.display()))?;
        store = store.with_disk_cache(disco);
    }

    if diferido {
        let dir = writeback::dir_de_volumen(writeback::dir_base(), &header.salt);
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

    let pesos = pesos_para(layout, &backends).await;
    let store = header::crear(backends, layout, &frase()?, pesos)
        .await
        .map_err(|e| match e {
            CoreError::YaHayVolumen => anyhow::anyhow!(
                "ya hay un volumen en estos proveedores.\n\
                 Si una creacion anterior quedo a medias, borra '{HEADER_KEY}' de cada \
                 proveedor a mano antes de reintentar."
            ),
            otro => otro.into(),
        })?;
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

    // A un temporal y luego renombrar, igual que hace el diario de escritura y por lo mismo:
    // crear el destino directamente lo trunca **antes** de leer nada. Si la bajada falla a
    // medias --o falla del todo, como con un volumen sin redundancia al que le falta un
    // proveedor-- quedaria un fichero a medias con pinta de resultado, y si el destino ya
    // existia se habria destruido una copia buena por intentar bajar otra.
    let temporal = format!("{destino}.multidrive-parcial");
    let mut fichero = tokio::fs::File::create(&temporal)
        .await
        .with_context(|| format!("no se pudo crear '{temporal}'"))?;

    if let Err(e) = volumen.read_file(remoto, &mut fichero).await {
        drop(fichero);
        let _ = tokio::fs::remove_file(&temporal).await;
        return Err(e.into());
    }
    drop(fichero);
    tokio::fs::rename(&temporal, &destino)
        .await
        .with_context(|| format!("no se pudo dejar el resultado en '{destino}'"))?;

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

/// El nombre que tenia la cabecera antes del cambio de nombre del proyecto.
///
/// Es la unica pieza del formato que un volumen viejo no puede resolver por configuracion: la
/// carpeta dentro de la cuenta se declara con `ROOT`, pero esto va escrito en el programa.
const CABECERA_VIEJA: &str = "libredisk-volume.json";

/// Renombra la cabecera de un volumen creado antes del cambio de nombre.
///
/// No mueve ni un fragmento: el resto de las claves son relativas a la carpeta del proveedor y
/// no llevan el nombre del proyecto dentro. Lo unico que cambia es como se llama el objeto que
/// dice que ahi hay un volumen.
///
/// El orden es escribir, comprobar y solo entonces borrar. Al reves, un corte de red entre el
/// borrado y la escritura dejaria el volumen sin cabecera en ese proveedor, que es como
/// dejarlo sin la sal: los fragmentos seguirian ahi y no habria forma de saber que son.
pub async fn migrar() -> Result<()> {
    migrar_en(&proveedores()?).await
}

/// Lo que hace [`migrar`], sobre unos proveedores dados.
///
/// Va aparte para poder probarlo: lo otro sale del entorno, y una orden que **borra** la
/// cabecera vieja no deberia estrenarse contra la cuenta de alguien.
pub async fn migrar_en(backends: &[Arc<dyn Backend>]) -> Result<()> {
    println!("Cabecera: '{CABECERA_VIEJA}'  ->  '{HEADER_KEY}'");
    println!();

    let (mut hechos, mut ya, mut sin_nada) = (0, 0, 0);
    for b in backends {
        let nombre = b.name();
        let nueva = b
            .exists(HEADER_KEY)
            .await
            .with_context(|| format!("preguntando a '{nombre}'"))?;
        let vieja = b
            .exists(CABECERA_VIEJA)
            .await
            .with_context(|| format!("preguntando a '{nombre}'"))?;

        match (nueva, vieja) {
            (true, false) => {
                println!("  {nombre}: ya estaba migrado");
                ya += 1;
            }
            (true, true) => {
                // Una migracion anterior que se corto entre escribir y borrar. La nueva ya
                // esta, asi que lo que queda es terminar de quitar la vieja.
                b.delete(CABECERA_VIEJA).await?;
                println!("  {nombre}: estaban las dos; borrada la vieja");
                hechos += 1;
            }
            (false, true) => {
                let bytes = b.get(CABECERA_VIEJA).await?;
                let cuantos = bytes.len();
                b.put(HEADER_KEY, bytes).await?;

                // Que el `put` no diera error dice que se escribio, no que este ahi. Y lo que
                // se va a borrar es lo unico que quedaria si no lo estuviera.
                if !b.exists(HEADER_KEY).await? {
                    bail!(
                        "en '{nombre}' se escribio '{HEADER_KEY}' pero el proveedor dice que no \
                         esta. No se ha borrado nada: vuelve a intentarlo."
                    );
                }
                b.delete(CABECERA_VIEJA).await?;
                println!("  {nombre}: migrada ({cuantos} bytes)");
                hechos += 1;
            }
            (false, false) => {
                println!("  {nombre}: no hay cabecera de ninguno de los dos nombres");
                sin_nada += 1;
            }
        }
    }

    println!();
    if sin_nada == backends.len() {
        println!("No se encontro ninguna cabecera. Si el volumen es de antes del cambio de");
        println!("nombre, tambien la **carpeta** dentro de cada cuenta se llama distinto: era");
        println!("`/libredisk` y ahora se busca en `/multidrive`. Declara la de antes con:");
        println!();
        for b in backends {
            println!("  MULTIDRIVE_{}_ROOT=/libredisk", b.name().to_uppercase());
        }
        println!();
        println!("o renombra la carpeta en cada cuenta, que viene a ser lo mismo.");
        return Ok(());
    }

    println!("migradas    {hechos}");
    println!("ya estaban  {ya}");
    if sin_nada > 0 {
        println!("sin nada    {sin_nada}");
        println!();
        println!("AVISO: hay proveedores sin cabecera. Si son del volumen, les falta; revisa");
        println!("con `multidrive volume status` y `multidrive scrub`.");
    }
    println!();
    println!("Comprueba con `multidrive volume status`.");
    Ok(())
}
/// Estado del volumen.
pub async fn status() -> Result<()> {
    let backends = proveedores()?;
    let header = header::read(&backends)
        .await?
        .ok_or(CoreError::NoVolume)
        .context("¿has ejecutado `multidrive volume create`?")?;

    // Interpretada y no en bruto: un volumen creado antes del cambio de nombre tiene
    // `stripe:2` escrito, y enseñarlo tal cual seria usar dos vocabularios a la vez.
    println!("topologia     {}", header.layout()?);
    println!("proveedores   {}", header.backends.join(", "));
    println!("formato       version {}", header.version);

    // Lo pendiente no depende de poder abrir el arbol: se lee del diario local.
    let base = writeback::dir_base();
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
    println!("El volumen tiene menos redundancia de la pedida. Ejecuta `multidrive rebuild`.");
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
    println!("  multidrive rescue --adoptar <id>");
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
    publicada: Option<&multidrive_core::tree::Root>,
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
    let montaje = multidrive_mount::montar(Arc::clone(&volumen), punto, !escritura)?;

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
            // Donde escribir lo sabe quien monta, no el nucleo: bajo el lanzador de WinFsp la
            // salida estandar es una tuberia cerrada y escribir ahi mataria el hilo de subida.
            Box::new(move |r| match r {
                Resultado::Hecho(i) => di!(
                    "  [sync] {} subidos ({}), {} descartados{}",
                    i.subidos,
                    tamano(i.bytes),
                    i.descartados,
                    if i.raiz_publicada {
                        ", raiz publicada"
                    } else {
                        ""
                    }
                ),
                Resultado::Fallo(e) => di!("  [sync] fallo, se reintentara: {e}"),
            }),
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
                    "AVISO: quedan {} trozo(s) sin subir. Ejecuta `multidrive sync`.",
                    i.fallidos.len()
                ),
                Err(e) => di!("AVISO: no se pudo subir: {e}. Ejecuta `multidrive sync`."),
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
            "PENDIENTE: {} trozo(s), {} SOLO EN ESTA MAQUINA. Ejecuta `multidrive sync`.",
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
    println!("  multidrive sync");
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
             `multidrive sync`; lo pendiente sigue guardado aqui."
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

/// Los proveedores del volumen frente a los que hay configurados.
///
/// Separarlos importa porque el orden es parte del formato: los que ya estan tienen que seguir
/// exactamente donde estaban, y los nuevos solo pueden ir detras.
struct Proveedores {
    /// Los que la cabecera declara, en su orden.
    del_volumen: Vec<Arc<dyn Backend>>,
    /// Los configurados que la cabecera no menciona, en el orden en que se configuraron.
    nuevos: Vec<Arc<dyn Backend>>,
}

fn separar(header: &Header, configurados: Vec<Arc<dyn Backend>>) -> Result<Proveedores> {
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
    Ok(Proveedores {
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
        .context("¿has ejecutado `multidrive volume create`?")?;
    let layout: Layout = header.layout.parse()?;

    println!("topologia     {layout}");
    println!(
        "tolera perder {} proveedor(es)",
        layout.tolerated_failures()
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
        if matches!(layout, Layout::Lineal { .. }) {
            // En lineal no hay nada que elegir, y el precio no es el mismo: anadir mueve solo
            // lo que aporta el que llega. Ofrecer las dos opciones de siempre haria pensar que
            // esto cuesta lo que cuesta en Reed-Solomon.
            println!("  multidrive providers add   # mas espacio: solo se muda lo que aporta");
        } else {
            println!("  multidrive providers add --mas-seguridad   # mas margen de caidas");
            println!("  multidrive providers add --mas-espacio     # mas capacidad (caro)");
        }
    }
    Ok(())
}

/// Incorpora al volumen los proveedores configurados de mas.
pub async fn providers_add(cambio: Cambio) -> Result<()> {
    let configurados = proveedores()?;
    let header = header::read(&configurados)
        .await?
        .ok_or(CoreError::NoVolume)
        .context("¿has ejecutado `multidrive volume create`?")?;
    let antes: Layout = header.layout.parse()?;
    let quien = separar(&header, configurados)?;

    if quien.nuevos.is_empty() {
        bail!(
            "no hay ningun proveedor configurado que el volumen no tenga ya.\n\
             Configura el nuevo y vuelve a intentarlo: `multidrive providers list`."
        );
    }
    let destino = rebalance::planear(antes, quien.nuevos.len(), cambio)?;

    println!("topologia     {antes}  ->  {destino}");
    println!(
        "tolera perder {}  ->  {}",
        antes.tolerated_failures(),
        destino.tolerated_failures()
    );
    println!(
        "sobrecoste    {:.2}x  ->  {:.2}x",
        antes.storage_overhead(),
        destino.storage_overhead()
    );
    println!("anadiendo     {}", quien.nuevos.len());
    for b in &quien.nuevos {
        println!("              {}", b.name());
    }
    println!();

    // Se lee siempre con la topologia vieja, que es la que describe lo que hay escrito.
    let store = almacen_con(&quien.del_volumen, &header, CACHE_POR_DEFECTO).await?;
    let volumen = Volume::open(store).await?;

    let (trozos, ilegibles) = volumen.walk().await;
    if !ilegibles.is_empty() {
        println!("AVISO: {} ruta(s) ilegibles, se omiten:", ilegibles.len());
        for (ruta, e) in ilegibles.iter().take(5) {
            println!("  {ruta}: {e}");
        }
        println!();
    }

    let todos: Vec<Arc<dyn Backend>> = quien
        .del_volumen
        .iter()
        .chain(&quien.nuevos)
        .cloned()
        .collect();
    let nombres: Vec<String> = todos.iter().map(|b| b.name().to_string()).collect();

    // Con que pesos esta colocado lo que hay escrito. Solo `lineal` los usa, y solo el
    // almacen sabe cuales son de verdad: una cabecera vieja puede no traerlos.
    let base = volumen.store().pesos();

    // Las tres vias se distinguen en cuanto hay que mover, y son tres ordenes de magnitud:
    // nada, la parte que le toca al que llega, o el volumen entero.
    let (reparto_nuevo, pesos_nuevos) = if rebalance::conserva_lo_escrito(antes, destino) {
        println!("ampliando {} referencia(s) de trozo...", trozos.len());
        let informe = volumen
            .store()
            .ampliar_redundancia(destino, &quien.nuevos, &trozos)
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
            return Err(fallo_a_medias(&informe.fallidos));
        }
        (header.reparto, header.pesos.clone())
    } else if rebalance::muda_una_fraccion(antes, destino) {
        // Lo barato de verdad. Cada trozo vive entero en un proveedor, asi que al que llega
        // solo le toca la parte que le corresponde por lo que aporta; el resto ni se lee.
        let pesos = pesos_ampliados(&base, &quien.nuevos).await;
        let parte = rebalance::fraccion_que_se_mueve(&base, &pesos);

        println!("pesos         {base:?}  ->  {pesos:?}");
        println!(
            "mudando       ~{:.0} % de {} referencia(s) de trozo: solo lo que le toca al que \
             llega.",
            parte * 100.0,
            trozos.len()
        );
        println!("Se puede interrumpir: lo copiado no se repite y el volumen se sigue leyendo.");
        println!();

        let informe = volumen
            .store()
            .mudar_reparto(destino, &pesos, &todos, &trozos)
            .await?;

        println!("trozos        {}", informe.trozos);
        println!("quietos       {}", informe.quietos);
        println!("ya estaban    {}", informe.ya_estaban);
        println!(
            "mudados       {}, {}",
            informe.mudados,
            tamano(informe.bytes)
        );
        if !informe.completo() {
            return Err(fallo_a_medias(&informe.fallidos));
        }
        (header.reparto, pesos)
    } else {
        // Aqui cambia todo de sitio, asi que se escribe en un reparto nuevo y los fragmentos
        // viejos se quedan donde estan: son los que sostienen el volumen mientras tanto.
        let nuevo = header.reparto + 1;
        println!("reparto       {} -> {nuevo}", header.reparto);
        println!(
            "reescribiendo {} referencia(s) de trozo. Esto reescribe el volumen entero.",
            trozos.len()
        );
        println!("Se puede interrumpir: lo subido no se repite y el volumen se sigue leyendo.");
        println!();

        let informe = volumen
            .store()
            .reescribir_reparto(destino, nuevo, &todos, &trozos)
            .await?;

        println!("trozos        {}", informe.trozos);
        println!("ya estaban    {}", informe.ya_estaban);
        println!(
            "subidos       {} fragmento(s), {}",
            informe.fragmentos,
            tamano(informe.bytes)
        );
        if !informe.completo() {
            return Err(fallo_a_medias(&informe.fallidos));
        }
        (nuevo, Vec::new())
    };

    // Si los trozos cambiaron de proveedor sin cambiar de reparto, es que se mudaron: la
    // copia vieja sigue ahi y hay que tirarla, pero no antes de publicar la cabecera.
    let mudanza = reparto_nuevo == header.reparto && pesos_nuevos != header.pesos;

    // El unico punto de compromiso. Hasta aqui el volumen se leia con lo de antes.
    let cabecera = Header {
        pesos: pesos_nuevos.clone(),
        version: header.version,
        layout: destino.to_string(),
        reparto: reparto_nuevo,
        salt: header.salt.clone(),
        backends: nombres,
    };
    header::write(&todos, &cabecera).await?;
    println!();
    println!("hecho: el volumen es {destino}");

    // Y solo ahora se puede tirar lo viejo: hasta publicar la cabecera era lo unico que
    // sostenia el volumen.
    if reparto_nuevo != header.reparto || mudanza {
        println!();
        println!("comprobando lo escrito antes de borrar lo que sobra...");

        // Que la subida no diera error dice que se escribio, no que este ahi. Y lo que se
        // va a borrar es lo unico que quedaria si no lo estuviera, asi que se pregunta a los
        // proveedores antes de destruir la red de seguridad.
        let revisor = volumen
            .store()
            .rehecho(destino, todos.clone(), pesos_nuevos.clone())?
            .with_reparto(reparto_nuevo);
        let salud = revisor.scrub_many(&trozos).await;

        if !salud.is_complete() {
            println!("  hay proveedores que no se dejaron enumerar; no se borra nada.");
            println!(
                "  Vuelve a ejecutarlo cuando respondan: `multidrive providers add` es idempotente."
            );
            return Ok(());
        }
        let incompletos = salud
            .health
            .iter()
            .filter(|h| !h.missing.is_empty())
            .count();
        if incompletos > 0 {
            println!("  {incompletos} trozo(s) con fragmentos ausentes; NO se borra nada.");
            println!("  El volumen se lee igual, pero revisa con `multidrive scrub` y `rebuild`.");
            return Ok(());
        }

        let borrados = if mudanza {
            println!("  entero. Borrando la copia que dejaron los trozos mudados...");
            volumen.store().olvidar_origen(&pesos_nuevos, &trozos).await
        } else {
            println!("  entero. Borrando el reparto {}...", header.reparto);
            volumen
                .store()
                .olvidar_reparto(header.reparto, antes.shard_count(), &todos, &trozos)
                .await
        };
        println!("  {borrados} fragmento(s) borrados");
    }

    println!("Comprueba con `multidrive scrub` que todos los fragmentos estan donde toca.");
    Ok(())
}

/// Un cambio de topologia a medias **no se publica**.
///
/// La cabecera es la que promete la redundancia y decide que reparto vale. Publicarla con
/// trozos sin terminar prometeria algo que no esta, que es peor que no haber cambiado nada.
fn fallo_a_medias(fallidos: &[(ChunkId, String)]) -> anyhow::Error {
    println!("fallidos      {}", fallidos.len());
    for (id, e) in fallidos.iter().take(5) {
        println!("  {id}: {e}");
    }
    println!();
    anyhow::anyhow!(
        "no se publica la topologia nueva con trozos sin terminar: la cabecera es la que \n         promete la redundancia.
\n         Vuelve a ejecutarlo, que lo ya subido no se repite."
    )
}

/// Saca del volumen un proveedor.
///
/// Si sigue configurado se lee de el y se le borran los fragmentos al final. Si ya no esta
/// --el caso que de verdad aprieta: la cuenta suspendida-- se sustituye por uno vacio y se
/// avisa de que su sitio no se puede reclamar.
pub async fn providers_remove(nombre: &str, sacrificio: Sacrificio) -> Result<()> {
    let configurados = proveedores()?;
    let header = header::read(&configurados)
        .await?
        .ok_or(CoreError::NoVolume)
        .context("¿has ejecutado `multidrive volume create`?")?;
    let antes: Layout = header.layout.parse()?;

    let Some(posicion) = header.backends.iter().position(|b| b == nombre) else {
        bail!(
            "'{nombre}' no es uno de los proveedores del volumen.\n\
             Los que hay: {}",
            header.backends.join(", ")
        );
    };

    // Los del volumen, en su orden, sustituyendo por uno vacio el que ya no este configurado.
    // Sin esto no se podria retirar una cuenta muerta, que es justo cuando hace falta.
    let mut ausentes = Vec::new();
    let del_volumen: Vec<Arc<dyn Backend>> = header
        .backends
        .iter()
        .map(|n| {
            match configurados.iter().find(|b| b.name() == n) {
                Some(b) => Ok(Arc::clone(b)),
                None => {
                    ausentes.push(n.clone());
                    // Un proveedor vacio: todo lo que se le pida fallara, que es exactamente
                    // lo que hace uno caido. El motor de RAID ya sabe convivir con eso.
                    multidrive_backend::OpendalBackend::memory(n.clone())
                        .map(|b| Arc::new(b) as Arc<dyn Backend>)
                        .map_err(anyhow::Error::from)
                }
            }
        })
        .collect::<Result<_>>()?;

    if !ausentes.is_empty() {
        println!(
            "AVISO: sin configurar, se tratan como caidos: {}",
            ausentes.join(", ")
        );
        println!();
    }

    let destino = rebalance::planear_retirada(antes, 1, sacrificio)?;
    let es_el_ultimo = posicion == header.backends.len() - 1;
    let recorte = es_el_ultimo && rebalance::basta_con_recortar(antes, destino);

    println!("retirando     {nombre}  (posicion {posicion})");
    println!("topologia     {antes}  ->  {destino}");
    println!(
        "tolera perder {}  ->  {}",
        antes.tolerated_failures(),
        destino.tolerated_failures()
    );
    println!(
        "sobrecoste    {:.2}x  ->  {:.2}x",
        antes.storage_overhead(),
        destino.storage_overhead()
    );
    println!(
        "coste         {}",
        if recorte {
            "ninguno: no se mueve ni un fragmento"
        } else {
            "reescribe el volumen entero"
        }
    );
    println!();

    let store = almacen_con(&del_volumen, &header, CACHE_POR_DEFECTO).await?;
    let volumen = Volume::open(store).await?;
    let (trozos, ilegibles) = volumen.walk().await;
    if !ilegibles.is_empty() {
        println!("AVISO: {} ruta(s) ilegibles, se omiten:", ilegibles.len());
        for (ruta, e) in ilegibles.iter().take(5) {
            println!("  {ruta}: {e}");
        }
        println!();
    }

    // Los que se quedan, en el orden que tendran.
    let quedan: Vec<Arc<dyn Backend>> = del_volumen
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != posicion)
        .map(|(_, b)| Arc::clone(b))
        .collect();

    let reparto_nuevo = if recorte {
        // Nada que mover: los que se quedan ya tienen sus fragmentos donde toca.
        header.reparto
    } else {
        let nuevo = header.reparto + 1;
        println!("reparto       {} -> {nuevo}", header.reparto);
        println!(
            "reescribiendo {} referencia(s) de trozo. Esto reescribe el volumen entero.",
            trozos.len()
        );
        println!("Se puede interrumpir: lo subido no se repite y el volumen se sigue leyendo.");
        println!();

        let informe = volumen
            .store()
            .reescribir_reparto(destino, nuevo, &quedan, &trozos)
            .await?;
        println!("trozos        {}", informe.trozos);
        println!("ya estaban    {}", informe.ya_estaban);
        println!(
            "subidos       {} fragmento(s), {}",
            informe.fragmentos,
            tamano(informe.bytes)
        );
        if !informe.completo() {
            return Err(fallo_a_medias(&informe.fallidos));
        }
        nuevo
    };

    // El punto de compromiso.
    let cabecera = Header {
        pesos: Vec::new(),
        version: header.version,
        layout: destino.to_string(),
        reparto: reparto_nuevo,
        salt: header.salt.clone(),
        backends: header
            .backends
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != posicion)
            .map(|(_, n)| n.clone())
            .collect(),
    };
    header::write(&quedan, &cabecera).await?;
    println!();
    println!("hecho: el volumen es {destino} sin {nombre}");

    // Y ahora lo que sobra. Borrar contra el suplente vacio "funcionaria" y contarlo seria
    // mentir: el sitio sigue ocupado en una cuenta a la que ya no llegamos.
    println!();
    let fallidos = if ausentes.iter().any(|a| a == nombre) {
        println!(
            "no se libera lo que ocupaba {nombre}: no esta configurado, \
             asi que no hay a donde pedirselo."
        );
        0
    } else {
        println!("liberando lo que ocupaba {nombre}...");
        let retirado = std::slice::from_ref(&del_volumen[posicion]);
        let (borrados, fallidos) = volumen
            .store()
            .olvidar_proveedores(&[posicion], retirado, &trozos)
            .await;
        println!("  {borrados} fragmento(s) borrados, {fallidos} que no se pudieron borrar");
        fallidos
    };

    if reparto_nuevo != header.reparto {
        println!();
        println!(
            "borrando el reparto {} de los que se quedan...",
            header.reparto
        );
        let n = volumen
            .store()
            .olvidar_reparto(header.reparto, antes.shard_count(), &del_volumen, &trozos)
            .await;
        println!("  {n} fragmento(s) borrados");
    }

    if fallidos > 0 || ausentes.iter().any(|a| a == nombre) {
        println!();
        println!("Los fragmentos que quedan alli siguen ocupando sitio en esa cuenta.");
        println!("No afectan al volumen: ya no forman parte de el. Borralos a mano si puedes.");
    }
    println!();
    println!("Quita ahora {nombre} de tu configuracion.");
    Ok(())
}
/// Cuanto espacio hay en cada proveedor, y cuanto de el ocupa el volumen.
///
/// Las dos cifras vienen de sitios distintos y cuestan cosas distintas, y eso se ve en la
/// salida: el cupo de la cuenta es **una peticion** por proveedor, mientras que lo que ocupa
/// el volumen obliga a **enumerar**, que contra una nube son minutos. Por eso lo segundo va
/// detras de `--medir` en vez de pasar siempre.
pub async fn espacio(medir: bool) -> Result<()> {
    let backends = multidrive_backend::live::configurados()?;
    if backends.is_empty() {
        println!("No hay ningun proveedor configurado.");
        return Ok(());
    }

    // Lo que ocupa el volumen solo se puede saber con la frase de paso: los fragmentos estan
    // bajo un prefijo conocido, pero es el volumen abierto quien dice cual.
    let ocupado = if medir {
        let volumen = abrir().await?;
        let (chunks, _) = volumen.walk().await;
        let informe = volumen.store().scrub_many(&chunks).await;
        let nombres = volumen.store().nombres();
        Some((nombres, informe.ocupado))
    } else {
        None
    };

    for (i, b) in backends.iter().enumerate() {
        let nombre = b.name();
        println!("{nombre}");

        match b.cuota().await {
            Ok(c) => match (c.total, c.usado) {
                (Some(total), Some(usado)) => {
                    println!(
                        "  cuenta        {} de {}   ({:.0} % ocupado, {} libres)",
                        tamano(usado),
                        tamano(total),
                        (usado as f64 / total.max(1) as f64) * 100.0,
                        tamano(total.saturating_sub(usado))
                    );
                }
                (None, Some(usado)) => {
                    println!(
                        "  cuenta        {} ocupados, sin limite declarado",
                        tamano(usado)
                    )
                }
                // Ni total ni ocupado: el proveedor no dice nada de su espacio. No es un
                // fallo, y decir "0 de 0" lo pareceria.
                _ => println!("  cuenta        no declara cupo (se paga por lo que ocupa)"),
            },
            // Un proveedor que no contesta no es un proveedor vacio. Se dice el motivo en vez
            // de poner un cero que se leeria como "aqui no hay nada guardado".
            Err(e) => println!("  cuenta        NO SE PUDO CONSULTAR: {e}"),
        }

        match &ocupado {
            // Se casa por nombre y no solo por posicion: si la configuracion ha cambiado
            // desde la medida, vale mas no decir nada que atribuirsela a otro proveedor.
            Some((nombres, bytes)) if nombres.get(i).is_some_and(|n| n == nombre) => {
                match bytes.get(i).copied().flatten() {
                    Some(b) => println!("  este volumen  {}", tamano(b)),
                    None => println!("  este volumen  no se pudo enumerar"),
                }
            }
            Some(_) => println!("  este volumen  (este proveedor no esta en el volumen)"),
            None => {}
        }
    }

    if ocupado.is_none() {
        println!();
        println!("Lo de arriba es la cuenta entera del proveedor, no lo que ocupa MultiDrive.");
        println!("Con --medir se enumera el volumen y se dice tambien eso, pero tarda:");
        println!("una enumeracion completa contra una nube se mide en minutos.");
    }
    Ok(())
}

/// Retira el volumen de los proveedores: borra sus fragmentos, su raiz y su cabecera.
///
/// **No se puede deshacer, y no hay papelera.** Por eso no basta con teclear la orden: hay
/// que escribir aparte los nombres de los proveedores, que es lo que obliga a mirar la lista
/// de lo que se va a borrar antes de confirmarla. Un `--si` suelto se teclea sin leer.
///
/// Solo toca **lo que es del volumen**: el prefijo de fragmentos, el puntero de raiz y la
/// cabecera. Si el usuario guarda otras cosas en esa misma carpeta —cosa que la
/// documentacion desaconseja pero nadie impide— se quedan donde estan. Borrar todo lo que
/// hubiera debajo seria mas facil de escribir y mucho peor de explicar.
pub async fn destroy(confirmacion: Option<String>) -> Result<()> {
    let backends = proveedores()?;
    let nombres: Vec<String> = backends
        .iter()
        .map(|b| Backend::name(b.as_ref()).to_string())
        .collect();

    let header = header::read(&backends)
        .await?
        .context("no hay ningun volumen en estos proveedores")?;

    // Los configurados tienen que ser los del volumen. Sin esto se podria retirar media
    // mitad --por ejemplo si un proveedor se quedo fuera de la configuracion-- y la orden
    // diria que termino, dejando la otra mitad como basura que ya nadie sabe de quien era.
    header.check_backends(&backends).context(
        "los proveedores configurados no son los del volumen. No se borra nada: retirarlo a \
         medias dejaria fragmentos sueltos que ya nadie podria identificar",
    )?;

    println!("Se va a RETIRAR el volumen de:");
    for n in &nombres {
        println!("  {n}");
    }
    println!();
    println!("  topologia     {}", header.layout()?);
    println!("  proveedores   {}", header.backends.join(", "));

    // Contar antes de borrar. Cuesta una enumeracion por proveedor, y es lo que convierte
    // "se va a borrar el volumen" en una cifra que se puede mirar antes de decir que si.
    let inventario = multidrive_core::retirar::inventariar(&backends)
        .await
        .context("no se borra nada: sin poder mirarlo entero, el recuento seria falso")?;
    println!(
        "  fragmentos    {} ({})",
        inventario.fragmentos(),
        tamano(inventario.bytes())
    );
    println!();

    let esperado = nombres.join(",");
    match confirmacion.as_deref() {
        Some(dado) if dado == esperado => {}
        _ => {
            println!("Esto NO se puede deshacer y no hay papelera.");
            println!("Si es lo que quieres, repite la orden confirmando los proveedores:");
            println!();
            println!("  multidrive volume destroy --confirmar {esperado}");
            return Ok(());
        }
    }

    let hecho = multidrive_core::retirar::retirar(&backends).await?;
    for (nombre, n, _) in &hecho.por_proveedor {
        println!("  {nombre}: {n} fragmento(s), raiz y cabecera");
    }

    println!();
    println!(
        "retirado      {} fragmento(s) en {} proveedor(es)",
        hecho.fragmentos(),
        backends.len()
    );
    println!();
    println!("La carpeta sigue existiendo en cada proveedor, vacia de lo que era del volumen.");
    println!("Si guardaste ahi otras cosas, siguen donde estaban.");
    Ok(())
}

#[cfg(test)]
mod tests_migrar {
    use std::sync::Arc;

    use multidrive_backend::{Backend, OpendalBackend};

    use super::*;

    fn memoria(nombre: &str) -> Arc<dyn Backend> {
        Arc::new(OpendalBackend::memory(nombre).unwrap()) as Arc<dyn Backend>
    }

    /// La cabecera vieja pasa a llamarse como la nueva, **con los mismos bytes**.
    ///
    /// Que sean los mismos importa: dentro van la sal y la topologia. Un byte distinto es un
    /// volumen que no abre.
    #[tokio::test]
    async fn la_cabecera_vieja_pasa_a_llamarse_como_la_nueva() {
        let b = memoria("uno");
        let cabecera = br#"{"version":1,"layout":"lineal:2"}"#.to_vec();
        b.put(CABECERA_VIEJA, cabecera.clone()).await.unwrap();

        migrar_en(&[Arc::clone(&b)]).await.unwrap();

        assert_eq!(b.get(HEADER_KEY).await.unwrap(), cabecera);
        assert!(
            !b.exists(CABECERA_VIEJA).await.unwrap(),
            "la vieja tiene que desaparecer, o el volumen queda con dos cabeceras"
        );
    }

    /// Volver a lanzarlo no rompe nada ni borra lo que hay.
    #[tokio::test]
    async fn volver_a_lanzarlo_no_toca_nada() {
        let b = memoria("uno");
        let cabecera = b"la cabecera".to_vec();
        b.put(CABECERA_VIEJA, cabecera.clone()).await.unwrap();

        migrar_en(&[Arc::clone(&b)]).await.unwrap();
        migrar_en(&[Arc::clone(&b)]).await.unwrap();

        assert_eq!(b.get(HEADER_KEY).await.unwrap(), cabecera);
    }

    /// Una migracion que se corto entre escribir y borrar se termina, y **no se pisa la
    /// nueva**: la buena es la que ya esta publicada.
    #[tokio::test]
    async fn si_estan_las_dos_se_borra_la_vieja_y_se_respeta_la_nueva() {
        let b = memoria("uno");
        b.put(CABECERA_VIEJA, b"la de antes".to_vec())
            .await
            .unwrap();
        b.put(HEADER_KEY, b"la buena".to_vec()).await.unwrap();

        migrar_en(&[Arc::clone(&b)]).await.unwrap();

        assert_eq!(b.get(HEADER_KEY).await.unwrap(), b"la buena");
        assert!(!b.exists(CABECERA_VIEJA).await.unwrap());
    }

    /// Sin cabecera de ninguno de los dos nombres no se inventa una.
    #[tokio::test]
    async fn sin_cabecera_no_se_crea_ninguna() {
        let b = memoria("uno");
        migrar_en(&[Arc::clone(&b)]).await.unwrap();

        assert!(!b.exists(HEADER_KEY).await.unwrap());
        assert!(!b.exists(CABECERA_VIEJA).await.unwrap());
    }

    /// Con varios proveedores se migran todos: la cabecera va replicada en cada uno, y dejar
    /// a uno con el nombre viejo es dejarlo sin cabecera para quien la busque.
    #[tokio::test]
    async fn se_migran_todos_los_proveedores() {
        let bs = [memoria("uno"), memoria("dos"), memoria("tres")];
        for b in &bs {
            b.put(CABECERA_VIEJA, b"igual en todos".to_vec())
                .await
                .unwrap();
        }

        migrar_en(&bs).await.unwrap();

        for b in &bs {
            assert_eq!(b.get(HEADER_KEY).await.unwrap(), b"igual en todos");
            assert!(!b.exists(CABECERA_VIEJA).await.unwrap(), "{}", b.name());
        }
    }
}
