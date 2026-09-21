//! Las ordenes que operan sobre un volumen de verdad.
//!
//! Los proveedores salen del entorno, igual que en `check`. La frase de paso sale de
//! `LIBREDISK_PASSPHRASE` y nunca de un argumento: lo que se escribe en la linea de ordenes
//! acaba en el historial del intérprete y en la lista de procesos.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use libredisk_backend::{Backend, live};
use libredisk_core::header::{self, HEADER_KEY, HEADER_VERSION, Header};
use libredisk_core::tree::{Kind, Volume};
use libredisk_core::{ChunkStore, CoreError};
use libredisk_crypto::MasterKey;
use libredisk_raid::Layout;

const FRASE: &str = "LIBREDISK_PASSPHRASE";

fn frase() -> Result<String> {
    match std::env::var(FRASE) {
        Ok(f) if !f.trim().is_empty() => Ok(f),
        _ => bail!(
            "falta la frase de paso.\n\n  export {FRASE}='...'\n\n\
             No hay recuperacion: si la pierdes, pierdes el volumen."
        ),
    }
}

fn proveedores() -> Result<Vec<Arc<dyn Backend>>> {
    let configurados = live::desde_entorno()
        .with_context(|| format!("configuracion de proveedores invalida\n\n{}", live::AYUDA))?;
    if configurados.is_empty() {
        bail!("no hay ningun proveedor configurado.\n\n{}", live::AYUDA);
    }
    Ok(configurados
        .into_iter()
        .map(|b| Arc::new(b) as Arc<dyn Backend>)
        .collect())
}

/// Abre el volumen que haya en los proveedores del entorno.
pub async fn abrir() -> Result<Volume> {
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
    let store = ChunkStore::new(layout, backends, key)?;

    match Volume::open(store).await {
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
