//! Ordenes para ver de donde salen las credenciales.
//!
//! Existen por un motivo concreto: cuando un servicio no arranca, el fallo casi nunca es el
//! servicio. Es que esta leyendo un fichero distinto del que se edito, o ninguno. Sin una
//! forma de preguntarselo, eso se depura a ciegas.
//!
//! Ninguna de estas ordenes imprime un valor. Lo que se pega en un informe de fallo es lo que
//! imprime la herramienta de diagnostico, asi que la herramienta de diagnostico no puede
//! imprimir secretos.

use anyhow::{Context, Result};
use libredisk_backend::config::{self, Config};
use libredisk_backend::live;

/// Donde se busca la configuracion, y cual de esos ficheros existe.
pub fn ruta() -> Result<()> {
    let candidatos = config::candidatos();
    println!("Ficheros de configuracion, del mas general al mas especifico.");
    println!("El ultimo que defina una clave es el que gana.\n");

    let mut alguno = false;
    for ruta in &candidatos {
        let existe = ruta.is_file();
        alguno |= existe;
        println!(
            "  {}  {}",
            if existe { "[si]" } else { "[no]" },
            ruta.display()
        );
    }

    println!("\nPor encima de todos ellos manda el entorno del proceso.");
    if !alguno {
        println!(
            "\nNo hay ninguno. Mientras la configuracion viva solo en el entorno, el volumen\n\
             solo existe para quien haya exportado las variables: un servicio no vera nada."
        );
        if let Some(f) = config::fichero_de_maquina() {
            println!(
                "\nEl de la maquina, que es el que lee un servicio, iria en:\n  {}",
                f.display()
            );
        }
    }
    Ok(())
}

/// Que claves hay configuradas y de donde vienen. **Sin los valores.**
pub fn ver() -> Result<()> {
    let config = Config::cargar().context("no se pudo leer la configuracion")?;

    if config.origenes().is_empty() {
        println!("Ningun fichero de configuracion. Solo cuenta el entorno.");
    } else {
        println!("Ficheros leidos, en orden:");
        for ruta in config.origenes() {
            println!("  {}", ruta.display());
        }
    }

    let del_fichero: Vec<&str> = config.claves().collect();
    if del_fichero.is_empty() {
        println!("\nNinguna clave en fichero.");
    } else {
        println!("\nClaves en fichero ({}):", del_fichero.len());
        for clave in &del_fichero {
            // El entorno gana, asi que conviene decir cuando una clave del fichero esta
            // siendo ignorada: es la forma mas facil de editar el fichero correcto y que
            // aun asi no sirva de nada.
            let tapada = std::env::var(*clave).is_ok_and(|v| !v.trim().is_empty());
            println!(
                "  {clave}{}",
                if tapada {
                    "   (tapada por el entorno)"
                } else {
                    ""
                }
            );
        }
    }

    println!();
    match live::configurados() {
        Ok(b) if b.is_empty() => println!("No sale ningun proveedor de aqui.\n\n{}", live::AYUDA),
        Ok(b) => {
            println!("Proveedores que salen de esto ({}):", b.len());
            for backend in &b {
                println!("  {}", libredisk_backend::Backend::name(backend));
            }
        }
        Err(e) => println!("La configuracion no es valida: {e}"),
    }
    Ok(())
}
