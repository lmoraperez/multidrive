//! Registro de LibreDisk como sistema de ficheros del lanzador de WinFsp.
//!
//! No se escribe un servicio de Windows propio, y no por ahorrar trabajo: **WinFsp ya trae
//! uno**. `WinFsp.Launcher` arranca con la maquina, corre como LocalSystem y sabe lanzar
//! sistemas de ficheros bajo demanda. Escribir otro al lado seria duplicar un servicio que ya
//! esta instalado, firmado y depurado.
//!
//! Lo que hay que hacer es declararse ante el: una clave en el registro que diga que
//! ejecutable lanzar y con que argumentos.
//!
//! # El problema de la frase de paso
//!
//! Un volumen de conocimiento cero no se puede montar solo al arrancar. Para descifrarlo hace
//! falta la frase, y si la maquina la sabe sin que nadie la escriba, es que esta guardada en
//! la maquina: el cifrado seguiria protegiendo frente a los proveedores, pero no frente a
//! quien se lleve el equipo.
//!
//! El lanzador resuelve exactamente esto, y por eso no hace falta inventar nada. Con
//! `Credentials=1`, Windows **pide la contrasena al conectar la unidad**, se la pasa al
//! lanzador, y el lanzador nos la entrega por la **entrada estandar**. No pasa por la linea
//! de ordenes, ni por el registro, ni por una variable de entorno: los tres sitios donde
//! acaba siendo legible por otros procesos.
//!
//! Si el usuario marca «recordar», la guarda el Gestor de credenciales de Windows. Eso es una
//! decision suya, tomada en un dialogo del sistema, no una politica nuestra.
//!
//! # El protocolo
//!
//! Leido del codigo del lanzador, no supuesto:
//!
//! 1. El lanzador escribe la frase en nuestra entrada estandar, en UTF-8 y **sin salto de
//!    linea**, y cierra la entrada.
//! 2. Nosotros tenemos **15 segundos** para responder exactamente `OK` por la salida
//!    estandar. Cualquier otra cosa, o el silencio, se interpreta como contrasena incorrecta.
//! 3. A partir de ahi el lanzador cierra su extremo de la tuberia y deja de leernos.
//!
//! El paso 3 es el que obliga a que el montaje no vuelva a escribir en la salida estandar:
//! ver [`crate::volumen::mount`].

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// Nombre por defecto de la clase de sistema de ficheros.
///
/// Es lo que aparece en la ruta de red: `\\libredisk\<instancia>`.
pub const NOMBRE: &str = "libredisk";

/// Donde el lanzador busca los sistemas de ficheros registrados.
///
/// Va en la vista de **32 bits** del registro aunque todo sea de 64: lo puso asi el propio
/// instalador de WinFsp, y su lanzador lee de ahi. Escribir en la vista de 64 dejaria una
/// clave que nadie lee, que es peor que no escribir ninguna.
const CLAVE: &str = r"SOFTWARE\WinFsp\Services";

/// Quien puede arrancar, parar y consultar el sistema de ficheros.
///
/// `SY` es LocalSystem, `BA` los administradores e `IU` quien haya iniciado sesion de forma
/// interactiva. Los ejemplos de WinFsp usan `WD` —todo el mundo—, que incluye tambien las
/// sesiones de red y la cuenta anonima. Arrancar sin la frase no sirve de nada, pero
/// **parar** si: con `WD`, cualquiera que llegue por la red podria desmontarle el volumen al
/// que lo esta usando.
const PERMISOS: &str = "D:P(A;;RPWPLC;;;SY)(A;;RPWPLC;;;BA)(A;;RPWPLC;;;IU)";

/// Lo que se registra para una clase de sistema de ficheros.
struct Registro {
    ejecutable: PathBuf,
    ordenes: String,
    registro_de_errores: Option<PathBuf>,
}

#[cfg(windows)]
mod registro {
    use anyhow::{Context, Result, bail};
    use winreg::RegKey;
    use winreg::enums::{
        HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    };

    use super::{CLAVE, PERMISOS, Registro};

    /// Traduce el «acceso denegado» a lo que de verdad hay que hacer.
    ///
    /// La clave esta bajo HKLM: sin elevacion no hay forma. Un `os error 5` pelado deja a
    /// quien lo lee buscando en el sitio equivocado.
    fn explicar(e: std::io::Error, que: &str) -> anyhow::Error {
        if e.raw_os_error() == Some(5) {
            anyhow::anyhow!(
                "no se pudo {que}: hace falta ejecutar esto como administrador.\n\
                 La clave vive en HKLM, que es la del equipo entero, no la de tu sesion."
            )
        } else {
            anyhow::Error::new(e).context(format!("no se pudo {que}"))
        }
    }

