//! Binario `libredisk`.
//!
//! Todavia no monta nada: el montaje FUSE es el siguiente hito. Por ahora expone el
//! almacen de trozos para poder comprobar a mano que el nucleo hace lo que promete.

use std::sync::Arc;

mod volumen;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use libredisk_backend::{Backend, OpendalBackend, conformance, live};
use libredisk_core::ChunkStore;
use libredisk_crypto::MasterKey;
use libredisk_raid::Layout;

#[derive(Parser)]
#[command(name = "libredisk", version, about = "Una sola unidad, muchas nubes")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Describe una topologia: tolerancia a fallos y sobrecoste de espacio.
    Layout {
        /// Topologia: `concat`, `mirror:3` o `rs:4,2`.
        spec: String,
    },
    /// Ciclo completo sobre directorios locales: escribir, perder proveedores, leer, reparar.
    ///
    /// Cada "proveedor" es un subdirectorio, para poder ver los fragmentos con `ls`.
    Demo {
        /// Topologia a demostrar.
        #[arg(long, default_value = "rs:4,2")]
        layout: String,
        /// Tamano del dato de prueba, en KiB.
        #[arg(long, default_value_t = 512)]
        kib: usize,
        /// Donde crear los directorios de los proveedores.
        #[arg(long, default_value = "/tmp/libredisk-demo")]
        dir: String,
    },
    /// Comprueba que los proveedores configurados en el entorno cumplen el contrato.
    ///
    /// Escribe y borra objetos de prueba en una carpeta dedicada de cada cuenta. No toca
    /// nada mas. Sin variables de entorno configuradas, solo explica cuales reconoce.
    Check {
        /// Numero de pasadas. Con mas de una, informa de mediana, rango y dispersion.
        ///
        /// Una sola pasada sirve de semaforo, no de medida: en nubes de consumo la
        /// diferencia entre dos ejecuciones seguidas puede pasar del 40 %.
        #[arg(long, default_value_t = 1)]
        pasadas: usize,
    },
    /// Crea, inspecciona y usa un volumen sobre los proveedores del entorno.
    #[command(subcommand)]
    Volume(VolumeCmd),
    /// Lista un directorio del volumen.
    Ls {
        #[arg(default_value = "/")]
        path: String,
    },
    /// Sube un fichero local al volumen.
    Put { local: String, remoto: String },
    /// Baja un fichero del volumen.
    Get {
        remoto: String,
        /// Destino local. Por defecto, el nombre del fichero.
        local: Option<String>,
    },
    /// Muestra un trozo de un fichero sin bajarlo entero.
    Cat {
        remoto: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 4096)]
        len: usize,
    },
    /// Crea un directorio, con los que falten por encima.
    Mkdir { path: String },
    /// Borra un fichero, o un directorio vacio.
    Rm { path: String },
    /// Revisa la salud de todos los trozos del volumen.
    Scrub,
    /// Regenera los fragmentos que falten, donde todavia se pueda.
    Rebuild,
    /// Reconstruye el volumen a partir de lo que quede en los proveedores.
    ///
    /// Para cuando se ha perdido el puntero de raiz. Enumera todos los trozos, los
    /// descifra y busca los directorios a los que no apunta nadie: son las raices
    /// candidatas, incluidas las de versiones anteriores.
    Rescue {
        /// Identificador de la raiz a la que volver.
        #[arg(long)]
        adoptar: Option<String>,
    },
    /// Respalda o restaura la cabecera del volumen, que lleva la sal.
    #[command(subcommand)]
    Header(HeaderCmd),
}

#[derive(Subcommand)]
enum VolumeCmd {
    /// Crea un volumen nuevo. Falla si ya hay uno.
    Create {
        /// Topologia: `concat`, `mirror:3` o `rs:4,2`.
        #[arg(long, default_value = "rs:4,2")]
        layout: String,
    },
    /// Topologia, proveedores y generacion actual.
    Status,
}

