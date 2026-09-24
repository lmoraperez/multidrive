//! Binario `libredisk`.
//!
//! Monta el volumen como una unidad en Windows (WinFsp) y expone el almacen de trozos para
//! poder comprobar a mano que el nucleo hace lo que promete. En Linux el montaje FUSE sigue
//! pendiente; todo lo demas funciona igual.

use std::sync::Arc;

mod configuracion;
mod servicio;
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
    Put {
        local: String,
        remoto: String,
        /// Prepara la subida en local y vuelve enseguida, sin tocar los proveedores.
        ///
        /// Lo escrito queda **solo en esta maquina** hasta `libredisk sync`.
        #[arg(long)]
        diferido: bool,
    },
    /// Sube lo que haya quedado pendiente de una escritura diferida.
    Sync,
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
    /// Cuanto espacio hay en cada proveedor y cuanto ocupa el volumen.
    Espacio {
        /// Enumera para saber tambien cuanto ocupa el volumen en cada uno.
        ///
        /// Sin esto solo se pregunta el cupo de la cuenta, que es una peticion. Con
        /// esto hay que enumerar, y contra una nube eso son minutos.
        #[arg(long)]
        medir: bool,
    },
    /// Revisa la salud de todos los trozos del volumen.
    Scrub,
    /// Regenera los fragmentos que falten, donde todavia se pueda.
    Rebuild,
    /// Monta el volumen como una unidad del sistema (Windows, WinFsp).
    Mount {
        /// Letra de unidad, como `Z:`, o un directorio vacio.
        punto: String,
        /// Tamano de la cache de lectura, en MiB.
        ///
        /// Es lo unico que separa un montaje usable de una demostracion: sin cache, cada
        /// lectura del sistema de ficheros se resuelve contra la nube.
        #[arg(long, default_value_t = 512)]
        cache_mb: usize,
        /// Tamano de la cache en disco, en MiB. Con 0, no se usa.
        ///
        /// Guarda los trozos **cifrados** y sobrevive al reinicio: es lo que evita pagar el
        /// arranque en frio cada vez que se monta.
        #[arg(long, default_value_t = 4096)]
        disco_mb: usize,
        /// Monta en lectura y escritura.
        ///
        /// Lo que se escriba queda **solo en esta maquina** hasta `libredisk sync`.
        #[arg(long)]
        escritura: bool,
        /// Cada cuantos segundos sube lo pendiente, con `--escritura`.
        #[arg(long, default_value_t = 30)]
        sync_cada: u64,
        /// Uso interno: lo arranco el lanzador de WinFsp.
        ///
        /// La frase de paso llega por la entrada estandar y hay que responderle `OK`. No
        /// tiene sentido a mano: sin nadie al otro lado, se queda esperando una frase que
        /// no va a llegar.
        #[arg(long, hide = true)]
        desde_lanzador: bool,
    },
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
    /// De donde salen las credenciales de los proveedores.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Registra el volumen en el lanzador de WinFsp para montarlo como unidad de red.
    #[command(subcommand)]
    Servicio(ServicioCmd),
    /// Proveedores del volumen: cuales tiene y como anadir otros.
    #[command(subcommand)]
    Providers(ProvidersCmd),
}

#[derive(Subcommand)]
enum VolumeCmd {
    /// Crea un volumen nuevo. Falla si ya hay uno.
    Create {
        /// Topologia: `concat`, `mirror:3` o `rs:4,2`.
        #[arg(long, default_value = "rs:4,2")]
        layout: String,
    },
    /// Retira el volumen de los proveedores: borra sus fragmentos, su raiz y su cabecera.
    ///
    /// No se puede deshacer y no hay papelera. Sin `--confirmar` solo ensena que se borraria.
    Destroy {
        /// Los nombres de los proveedores separados por comas, tal y como los lista la
        /// propia orden al ensenar lo que va a borrar.
        ///
        /// Se piden escritos aparte a proposito: obliga a mirar la lista antes de
        /// confirmarla, y un `--si` suelto se teclea sin leer.
        #[arg(long)]
        confirmar: Option<String>,
    },
    /// Topologia, proveedores y generacion actual.
    Status,
}