    pub fn escribir(nombre: &str, r: &Registro) -> Result<()> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let (clave, _) = hklm
            .create_subkey_with_flags(
                format!("{CLAVE}\\{nombre}"),
                KEY_WRITE | KEY_WOW64_32KEY | REG_OPTION_NON_VOLATILE,
            )
            .map_err(|e| explicar(e, "crear la clave del registro"))?;

        clave
            .set_value("Executable", &r.ejecutable.as_os_str())
            .and_then(|_| clave.set_value("CommandLine", &r.ordenes))
            .and_then(|_| clave.set_value("Security", &PERMISOS))
            // Con control de trabajo, si el lanzador se para se lleva por delante el
            // montaje. Sin el quedaria un proceso huerfano sirviendo una unidad que el
            // sistema ya da por desmontada.
            .and_then(|_| clave.set_value("JobControl", &1u32))
            // 1 = pedir contrasena y pasarla por la entrada estandar.
            .and_then(|_| clave.set_value("Credentials", &1u32))
            .map_err(|e| explicar(e, "escribir los valores del registro"))?;

        match &r.registro_de_errores {
            Some(f) => clave
                .set_value("Stderr", &f.as_os_str())
                .map_err(|e| explicar(e, "apuntar el registro de errores"))?,
            None => {
                let _ = clave.delete_value("Stderr");
            }
        }
        Ok(())
    }

    pub fn borrar(nombre: &str) -> Result<()> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let servicios = hklm
            .open_subkey_with_flags(CLAVE, KEY_WRITE | KEY_WOW64_32KEY)
            .map_err(|e| explicar(e, "abrir la clave de servicios de WinFsp"))?;
        match servicios.delete_subkey_all(nombre) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("'{nombre}' no estaba registrado")
            }
            Err(e) => Err(explicar(e, "borrar la clave del registro")),
        }
    }

    /// Lo registrado, si lo hay. Solo lectura: no necesita elevacion.
    pub fn leer(nombre: &str) -> Result<Option<Vec<(String, String)>>> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let clave = match hklm
            .open_subkey_with_flags(format!("{CLAVE}\\{nombre}"), KEY_READ | KEY_WOW64_32KEY)
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(explicar(e, "leer la clave del registro")),
        };
        let mut valores = Vec::new();
        for (nombre, valor) in clave.enum_values().flatten() {
            valores.push((nombre, valor.to_string()));
        }
        Ok(Some(valores))
    }

    /// Si WinFsp esta instalado, donde.
    pub fn winfsp() -> Result<Option<std::path::PathBuf>> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let clave =
            match hklm.open_subkey_with_flags(r"SOFTWARE\WinFsp", KEY_READ | KEY_WOW64_32KEY) {
                Ok(c) => c,
                Err(_) => return Ok(None),
            };
        let dir: String = clave
            .get_value("InstallDir")
            .context("WinFsp sin InstallDir")?;
        Ok(Some(std::path::PathBuf::from(dir)))
    }
}

#[cfg(not(windows))]
mod registro {
    use anyhow::{Result, bail};

    use super::Registro;

    const SOLO_WINDOWS: &str = "el lanzador de WinFsp solo existe en Windows";

    pub fn escribir(_nombre: &str, _r: &Registro) -> Result<()> {
        bail!(SOLO_WINDOWS)
    }
    pub fn borrar(_nombre: &str) -> Result<()> {
        bail!(SOLO_WINDOWS)
    }
    pub fn leer(_nombre: &str) -> Result<Option<Vec<(String, String)>>> {
        bail!(SOLO_WINDOWS)
    }
    pub fn winfsp() -> Result<Option<std::path::PathBuf>> {
        Ok(None)
    }
}

