//! Ordenes para ver de donde salen las credenciales.
//!
//! Existen por un motivo concreto: cuando un servicio no arranca, el fallo casi nunca es el
//! servicio. Es que esta leyendo un fichero distinto del que se edito, o ninguno. Sin una
//! forma de preguntarselo, eso se depura a ciegas.
//!
//! Ninguna de estas ordenes imprime un valor. Lo que se pega en un informe de fallo es lo que
//! imprime la herramienta de diagnostico, asi que la herramienta de diagnostico no puede
//! imprimir secretos.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use libredisk_backend::config::{self, Config};
use libredisk_backend::{catalogo, conformance, live};

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
        Ok(b) if b.is_empty() => println!("No sale ningun proveedor de aqui.\n\n{}", live::ayuda()),
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

/// Anade un proveedor a la configuracion del usuario, leyendo los valores de la entrada.
///
/// Los valores van por la entrada estandar y no por argumentos, y eso no es una rareza: aqui
/// dentro hay tokens de refresco y claves secretas, y **lo que se escribe en la linea de
/// ordenes acaba en el historial del interprete y en la lista de procesos**. Es la misma
/// regla que sigue la frase de paso.
///
/// Prueba el proveedor antes de escribir nada. Guardar primero y descubrir despues que las
/// credenciales estaban mal deja la configuracion con un proveedor que no responde, y a
/// partir de ahi todo lo que el programa enumere o mida sale con un hueco que hay que ir a
/// explicar.
pub async fn add(tipo: &str) -> Result<()> {
    let tipo = tipo.to_uppercase();
    let t = catalogo::tipo(&tipo).ok_or_else(|| {
        anyhow!(
            "no conozco el tipo '{tipo}'. Los que hay: {}",
            catalogo::TIPOS
                .iter()
                .map(|t| t.clave)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    let valores = leer_pares()?;
    if let Err(fallos) = catalogo::validar(t, &|c| valores.get(c).cloned()) {
        bail!(
            "faltan datos para '{}':\n  {}\n\nSe esperan lineas CLAVE=VALOR por la entrada. \
             Claves de este tipo: {}",
            t.etiqueta,
            fallos.join("\n  "),
            t.campos
                .iter()
                .map(|c| c.clave)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let ranura = live::hueco_libre_configurado(t)?
        .ok_or_else(|| anyhow!("no quedan ranuras libres de tipo {}", t.clave))?;
    let nombre = live::nombre_de_ranura(&ranura);

    println!("Probando '{nombre}' antes de guardar nada...");
    let backend = live::construir_uno(t, &ranura, &|c| valores.get(c).cloned())?;
    match conformance::prueba_rapida(&backend).await {
        Ok(que) => println!("  {que}"),
        Err(e) => bail!("no se guarda nada porque el proveedor no responde: {e}"),
    }

    let fichero = config::fichero_de_usuario()
        .context("no se sabe donde vive la configuracion de este usuario")?;
    config::guardar(
        &fichero,
        &live::claves_de(t, &ranura, &|c| valores.get(c).cloned()),
    )?;

    println!();
    println!("guardado        {nombre}");
    println!("en              {}", fichero.display());
    println!();
    println!("Esto lo deja CONFIGURADO. Si ya hay un volumen, no lo mete dentro: para eso hay");
    println!("que mover fragmentos, y hay que elegir para que.");
    println!("  libredisk providers add --mas-seguridad   una caida mas de margen (barato)");
    println!("  libredisk providers add --mas-espacio     mas capacidad (reescribe el volumen)");
    Ok(())
}

/// Lee lineas `CLAVE=VALOR` de la entrada estandar.
///
/// Se ignoran las vacias y las que empiezan por `#`, para poder pasarle un trozo de fichero
/// tal cual.
fn leer_pares() -> Result<HashMap<String, String>> {
    use std::io::BufRead;

    let mut out = HashMap::new();
    for linea in std::io::stdin().lock().lines() {
        let linea = linea.context("no se pudo leer la entrada")?;
        let l = linea.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let (clave, valor) = l
            .split_once('=')
            .with_context(|| format!("esto no es CLAVE=VALOR: {l:?}"))?;
        out.insert(nombre_corto(clave.trim()), valor.trim().to_string());
    }
    Ok(out)
}

/// Acepta el nombre corto (`ROOT`) y el largo (`LIBREDISK_FS_ROOT`), porque quien copia de la
/// salida de `config ver` tiene el largo delante.
///
/// Se busca el sufijo **mas largo** del catalogo que encaje, y no el trozo detras del ultimo
/// guion bajo: `SECRET_ACCESS_KEY` lleva guiones dentro, y quedarse con `KEY` lo convertiria
/// en un campo que no existe.
fn nombre_corto(clave: &str) -> String {
    catalogo::TIPOS
        .iter()
        .flat_map(|t| t.campos)
        .filter(|c| clave == c.clave || clave.ends_with(&format!("_{}", c.clave)))
        .max_by_key(|c| c.clave.len())
        .map(|c| c.clave.to_string())
        .unwrap_or_else(|| clave.to_string())
}