#[derive(Subcommand)]
enum ProvidersCmd {
    /// Que proveedores tiene el volumen, y cuales hay configurados de mas.
    List,
    /// Saca del volumen un proveedor.
    ///
    /// Funciona aunque el proveedor ya no responda, que es el caso que de verdad aprieta.
    Remove {
        /// Nombre del proveedor, tal y como lo lista `providers list`.
        nombre: String,
        /// Mismo espacio util, un margen de caida menos. `rs(k,m)` pasa a `rs(k,m-1)`.
        ///
        /// Si el que se retira es el ultimo, no se mueve ni un fragmento.
        #[arg(long, conflicts_with = "menos_espacio")]
        menos_seguridad: bool,
        /// Mismo margen de caidas, menos espacio util. `rs(k,m)` pasa a `rs(k-1,m)`.
        ///
        /// Reescribe el volumen entero.
        #[arg(long)]
        menos_espacio: bool,
    },
    /// Incorpora al volumen los proveedores configurados de mas.
    ///
    /// Hay que elegir para que: las dos cosas cuestan ordenes de magnitud distintos.
    Add {
        /// Mismo espacio util, una caida mas de margen.
        ///
        /// Barato: los fragmentos de datos no se mueven y el volumen se sigue leyendo
        /// mientras tanto. Se puede interrumpir sin consecuencias.
        #[arg(long, conflicts_with = "mas_espacio")]
        mas_seguridad: bool,
        /// Mas capacidad, el mismo margen.
        ///
        /// Caro: cambian las fronteras del reparto, asi que hay que releer y volver a subir
        /// el volumen entero.
        #[arg(long)]
        mas_espacio: bool,
    },
}

