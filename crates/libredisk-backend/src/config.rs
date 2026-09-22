//! De donde salen las credenciales cuando no hay un interprete que las exporte.
//!
//! Hasta ahora la configuracion vivia solo en el entorno del proceso, que es lo correcto
//! cuando quien lanza la orden es una persona en una consola: exporta las variables, ejecuta
//! y las variables se van con la sesion.
//!
//! Un servicio no tiene consola ni sesion. Arranca como LocalSystem, con el entorno de la
//! maquina, y ahi no hay ningun `.env` de nadie. Sin un fichero de configuracion no hay
//! servicio posible, y sin servicio el volumen solo existe mientras alguien tenga una consola
//! abierta.
//!
//! El formato es deliberadamente aburrido —`CLAVE=valor`, una por linea— y las claves son las
//! mismas que las variables de entorno. Asi el fichero se escribe copiando el `.env` que ya
//! se tenia, sin traducir nada.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::BackendError;

/// Nombre del fichero, igual en todos los sistemas.
const FICHERO: &str = "libredisk.conf";

/// Configuracion leida de disco.
///
/// Guarda de que ficheros salio cada cosa, no por registro sino porque el sintoma mas comun
/// de una mala configuracion es *no saber cual de los ficheros esta ganando*.
#[derive(Debug, Default, Clone)]
pub struct Config {
    valores: BTreeMap<String, String>,
    origenes: Vec<PathBuf>,
}

impl Config {
    /// Lee los ficheros candidatos que existan, en orden.
    ///
    /// Un fichero que no existe no es un error: casi nadie tendra todos. Uno que existe y no
    /// se puede leer o no se entiende, si.
    pub fn cargar() -> Result<Self, BackendError> {
        Self::desde(&candidatos())
    }

    /// Igual, pero leyendo las rutas que se le digan. Existe para poder probarlo.
    pub fn desde(rutas: &[PathBuf]) -> Result<Self, BackendError> {
        let mut config = Self::default();
        for ruta in rutas {
            if !ruta.is_file() {
                continue;
            }
            let texto =
                std::fs::read_to_string(ruta).map_err(|e| BackendError::FicheroDeConfig {
                    fichero: ruta.display().to_string(),
                    detalle: e.to_string(),
                })?;
            config.absorber(&texto, ruta)?;
            config.origenes.push(ruta.clone());
        }
        Ok(config)
    }

    /// Mezcla un fichero sobre lo que ya hubiera. El ultimo en llegar gana.
    fn absorber(&mut self, texto: &str, ruta: &Path) -> Result<(), BackendError> {
        // El Bloc de notas y PowerShell escriben UTF-8 **con marca de orden de bytes**, y es
        // con lo que la gente va a editar esto en Windows. Sin quitarla, la primera linea del
        // fichero nunca casa y el error apunta a una linea que se ve perfecta.
        let texto = texto.strip_prefix('\u{feff}').unwrap_or(texto);
        for (n, linea) in texto.lines().enumerate() {
            let linea = linea.trim();
            // Se admite `export CLAVE=valor` para poder copiar un `.env` tal cual.
            let linea = linea.strip_prefix("export ").unwrap_or(linea);
            if linea.is_empty() || linea.starts_with('#') {
                continue;
            }
            let Some((clave, valor)) = linea.split_once('=') else {
                return Err(BackendError::FicheroDeConfig {
                    fichero: ruta.display().to_string(),
                    detalle: format!("linea {}: no tiene la forma CLAVE=valor", n + 1),
                });
            };
            let clave = clave.trim();
            if clave.is_empty() {
                return Err(BackendError::FicheroDeConfig {
                    fichero: ruta.display().to_string(),
                    detalle: format!("linea {}: la clave esta vacia", n + 1),
                });
            }
            self.valores
                .insert(clave.to_string(), desentrecomillar(valor.trim()));
        }
        Ok(())
    }