#[derive(Subcommand)]
enum HeaderCmd {
    /// Vuelca la cabecera a un fichero local, para guardarla con la frase de paso.
    Export { fichero: String },
    /// Restaura la cabecera en los proveedores desde un fichero local.
    Import { fichero: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Layout { spec } => describir(&spec),
        Command::Demo { layout, kib, dir } => demo(&layout, kib, &dir).await,
        Command::Check { pasadas } => check(pasadas).await,
        Command::Volume(VolumeCmd::Create { layout }) => volumen::create(&layout).await,
        Command::Volume(VolumeCmd::Status) => volumen::status().await,
        Command::Ls { path } => volumen::ls(&path).await,
        Command::Put { local, remoto } => volumen::put(&local, &remoto).await,
        Command::Get { remoto, local } => volumen::get(&remoto, local.as_deref()).await,
        Command::Cat {
            remoto,
            offset,
            len,
        } => volumen::cat(&remoto, offset, len).await,
        Command::Mkdir { path } => volumen::mkdir(&path).await,
        Command::Rm { path } => volumen::rm(&path).await,
        Command::Scrub => volumen::scrub().await,
        Command::Rebuild => volumen::rebuild().await,
        Command::Rescue { adoptar } => volumen::rescue(adoptar.as_deref()).await,
        Command::Header(HeaderCmd::Export { fichero }) => volumen::header_export(&fichero).await,
        Command::Header(HeaderCmd::Import { fichero }) => volumen::header_import(&fichero).await,
    }
}

async fn check(pasadas: usize) -> Result<()> {
    let proveedores = live::desde_entorno()
        .with_context(|| format!("configuracion de proveedores invalida\n\n{}", live::AYUDA))?;

    if proveedores.is_empty() {
        println!("No hay ningun proveedor configurado.\n");
        println!("{}", live::AYUDA);
        return Ok(());
    }

    let mut con_fallos = Vec::new();
    for proveedor in &proveedores {
        let informe = conformance::ejecutar_varias(proveedor, pasadas).await;
        imprimir(&informe);
        if !informe.todo_ok() {
            con_fallos.push(informe.proveedor.clone());
        }
    }

    if con_fallos.is_empty() {
        println!("Todos los proveedores cumplen el contrato de Backend.");
        Ok(())
    } else {
        anyhow::bail!("no sirven como pata de un RAID: {}", con_fallos.join(", "))
    }
}

fn imprimir(informe: &conformance::InformeAgregado) {
    let varias = informe.pasadas > 1;
    if varias {
        println!(
            "=== {} === ({} pasadas)",
            informe.proveedor, informe.pasadas
        );
        println!(
            "  {:<6} {:<32} {:>10} {:>8} {:>8} {:>7}",
            "", "comprobacion", "mediana", "min", "max", "disp."
        );
    } else {
        println!("=== {} ===", informe.proveedor);
    }

    for c in &informe.checks {
        let estado = if c.ok() { "ok" } else { "FALLA" };
        let mediana = format!("{} ms", c.mediana.as_millis());
        if varias {
            println!(
                "  {estado:<6} {:<32} {mediana:>10} {:>8} {:>8} {:>6.0}%",
                c.nombre,
                c.min.as_millis(),
                c.max.as_millis(),
                c.dispersion() * 100.0,
            );
        } else {
            println!("  {estado:<6} {:<32} {mediana:>10}", c.nombre);
        }

        // Fallar a veces es peor noticia que fallar siempre: el proveedor cumple a ratos.
        if !c.ok() && c.pasadas_ok > 0 {
            println!(
                "         intermitente: paso {} de {} pasadas",
                c.pasadas_ok, informe.pasadas
            );
        }
        for motivo in &c.fallos {
            println!("         {motivo}");
        }
        if !c.ok() {
            println!("         importa porque: {}", c.porque);
        }
    }

    for (prefijo, motivo) in &informe.limpiezas_fallidas {
        println!("  AVISO  quedaron objetos sin borrar en '{prefijo}': {motivo}");
    }
    println!();
}

