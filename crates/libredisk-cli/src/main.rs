//! Binario `libredisk`.
//!
//! Todavia no monta nada: el montaje FUSE es el siguiente hito. Por ahora expone el
//! almacen de trozos para poder comprobar a mano que el nucleo hace lo que promete.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use libredisk_backend::{Backend, OpendalBackend};
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
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Layout { spec } => describir(&spec),
        Command::Demo { layout, kib, dir } => demo(&layout, kib, &dir).await,
    }
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