    /// El valor de una clave, si esta y no esta vacio.
    pub fn get(&self, clave: &str) -> Option<String> {
        self.valores
            .get(clave)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// Los ficheros de los que salio algo, en el orden en que se leyeron.
    pub fn origenes(&self) -> &[PathBuf] {
        &self.origenes
    }

    /// Las claves que trae, **sin los valores**.
    ///
    /// Para poder diagnosticar una configuracion sin volcar secretos a la consola, que es lo
    /// que uno acaba pegando en un informe de fallo.
    pub fn claves(&self) -> impl Iterator<Item = &str> {
        self.valores.keys().map(String::as_str)
    }
}

/// Quita las comillas que envuelvan a un valor, si lo envuelven enteras.
///
/// `CLAVE="con espacios"` es lo que uno escribe por costumbre al venir de un `.env`. No se
/// interpretan escapes dentro: un secreto con una comilla en medio se guarda tal cual.
fn desentrecomillar(valor: &str) -> String {
    for comilla in ['"', '\''] {
        if valor.len() >= 2 && valor.starts_with(comilla) && valor.ends_with(comilla) {
            return valor[1..valor.len() - 1].to_string();
        }
    }
    valor.to_string()
}

/// Los ficheros donde se busca la configuracion, **del mas general al mas especifico**.
///
/// El orden es el que decide quien gana: lo del usuario pisa lo de la maquina, y
/// `LIBREDISK_CONFIG` pisa a los dos, porque quien lo pone esta diciendo exactamente lo que
/// quiere.
///
/// No hay ni un `cfg(windows)` aqui, y no es casualidad: todo son consultas al entorno. En
/// Windows existen `ProgramData` y `APPDATA` y no existe `XDG_CONFIG_HOME`; en Linux al
/// reves. Cada sistema encuentra los suyos y no ve los del otro, sin que el codigo tenga que
/// saber en cual esta.
pub fn candidatos() -> Vec<PathBuf> {
    let mut rutas = Vec::new();
    let var = |clave: &str| std::env::var(clave).ok().filter(|v| !v.trim().is_empty());

    // De la maquina: lo que lee un servicio. Son alternativas, no una lista: en Windows
    // `/etc` no existe, y ofrecerlo solo ensucia el diagnostico con una ruta imposible.
    match var("ProgramData") {
        Some(dir) => rutas.push(PathBuf::from(dir).join("LibreDisk").join(FICHERO)),
        None => rutas.push(PathBuf::from("/etc/libredisk").join(FICHERO)),
    }

    // Del usuario.
    if let Some(dir) = var("XDG_CONFIG_HOME") {
        rutas.push(PathBuf::from(dir).join("libredisk").join(FICHERO));
    } else if let Some(dir) = var("HOME") {
        rutas.push(
            PathBuf::from(dir)
                .join(".config")
                .join("libredisk")
                .join(FICHERO),
        );
    }
    if let Some(dir) = var("APPDATA") {
        rutas.push(PathBuf::from(dir).join("LibreDisk").join(FICHERO));
    }

    // Lo que se pida a mano.
    if let Some(ruta) = var("LIBREDISK_CONFIG") {
        rutas.push(PathBuf::from(ruta));
    }

    rutas
}

/// La carpeta donde escribir la configuracion de la **maquina**, que es la que ve un servicio.
pub fn carpeta_de_maquina() -> Option<PathBuf> {
    std::env::var("ProgramData")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|d| PathBuf::from(d).join("LibreDisk"))
        .or_else(|| Some(PathBuf::from("/etc/libredisk")))
}