fn describir(spec: &str) -> Result<()> {
    let layout: Layout = spec
        .parse()
        .with_context(|| format!("topologia invalida: {spec}"))?;
    println!("topologia           {layout}");
    println!("proveedores         {}", layout.shard_count());
    println!("fragmentos minimos  {}", layout.shards_needed());
    println!(
        "tolera perder       {} proveedor(es)",
        layout.tolerated_failures()
    );
    println!("sobrecoste espacio  {:.2}x", layout.storage_overhead());
    println!();
    println!(
        "Para guardar 1 TB utiles harian falta {:.2} TB repartidos.",
        layout.storage_overhead()
    );
    Ok(())
}

async fn demo(spec: &str, kib: usize, dir: &str) -> Result<()> {
    let layout: Layout = spec
        .parse()
        .with_context(|| format!("topologia invalida: {spec}"))?;
    let base = std::path::Path::new(dir);
    if base.exists() {
        std::fs::remove_dir_all(base).context("no se pudo limpiar el directorio de la demo")?;
    }

    println!("== configuracion ==");
    println!(
        "topologia {layout}, {} proveedores, tolera {} caidas\n",
        layout.shard_count(),
        layout.tolerated_failures()
    );

    let mut backends: Vec<Arc<dyn Backend>> = Vec::new();
    for i in 0..layout.shard_count() {
        let root = base.join(format!("proveedor-{i}"));
        std::fs::create_dir_all(&root)?;
        backends.push(Arc::new(OpendalBackend::fs(
            format!("proveedor-{i}"),
            root.to_str().context("ruta no valida en UTF-8")?,
        )?));
    }

    // Parametros baratos: en produccion la derivacion es deliberadamente lenta.
    let key = MasterKey::derive_with("frase de la demo", b"sal-de-la-demo16", 8, 1)?;
    let store = ChunkStore::new(layout, backends, key)?;

    let datos: Vec<u8> = (0..kib * 1024).map(|i| (i % 251) as u8).collect();
    println!("== escritura ==");
    let report = store.put(&datos).await?;
    let redundancia = if report.is_fully_redundant() {
        "completa"
    } else {
        "degradada"
    };
    let chunk = report.chunk;
    println!("{} KiB escritos", kib);
    println!("id del trozo  {}", chunk.id);
    println!("redundancia   {redundancia}\n");

    println!("fragmentos en disco:");
    for i in 0..layout.shard_count() {
        let path = base
            .join(format!("proveedor-{i}"))
            .join(chunk.id.shard_key(i));
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        println!("  proveedor-{i}  {size:>8} bytes");
    }

    let caidos: Vec<usize> = (0..layout.tolerated_failures()).collect();
    if !caidos.is_empty() {
        println!(
            "\n== simulando la caida de {} proveedor(es) ==",
            caidos.len()
        );
        for &i in &caidos {
            std::fs::remove_dir_all(base.join(format!("proveedor-{i}")))?;
            println!("  proveedor-{i} borrado por completo");
        }

        let salud = store.scrub(&chunk).await;
        println!(
            "\nfragmentos presentes {:?}, ausentes {:?}",
            salud.present, salud.missing
        );

        println!("\n== lectura con los proveedores caidos ==");
        let recuperado = store
            .get(&chunk)
            .await
            .context("no se pudo recuperar el trozo")?;
        anyhow::ensure!(recuperado == datos, "los datos recuperados no coinciden");
        println!(
            "  {} KiB recuperados e integros (hash verificado)",
            recuperado.len() / 1024
        );

        println!("\n== reparacion ==");
        for &i in &caidos {
            std::fs::create_dir_all(base.join(format!("proveedor-{i}")))?;
        }
        let reparados = store.repair(&chunk).await?;
        println!("  fragmentos regenerados: {reparados:?}");
        println!(
            "  tolerancia restaurada a {} caidas",
            store.scrub(&chunk).await.remaining_tolerance
        );
    }

    println!("\nDirectorios de la demo en {dir}");
    Ok(())
}