#[derive(Subcommand)]
enum ServicioCmd {
    /// Declara LibreDisk ante el lanzador de WinFsp. Necesita administrador.
    ///
    /// A partir de ahi el volumen se monta desde el Explorador como una unidad de red, y
    /// **Windows pide la frase de paso**: no se guarda en ningun sitio.
    Registrar {
        /// Nombre de la clase. Es lo que aparece en `\<nombre>volumen`.
        #[arg(long, default_value = servicio::NOMBRE)]
        nombre: String,
        /// Tamano de la cache de lectura en memoria, en MiB.
        #[arg(long, default_value_t = 512)]
        cache_mb: usize,
        /// Tamano de la cache en disco, en MiB. Con 0, no se usa.
        #[arg(long, default_value_t = 4096)]
        disco_mb: usize,
        /// Montar en lectura y escritura.
        #[arg(long)]
        escritura: bool,
        /// Cada cuantos segundos sube lo pendiente, con `--escritura`.
        #[arg(long, default_value_t = 30)]
        sync_cada: u64,
    },
    /// Quita el registro. No toca el volumen ni sus datos.
    Quitar {
        #[arg(long, default_value = servicio::NOMBRE)]
        nombre: String,
    },
    /// Que hay registrado, y si la configuracion esta donde un servicio pueda verla.
    Estado {
        #[arg(long, default_value = servicio::NOMBRE)]
        nombre: String,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Ficheros donde se busca la configuracion, y cual de ellos existe.
    Ruta,
    /// Conecta una cuenta de nube por el navegador y la guarda.
    ///
    /// Lo mismo que el boton de la ventana, pero contando cada paso. Util cuando la ventana
    /// se queda a medias y no hay donde mirar, y en una maquina sin ventana.
    Conectar {
        /// ONEDRIVE o GDRIVE.
        tipo: String,
    },
    /// Anade un proveedor, leyendo los valores por la entrada estandar.
    ///
    /// Los valores no van por argumentos porque aqui dentro hay tokens y claves secretas, y
    /// lo que se escribe en la linea de ordenes acaba en el historial del interprete y en la
    /// lista de procesos.
    ///
    /// Se prueba contra el proveedor antes de guardar nada.
    Add {
        /// FS, ONEDRIVE, GDRIVE, S3 o WEBDAV.
        tipo: String,
    },
    /// Que claves hay y de donde vienen. **No imprime ningun valor.**
    Ver,
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
        Command::Volume(VolumeCmd::Destroy { confirmar }) => volumen::destroy(confirmar).await,
        Command::Ls { path } => volumen::ls(&path).await,
        Command::Put {
            local,
            remoto,
            diferido,
        } => {
            if diferido {
                volumen::put_diferido(&local, &remoto).await
            } else {
                volumen::put(&local, &remoto).await
            }
        }
        Command::Sync => volumen::sync().await,
        Command::Get { remoto, local } => volumen::get(&remoto, local.as_deref()).await,
        Command::Cat {
            remoto,
            offset,
            len,
        } => volumen::cat(&remoto, offset, len).await,
        Command::Mkdir { path } => volumen::mkdir(&path).await,
        Command::Rm { path } => volumen::rm(&path).await,
        Command::Scrub => volumen::scrub().await,
        Command::Espacio { medir } => volumen::espacio(medir).await,
        Command::Rebuild => volumen::rebuild().await,
        Command::Mount {
            punto,
            cache_mb,
            disco_mb,
            escritura,
            sync_cada,
            desde_lanzador,
        } => {
            // La frase tiene que estar antes de tocar los proveedores: es lo que deriva la
            // clave con la que se abre la cabecera.
            if desde_lanzador {
                volumen::fijar_frase(servicio::frase_del_lanzador()?);
            }
            volumen::mount(
                &punto,
                cache_mb,
                disco_mb,
                escritura,
                sync_cada,
                desde_lanzador,
            )
            .await
        }
        Command::Rescue { adoptar } => volumen::rescue(adoptar.as_deref()).await,
        Command::Header(HeaderCmd::Export { fichero }) => volumen::header_export(&fichero).await,
        Command::Header(HeaderCmd::Import { fichero }) => volumen::header_import(&fichero).await,
        Command::Config(ConfigCmd::Ruta) => configuracion::ruta(),
        Command::Config(ConfigCmd::Ver) => configuracion::ver(),
        Command::Config(ConfigCmd::Add { tipo }) => configuracion::add(&tipo).await,
        Command::Config(ConfigCmd::Conectar { tipo }) => configuracion::conectar(&tipo).await,
        Command::Servicio(ServicioCmd::Registrar {
            nombre,
            cache_mb,
            disco_mb,
            escritura,
            sync_cada,
        }) => servicio::registrar(&nombre, cache_mb, disco_mb, escritura, sync_cada),
        Command::Servicio(ServicioCmd::Quitar { nombre }) => servicio::quitar(&nombre),
        Command::Servicio(ServicioCmd::Estado { nombre }) => servicio::estado(&nombre),
        Command::Providers(ProvidersCmd::List) => volumen::providers_list().await,
        Command::Providers(ProvidersCmd::Add {
            mas_seguridad,
            mas_espacio,
        }) => {
            let cambio = match (mas_seguridad, mas_espacio) {
                (true, _) => libredisk_core::rebalance::Cambio::MasSeguridad,
                (_, true) => libredisk_core::rebalance::Cambio::MasEspacio,
                // Sin elegir no se hace nada: la diferencia entre las dos es de horas a
                // dias, y adivinarla por el usuario seria caro de la peor manera.
                _ => anyhow::bail!(
                    "hay que decir para que se anade el proveedor:

  \n                     --mas-seguridad   mismo espacio util, una caida mas de margen (barato)
  \n                     --mas-espacio     mas capacidad, el mismo margen (reescribe el volumen)

\n                     `libredisk providers list` ensena como esta ahora."
                ),
            };
            volumen::providers_add(cambio).await
        }
        Command::Providers(ProvidersCmd::Remove {
            nombre,
            menos_seguridad,
            menos_espacio,
        }) => {
            let sacrificio = match (menos_seguridad, menos_espacio) {
                (true, _) => libredisk_core::rebalance::Sacrificio::Seguridad,
                (_, true) => libredisk_core::rebalance::Sacrificio::Espacio,
                // Igual que al anadir: la diferencia entre las dos es de minutos a dias, y
                // ademas cambia lo que el volumen promete. No se adivina.
                _ => anyhow::bail!(
                    "hay que decir que se sacrifica al quitar el proveedor:\n\n  \
                     --menos-seguridad   mismo espacio util, un margen de caida menos\n  \
                     --menos-espacio     mismo margen, menos espacio (reescribe el volumen)\n\n\
                     `libredisk providers list` ensena como esta ahora."
                ),
            };
            volumen::providers_remove(&nombre, sacrificio).await
        }
    }
}

async fn check(pasadas: usize) -> Result<()> {
    let proveedores = live::configurados()
        .with_context(|| format!("configuracion de proveedores invalida\n\n{}", live::ayuda()))?;

    if proveedores.is_empty() {
        println!("No hay ningun proveedor configurado.\n");
        println!("{}", live::ayuda());
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
            .join(chunk.id.shard_key(0, i));
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