/// El fichero de configuracion de la maquina.
pub fn fichero_de_maquina() -> Option<PathBuf> {
    carpeta_de_maquina().map(|d| d.join(FICHERO))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(nombre: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "libredisk-config-{}-{nombre}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn escribir(d: &Path, nombre: &str, contenido: &str) -> PathBuf {
        let ruta = d.join(nombre);
        std::fs::write(&ruta, contenido).unwrap();
        ruta
    }

    /// El Bloc de notas y PowerShell escriben UTF-8 con marca de orden de bytes. Si no se
    /// quita, la primera linea del fichero no casa nunca y el error senala una linea que a
    /// simple vista esta bien: se pierde media tarde buscando un caracter invisible.
    #[test]
    fn una_marca_de_orden_de_bytes_no_rompe_la_primera_linea() {
        let d = dir("bom");
        let f = escribir(
            &d,
            "a.conf",
            "\u{feff}# comentario\nLIBREDISK_FS_ROOT=/datos\n",
        );
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(c.get("LIBREDISK_FS_ROOT").as_deref(), Some("/datos"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn la_marca_tampoco_rompe_una_clave_en_la_primera_linea() {
        let d = dir("bom-clave");
        let f = escribir(&d, "a.conf", "\u{feff}LIBREDISK_FS_ROOT=/datos\n");
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(c.get("LIBREDISK_FS_ROOT").as_deref(), Some("/datos"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lee_claves_y_valores() {
        let d = dir("basico");
        let f = escribir(&d, "a.conf", "LIBREDISK_GDRIVE_ROOT=/volumen\n");
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(c.get("LIBREDISK_GDRIVE_ROOT").as_deref(), Some("/volumen"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Un `.env` se copia tal cual: con `export`, con comentarios y con comillas.
    #[test]
    fn digiere_un_env_sin_traducirlo() {
        let d = dir("env");
        let f = escribir(
            &d,
            "a.conf",
            "# credenciales\n\
             export LIBREDISK_GDRIVE_CLIENT_ID=\"un-id\"\n\
             \n\
             LIBREDISK_GDRIVE_ROOT='/con espacios'\n",
        );
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(
            c.get("LIBREDISK_GDRIVE_CLIENT_ID").as_deref(),
            Some("un-id")
        );
        assert_eq!(
            c.get("LIBREDISK_GDRIVE_ROOT").as_deref(),
            Some("/con espacios")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Un secreto puede llevar comillas dentro, y no se tocan.
    #[test]
    fn solo_quita_las_comillas_que_envuelven() {
        let d = dir("comillas");
        let f = escribir(&d, "a.conf", "A=un\"valor\"raro\nB=\"entero\"\n");
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(c.get("A").as_deref(), Some("un\"valor\"raro"));
        assert_eq!(c.get("B").as_deref(), Some("entero"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// El `=` del valor no parte la linea: los tokens en base64 llevan `=` al final.
    #[test]
    fn el_valor_puede_llevar_iguales() {
        let d = dir("iguales");
        let f = escribir(&d, "a.conf", "T=abc=def==\n");
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(c.get("T").as_deref(), Some("abc=def=="));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn el_ultimo_fichero_gana() {
        let d = dir("orden");
        let a = escribir(&d, "a.conf", "R=/de-la-maquina\nS=solo-en-a\n");
        let b = escribir(&d, "b.conf", "R=/del-usuario\n");
        let c = Config::desde(&[a, b]).unwrap();
        assert_eq!(c.get("R").as_deref(), Some("/del-usuario"));
        assert_eq!(
            c.get("S").as_deref(),
            Some("solo-en-a"),
            "lo que el segundo no menciona no se borra"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn un_fichero_que_no_existe_no_es_un_error() {
        let c = Config::desde(&[PathBuf::from("/no/existe/libredisk.conf")]).unwrap();
        assert!(c.origenes().is_empty());
    }

    /// Pero uno que existe y esta mal, si. Callarselo seria dejar el volumen a medio
    /// configurar sin decirlo, que es como se acaba escribiendo sin la redundancia que uno
    /// cree tener.
    #[test]
    fn una_linea_sin_igual_es_un_error_que_dice_donde() {
        let d = dir("malo");
        let f = escribir(&d, "a.conf", "bien=1\nesto no es una asignacion\n");
        let e = Config::desde(&[f]).unwrap_err().to_string();
        assert!(e.contains("linea 2"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn una_clave_vacia_es_un_error() {
        let d = dir("vacia");
        let f = escribir(&d, "a.conf", "=algo\n");
        assert!(Config::desde(&[f]).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn una_clave_sin_valor_es_como_si_no_estuviera() {
        let d = dir("sin-valor");
        let f = escribir(&d, "a.conf", "A=\nB=   \n");
        let c = Config::desde(&[f]).unwrap();
        assert!(c.get("A").is_none());
        assert!(c.get("B").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn las_claves_se_pueden_listar_sin_los_valores() {
        let d = dir("claves");
        let f = escribir(&d, "a.conf", "SECRETO=no-me-imprimas\nOTRA=1\n");
        let c = Config::desde(&[f]).unwrap();
        let claves: Vec<_> = c.claves().collect();
        assert_eq!(claves, vec!["OTRA", "SECRETO"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
