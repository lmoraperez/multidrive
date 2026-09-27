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
use multidrive_backend::config::{self, Config};
use multidrive_backend::{catalogo, conformance, live};
use multidrive_core::maleta;

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

    // Si esta copia trae aplicacion registrada. Es lo que decide si al usuario se le pide el
    // ID y el secreto o solo se le pide pulsar un boton, asi que conviene poder verlo sin
    // abrir la ventana.
    println!();
    let con_app: Vec<&str> = catalogo::TIPOS
        .iter()
        .filter(|t| t.oauth.is_some())
        .filter(|t| multidrive_backend::aplicacion::configurada(t.clave).is_some())
        .map(|t| t.etiqueta)
        .collect();
    if con_app.is_empty() {
        println!("Sin aplicacion registrada: para conectar una cuenta de nube hay que pegar");
        println!("el ID y el secreto de una. Ver docs/APLICACION-OAUTH.md.");
    } else {
        println!("Trae aplicacion registrada para: {}", con_app.join(", "));
        println!("Ahi solo hay que pulsar conectar y entrar en la cuenta.");
    }

    match live::configurados() {
        Ok(b) if b.is_empty() => println!("No sale ningun proveedor de aqui.\n\n{}", live::ayuda()),
        Ok(b) => {
            println!("Proveedores que salen de esto ({}):", b.len());
            for backend in &b {
                println!("  {}", multidrive_backend::Backend::name(backend));
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

    if let Ok(Some(otro)) = live::ya_configurada(t, &backend).await {
        bail!(
            "esa cuenta ya esta configurada como '{otro}'.\n\
             Dos proveedores sobre la misma cuenta no son redundancia: se verian como dos,\n\
             pero el dia que se pierda la cuenta se pierden los dos a la vez."
        );
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
    println!("  multidrive providers add --mas-seguridad   una caida mas de margen (barato)");
    println!("  multidrive providers add --mas-espacio     mas capacidad (reescribe el volumen)");
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

/// Acepta el nombre corto (`ROOT`) y el largo (`MULTIDRIVE_FS_ROOT`), porque quien copia de la
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

/// Conecta una cuenta de nube por el navegador y la guarda, contando cada paso.
///
/// Es lo mismo que hace el botón de la ventana. Existe aparte por dos motivos:
///
/// - Un servidor no tiene ventana, y esto se puede ejecutar por SSH con reenvío de
///   navegador, o copiando la dirección a mano cuando falla al abrirlo.
/// - **Se ve lo que pasa.** La ventana no tiene consola detrás, así que cuando algo se
///   queda a medias no hay donde mirar; aquí cada paso dice cuándo empieza y cuánto tardó.
pub async fn conectar(tipo: &str) -> Result<()> {
    let tipo = tipo.to_uppercase();
    let t = catalogo::tipo(&tipo).ok_or_else(|| anyhow!("no conozco el tipo '{tipo}'"))?;
    let oauth = t
        .oauth
        .ok_or_else(|| anyhow!("'{}' no se conecta por navegador", t.etiqueta))?;

    let app = multidrive_backend::aplicacion::configurada(&tipo).ok_or_else(|| {
        anyhow!(
            "esta copia no trae aplicacion registrada para {}. Ver docs/APLICACION-OAUTH.md",
            t.etiqueta
        )
    })?;

    let ranura = live::hueco_libre_configurado(t)?
        .ok_or_else(|| anyhow!("no quedan ranuras libres de tipo {}", t.clave))?;
    let nombre = live::nombre_de_ranura(&ranura);
    println!("Se conectara como '{nombre}'.");
    println!();

    let reloj = std::time::Instant::now();
    println!("[1/4] abriendo el navegador; entra en tu cuenta y da permiso...");
    let conectada = multidrive_backend::oauth::conectar(
        oauth,
        &app.client_id,
        &app.client_secret,
        &multidrive_backend::oauth::abrir_en_navegador,
    )
    .await?;
    println!(
        "      cuenta conectada ({:.1} s)",
        reloj.elapsed().as_secs_f64()
    );

    let valores = |c: &str| match c {
        "REFRESH_TOKEN" => Some(conectada.refresh_token.clone()),
        _ => None,
    };
    let backend = live::construir_uno(t, &ranura, &valores)?;

    let reloj = std::time::Instant::now();
    println!("[2/4] comprobando que escribe, lee y borra...");
    match conformance::prueba_rapida(&backend).await {
        Ok(que) => println!(
            "      {que}\n      ({:.1} s)",
            reloj.elapsed().as_secs_f64()
        ),
        Err(e) => bail!("no se guarda nada porque el proveedor no responde: {e}"),
    }

    let reloj = std::time::Instant::now();
    println!("[3/4] comprobando que no sea una cuenta ya configurada...");
    match live::ya_configurada(t, &backend).await {
        Ok(Some(otro)) => bail!(
            "esa cuenta ya esta configurada como '{otro}'.\n\
             Dos proveedores sobre la misma cuenta no son redundancia: se verian como dos,\n\
             pero el dia que se pierda la cuenta se pierden los dos a la vez."
        ),
        Ok(None) => println!(
            "      es una cuenta nueva ({:.1} s)",
            reloj.elapsed().as_secs_f64()
        ),
        // No poder comprobarlo no bloquea, pero se dice: la proxima cifra de «aguanta perder
        // uno» se habra calculado sin saberlo.
        Err(e) => println!("      AVISO: no se pudo comprobar: {e}"),
    }

    println!("[4/4] guardando...");
    let fichero = config::fichero_de_usuario()
        .context("no se sabe donde vive la configuracion de este usuario")?;
    config::guardar(&fichero, &live::claves_de(t, &ranura, &valores))?;

    println!();
    println!("guardado        {nombre}");
    println!("en              {}", fichero.display());
    Ok(())
}

/// Guarda la configuracion, cifrada, en las nubes del propio usuario.
pub async fn publicar() -> Result<()> {
    let backends = crate::volumen::proveedores()?;
    let claves = maleta::claves_de_este_equipo()?;
    let frase = crate::volumen::frase()?;

    let bulto = maleta::empaquetar(&claves, &frase)?;

    println!(
        "maleta      {} clave(s), {} bytes",
        claves.len(),
        bulto.len()
    );
    println!("            cifrada con tu frase de paso; sin ella no se abre");
    println!();
    println!("guardando en {} proveedor(es)...", backends.len());

    let resultados = maleta::guardar(&backends, &bulto).await;
    let mut logradas = 0;
    for (nombre, fallo) in &resultados {
        match fallo {
            None => {
                logradas += 1;
                println!("  {nombre}: guardada");
            }
            Some(e) => println!("  {nombre}: NO se pudo: {e}"),
        }
    }

    if logradas == 0 {
        bail!("no quedo en ningun proveedor, asi que no hay maleta");
    }

    println!();
    println!("En otro equipo: configura una sola de estas cuentas y ejecuta");
    println!("  multidrive config recuperar");
    println!();
    println!("AVISO: esto acaba de subir tus llaves de acceso a tus propias cuentas. Van");
    println!("cifradas, pero quien entre en una de esas cuentas se lleva el bulto, y lo unico");
    println!("que lo protege es tu frase de paso. Una frase corta aqui es una frase corta");
    println!("delante de alguien que tiene el fichero y todo el tiempo del mundo.");
    Ok(())
}

/// Baja la maleta, la abre y escribe en la configuracion lo que falte.
///
/// Lo que ya esta y coincide no se toca. Lo que ya esta y **no** coincide tampoco: se dice y
/// se deja como estaba, porque pisar una credencial que funciona con otra de una maleta vieja
/// es la forma mas rapida de quedarse sin acceso a una cuenta. Para eso esta `--sobrescribir`.
pub async fn recuperar(sobrescribir: bool) -> Result<()> {
    let backends = crate::volumen::proveedores()?;
    let frase = crate::volumen::frase()?;

    println!("buscando la maleta en {} proveedor(es)...", backends.len());
    let (donde, m) = maleta::recoger(&backends, &frase).await?.ok_or_else(|| {
        anyhow!(
            "no hay ninguna maleta en estos proveedores.\n\n\
                 En el equipo que si tiene la configuracion:\n  multidrive config publicar"
        )
    })?;

    println!("encontrada en '{donde}', {}", antiguedad(m.hecha_en));
    println!();

    let actuales = maleta::claves_de_este_equipo()?;
    let mut nuevas = Vec::new();
    let (mut iguales, mut choques) = (0, 0);

    for (clave, valor) in &m.claves {
        match actuales.get(clave) {
            None => {
                println!("  {clave}  nueva");
                nuevas.push((clave.clone(), valor.clone()));
            }
            Some(v) if v == valor => iguales += 1,
            Some(_) => {
                choques += 1;
                if sobrescribir {
                    println!("  {clave}  DISTINTA, se pisa");
                    nuevas.push((clave.clone(), valor.clone()));
                } else {
                    println!("  {clave}  DISTINTA, se deja la de aqui");
                }
            }
        }
    }

    if iguales > 0 {
        println!("  ({iguales} clave(s) ya estaban y coinciden)");
    }

    if nuevas.is_empty() {
        println!();
        println!("No hay nada que escribir: este equipo ya tiene lo que lleva la maleta.");
        if choques > 0 && !sobrescribir {
            println!("Hay {choques} clave(s) distintas. Para tomar las de la maleta:");
            println!("  multidrive config recuperar --sobrescribir");
        }
        return Ok(());
    }

    let fichero = config::fichero_de_usuario()
        .ok_or_else(|| anyhow!("no se sabe donde guardar la configuracion de este usuario"))?;
    config::guardar(&fichero, &nuevas)?;

    println!();
    println!("escritas    {} clave(s)", nuevas.len());
    println!("en          {}", fichero.display());
    if choques > 0 && !sobrescribir {
        println!();
        println!("Quedan {choques} clave(s) distintas sin tocar. Si las de la maleta son las");
        println!("buenas:  multidrive config recuperar --sobrescribir");
    }
    println!();
    println!("Comprueba con:");
    println!("  multidrive config ver");
    println!("  multidrive volume status");
    Ok(())
}

/// «hace 3 dias», que es lo que uno quiere saber de una maleta: si es la de antes o la de
/// despues del ultimo cambio.
fn antiguedad(hecha_en: u64) -> String {
    let ahora = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if hecha_en == 0 || ahora <= hecha_en {
        return "recien hecha".into();
    }
    let s = ahora - hecha_en;
    match s {
        0..=90 => "hecha hace un momento".into(),
        91..=5400 => format!("hecha hace {} minuto(s)", s / 60),
        5401..=172_800 => format!("hecha hace {} hora(s)", s / 3600),
        _ => format!("hecha hace {} dia(s)", s / 86_400),
    }
}