/// Registra LibreDisk en el lanzador de WinFsp.
pub fn registrar(
    nombre: &str,
    cache_mb: usize,
    disco_mb: usize,
    escritura: bool,
    sync_cada: u64,
) -> Result<()> {
    if registro::winfsp()?.is_none() {
        bail!(
            "no se encuentra WinFsp. Instalalo desde https://winfsp.dev antes de registrar\n\
             el servicio: quien monta la unidad es su controlador, no nosotros."
        );
    }

    let ejecutable = std::env::current_exe().context("no se sabe donde esta este ejecutable")?;
    if ejecutable.to_string_lossy().contains("\\target\\") {
        eprintln!(
            "AVISO: se va a registrar el binario de compilacion:\n  {}\n\
             Si borras ese directorio, el servicio dejara de arrancar.",
            ejecutable.display()
        );
    }

    // `%2` es la letra de unidad que pide quien conecta; `%1` seria la ruta de red. La frase
    // de paso **no** aparece aqui, y ese es justo el motivo de usar el lanzador.
    let mut ordenes =
        format!("mount %2 --desde-lanzador --cache-mb {cache_mb} --disco-mb {disco_mb}");
    if escritura {
        ordenes.push_str(&format!(" --escritura --sync-cada {sync_cada}"));
    }

    let registro_de_errores =
        libredisk_backend::config::carpeta_de_maquina().map(|d| d.join(format!("{nombre}.log")));
    if let Some(f) = &registro_de_errores
        && let Some(padre) = f.parent()
    {
        // Un servicio que falla sin dejar rastro es un servicio imposible de arreglar. El
        // lanzador redirige nuestra salida de error a este fichero.
        std::fs::create_dir_all(padre)
            .with_context(|| format!("no se pudo crear {}", padre.display()))?;
    }

    registro::escribir(
        nombre,
        &Registro {
            ejecutable: ejecutable.clone(),
            ordenes: ordenes.clone(),
            registro_de_errores: registro_de_errores.clone(),
        },
    )?;

    println!("registrado como '{nombre}'");
    println!("  ejecutable   {}", ejecutable.display());
    println!("  ordenes      {ordenes}");
    if let Some(f) = &registro_de_errores {
        println!("  errores en   {}", f.display());
    }
    println!();
    println!("Para montar, desde el Explorador: Conectar a unidad de red");
    println!("  carpeta      \\\\{nombre}\\volumen");
    println!("  contrasena   la frase de paso del volumen");
    println!();
    println!("O desde una consola:");
    println!("  net use Z: \\\\{nombre}\\volumen");
    println!();
    println!("Las credenciales de los proveedores tienen que estar donde las vea un servicio:");
    println!("  libredisk config ruta");
    Ok(())
}

/// Quita el registro. No toca ni el volumen ni la configuracion.
pub fn quitar(nombre: &str) -> Result<()> {
    registro::borrar(nombre)?;
    println!("'{nombre}' ya no esta registrado");
    println!("El volumen y sus datos no se han tocado: esto solo era la forma de montarlo.");
    Ok(())
}

/// Que hay registrado, y si la configuracion esta donde un servicio pueda verla.
pub fn estado(nombre: &str) -> Result<()> {
    match registro::winfsp()? {
        Some(d) => println!("WinFsp      instalado en {}", d.display()),
        None => println!("WinFsp      NO INSTALADO (https://winfsp.dev)"),
    }

    match registro::leer(nombre)? {
        None => {
            println!("registro    '{nombre}' no esta registrado");
            println!("\n  libredisk servicio registrar");
        }
        Some(valores) => {
            println!("registro    '{nombre}'");
            for (clave, valor) in valores {
                println!("  {clave:<12} {valor}");
            }
        }
    }

    // Lo que mas veces rompe un servicio recien registrado: la configuracion esta en el
    // `.env` de una consola, que LocalSystem no ve.
    println!();
    let maquina = libredisk_backend::config::fichero_de_maquina();
    match &maquina {
        Some(f) if f.is_file() => println!("config      {} (la vera el servicio)", f.display()),
        Some(f) => {
            println!("config      NO EXISTE {}", f.display());
            println!(
                "\n  Un servicio arranca como LocalSystem y no ve tu `.env`. Sin este fichero\n\
                 no encontrara ningun proveedor y el montaje fallara. Ver `libredisk config ruta`."
            );
        }
        None => println!("config      no se sabe donde iria el fichero de la maquina"),
    }
    Ok(())
}

/// Lee la frase de paso que el lanzador deja en la entrada estandar.
///
/// Se lee hasta el final, no hasta un salto de linea: el lanzador no manda ninguno, escribe la
/// frase y cierra. Esperar un `\n` seria esperar para siempre.
pub fn frase_del_lanzador() -> Result<String> {
    use std::io::Read;

    let mut buf = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buf)
        .context("no se pudo leer la frase de paso de la entrada estandar")?;
    let frase = String::from_utf8(buf).context("la frase de paso no es UTF-8 valido")?;
    // Se recorta por si alguien la pasa a mano con un salto al final; el lanzador no lo pone.
    let frase = frase.trim_end_matches(['\r', '\n']).to_string();
    if frase.is_empty() {
        bail!("el lanzador no mando ninguna frase de paso");
    }
    Ok(frase)
}

/// Responde al lanzador que la frase era buena.
///
/// Son exactamente dos bytes, y son los **primeros** que salen por ahi: el lanzador lee dos y
/// compara. Cualquier cosa impresa antes se leeria en su lugar y la respuesta seria «no».
pub fn confirmar_al_lanzador() -> Result<()> {
    use std::io::Write;

    let salida = std::io::stdout();
    let mut salida = salida.lock();
    salida
        .write_all(b"OK")
        .and_then(|()| salida.flush())
        .context("no se pudo responder al lanzador")
}
