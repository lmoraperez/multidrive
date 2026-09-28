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
const FICHERO: &str = "multidrive.conf";

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
/// `MULTIDRIVE_CONFIG` pisa a los dos, porque quien lo pone esta diciendo exactamente lo que
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
        Some(dir) => rutas.push(PathBuf::from(dir).join("MultiDrive").join(FICHERO)),
        None => rutas.push(PathBuf::from("/etc/multidrive").join(FICHERO)),
    }

    // Del usuario.
    if let Some(dir) = var("XDG_CONFIG_HOME") {
        rutas.push(PathBuf::from(dir).join("multidrive").join(FICHERO));
    } else if let Some(dir) = var("HOME") {
        rutas.push(
            PathBuf::from(dir)
                .join(".config")
                .join("multidrive")
                .join(FICHERO),
        );
    }
    if let Some(dir) = var("APPDATA") {
        rutas.push(PathBuf::from(dir).join("MultiDrive").join(FICHERO));
    }

    // Lo que se pida a mano.
    if let Some(ruta) = var("MULTIDRIVE_CONFIG") {
        rutas.push(PathBuf::from(ruta));
    }

    rutas
}

/// El fichero de configuracion **del usuario**, que es donde se escriben las credenciales.
///
/// No es el de la maquina, y la diferencia no es de gusto: el de la maquina vive en
/// `ProgramData` o en `/etc`, que cualquier usuario del equipo puede leer. Ahi dentro hay
/// tokens de refresco y claves de S3, o sea acceso permanente a las cuentas. Un servicio
/// necesita ese fichero para arrancar sin sesion, pero eso es una decision que hay que tomar
/// a sabiendas y a mano: **nada de lo que escribe MultiDrive solo va alli**.
pub fn fichero_de_usuario() -> Option<PathBuf> {
    let var = |clave: &str| std::env::var(clave).ok().filter(|v| !v.trim().is_empty());
    // `MULTIDRIVE_CONFIG` gana, igual que al leer: quien la pone esta diciendo exactamente
    // que fichero quiere, y seria raro que se leyera de uno y se escribiera en otro.
    if let Some(ruta) = var("MULTIDRIVE_CONFIG") {
        return Some(PathBuf::from(ruta));
    }
    if let Some(dir) = var("APPDATA") {
        return Some(PathBuf::from(dir).join("MultiDrive").join(FICHERO));
    }
    if let Some(dir) = var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("multidrive").join(FICHERO));
    }
    var("HOME").map(|d| {
        PathBuf::from(d)
            .join(".config")
            .join("multidrive")
            .join(FICHERO)
    })
}

/// Escribe o actualiza claves en un fichero de configuracion, dejando lo demas como estaba.
///
/// Conserva comentarios, orden y espaciado: el fichero es de quien lo escribe, y una
/// herramienta que lo reformatea a su gusto cada vez que toca una clave convierte cualquier
/// anotacion en algo que no merece la pena escribir.
///
/// Se escribe a un temporal y se renombra, igual que el diario de escritura y que `get`: si
/// el proceso muere a mitad, queda el fichero de antes y no uno truncado. Perder la
/// configuracion de todos los proveedores por un corte de luz seria caro de recuperar.
///
/// En Unix nace con permisos `0600`. Lo que se guarda aqui son tokens de refresco y claves
/// secretas, o sea acceso permanente a las cuentas del usuario.
pub fn guardar(ruta: &Path, nuevas: &[(String, String)]) -> Result<(), BackendError> {
    let fallo = |detalle: String| BackendError::FicheroDeConfig {
        fichero: ruta.display().to_string(),
        detalle,
    };

    if let Some(padre) = ruta.parent() {
        std::fs::create_dir_all(padre).map_err(|e| fallo(e.to_string()))?;
    }

    let anterior = match std::fs::read_to_string(ruta) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(fallo(e.to_string())),
    };

    let mut lineas: Vec<String> = anterior.lines().map(str::to_string).collect();
    let mut por_anadir = Vec::new();

    for (clave, valor) in nuevas {
        let linea = format!("{clave}={valor}");
        match lineas.iter().position(|l| asigna_a(l, clave)) {
            Some(i) => lineas[i] = linea,
            None => por_anadir.push(linea),
        }
    }
    lineas.extend(por_anadir);

    let mut texto = lineas.join("\n");
    if !texto.is_empty() {
        texto.push('\n');
    }

    let temporal = ruta.with_extension("parcial");
    escribir_restringido(&temporal, &texto).map_err(|e| fallo(e.to_string()))?;
    if let Err(e) = std::fs::rename(&temporal, ruta) {
        std::fs::remove_file(&temporal).ok();
        return Err(fallo(e.to_string()));
    }
    Ok(())
}

/// Quita del fichero las claves que empiecen por `prefijo`, y dice cuantas quito.
///
/// Sirve para olvidar un proveedor: todo lo suyo son claves `MULTIDRIVE_<RANURA>_...`, asi
/// que el prefijo las nombra a todas sin tener que enumerar los campos del catalogo. **Quien
/// llame tiene que incluir el guion bajo final**: sin el, `MULTIDRIVE_GDRIVE_` se llevaria por
/// delante a `MULTIDRIVE_GDRIVE1_ROOT`, que es de otra cuenta.
///
/// Los comentarios se quedan. Uno puede estar hablando de la clave que se va, pero adivinar
/// cual es cual seria borrar texto del usuario por una corazonada.
///
/// Se escribe a un temporal y se renombra, igual que [`guardar`] y por lo mismo: un corte a
/// mitad no puede dejar la configuracion truncada.
pub fn quitar(ruta: &Path, prefijo: &str) -> Result<usize, BackendError> {
    let fallo = |detalle: String| BackendError::FicheroDeConfig {
        fichero: ruta.display().to_string(),
        detalle,
    };

    let anterior = match std::fs::read_to_string(ruta) {
        Ok(s) => s,
        // No hay fichero, luego no hay nada que quitar. No es un error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(fallo(e.to_string())),
    };

    let antes = anterior.lines().count();
    let quedan: Vec<&str> = anterior
        .lines()
        .filter(|l| !clave_de(l).is_some_and(|c| c.starts_with(prefijo)))
        .collect();
    let quitadas = antes - quedan.len();
    if quitadas == 0 {
        return Ok(0);
    }

    let mut texto = quedan.join("\n");
    if !texto.is_empty() {
        texto.push('\n');
    }

    let temporal = ruta.with_extension("parcial");
    escribir_restringido(&temporal, &texto).map_err(|e| fallo(e.to_string()))?;
    if let Err(e) = std::fs::rename(&temporal, ruta) {
        std::fs::remove_file(&temporal).ok();
        return Err(fallo(e.to_string()));
    }
    Ok(quitadas)
}
/// Si esta linea le asigna un valor a esa clave, sea como sea que este escrita.
///
/// Tiene que reconocer las mismas formas que acepta el lector —con `export`, con espacios
/// alrededor del `=`— o reescribir una clave anadiria una segunda linea y la vieja seguiria
/// ganando o perdiendo segun el orden. Dos lineas para la misma clave es justo el estado en
/// el que nadie sabe cual manda.
fn asigna_a(linea: &str, clave: &str) -> bool {
    clave_de(linea) == Some(clave)
}

/// La clave que asigna esta linea, si asigna alguna.
///
/// Reconoce las mismas formas que acepta el lector --con `export`, con espacios alrededor del
/// `=`-- porque las dos cosas que la usan, reescribir y quitar, fallan igual de mal si se les
/// escapa una: reescribir deja dos lineas para la misma clave, y quitar deja la que borraba.
fn clave_de(linea: &str) -> Option<&str> {
    let l = linea.trim_start();
    if l.starts_with('#') {
        return None;
    }
    let l = l.strip_prefix("export ").unwrap_or(l);
    l.split_once('=').map(|(izq, _)| izq.trim())
}

#[cfg(unix)]
fn escribir_restringido(ruta: &Path, texto: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        // Desde que nace, no cuando se termina de escribir: entre `create` y un `chmod`
        // posterior hay una ventana en la que el fichero con los tokens es legible por todo
        // el mundo.
        .mode(0o600)
        .open(ruta)?;
    f.write_all(texto.as_bytes())?;
    f.sync_all()
}

#[cfg(not(unix))]
fn escribir_restringido(ruta: &Path, texto: &str) -> std::io::Result<()> {
    // En Windows el fichero va bajo `%APPDATA%`, cuyo ACL ya excluye a los demas usuarios y
    // lo hereda lo que se cree dentro. Por eso aqui no se toca el ACL: lo que si se hace es
    // **no escribir nunca en `ProgramData`**, que es el que si esta abierto a todos. Ver
    // `fichero_de_usuario`.
    std::fs::write(ruta, texto)
}

/// La carpeta donde escribir la configuracion de la **maquina**, que es la que ve un servicio.
pub fn carpeta_de_maquina() -> Option<PathBuf> {
    std::env::var("ProgramData")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|d| PathBuf::from(d).join("MultiDrive"))
        .or_else(|| Some(PathBuf::from("/etc/multidrive")))
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
            "multidrive-config-{}-{nombre}-{}",
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

    /// Olvidar un proveedor se lleva lo suyo y **solo** lo suyo.
    ///
    /// La trampa está en el número: `GDRIVE` y `GDRIVE1` son dos cuentas distintas, y sin el
    /// guión bajo final del prefijo la primera se llevaría por delante a la segunda. Quien
    /// quita una cuenta y se queda sin la otra no lo descubre hasta que el volumen no abre.
    #[test]
    fn quitar_se_lleva_una_ranura_sin_tocar_la_de_al_lado() {
        let d = dir("quitar");
        let ruta = escribir(
            &d,
            "multidrive.conf",
            "# la del trabajo\n\
             MULTIDRIVE_GDRIVE_REFRESH_TOKEN=uno\n\
             MULTIDRIVE_GDRIVE_ROOT=/multidrive\n\
             MULTIDRIVE_GDRIVE1_REFRESH_TOKEN=dos\n\
             MULTIDRIVE_APP_GDRIVE_CLIENT_ID=de-la-aplicacion\n",
        );

        assert_eq!(quitar(&ruta, "MULTIDRIVE_GDRIVE_").unwrap(), 2);

        let c = Config::desde(std::slice::from_ref(&ruta)).unwrap();
        assert_eq!(c.get("MULTIDRIVE_GDRIVE_REFRESH_TOKEN"), None);
        assert_eq!(c.get("MULTIDRIVE_GDRIVE_ROOT"), None);
        assert_eq!(
            c.get("MULTIDRIVE_GDRIVE1_REFRESH_TOKEN").as_deref(),
            Some("dos"),
            "la cuenta de al lado no se toca"
        );
        assert_eq!(
            c.get("MULTIDRIVE_APP_GDRIVE_CLIENT_ID").as_deref(),
            Some("de-la-aplicacion"),
            "la aplicacion no es de la cuenta"
        );
        let texto = std::fs::read_to_string(&ruta).unwrap();
        assert!(texto.contains("# la del trabajo"), "se llevo un comentario");
    }

    /// Las formas raras de escribir una asignación también cuentan.
    #[test]
    fn quitar_reconoce_export_y_espacios() {
        let d = dir("quitar");
        let ruta = escribir(
            &d,
            "multidrive.conf",
            "export MULTIDRIVE_FS_ROOT=/uno\n  MULTIDRIVE_FS_OTRO = /dos\nMULTIDRIVE_FS2_ROOT=/tres\n",
        );
        assert_eq!(quitar(&ruta, "MULTIDRIVE_FS_").unwrap(), 2);
        let texto = std::fs::read_to_string(&ruta).unwrap();
        assert!(texto.contains("MULTIDRIVE_FS2_ROOT"), "{texto}");
        assert!(!texto.contains("MULTIDRIVE_FS_"), "{texto}");
    }

    /// Sin fichero no hay nada que quitar, y eso no es un fallo.
    #[test]
    fn quitar_de_un_fichero_que_no_existe_no_es_un_error() {
        assert_eq!(
            quitar(&dir("quitar-vacio").join("no-existe.conf"), "MULTIDRIVE_").unwrap(),
            0
        );
    }
    #[test]
    fn guardar_crea_el_fichero_y_lo_que_se_escribe_se_lee() {
        let d = dir("crear");
        let ruta = d.join("multidrive.conf");
        guardar(
            &ruta,
            &[("MULTIDRIVE_FS_ROOT".into(), "D:\\multidrive".into())],
        )
        .unwrap();

        let c = Config::desde(&[ruta]).unwrap();
        assert_eq!(
            c.get("MULTIDRIVE_FS_ROOT").as_deref(),
            Some("D:\\multidrive")
        );
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn actualizar_una_clave_no_anade_una_segunda_linea() {
        // Dos lineas para la misma clave es el estado en el que nadie sabe cual manda.
        let d = dir("actualizar");
        let ruta = escribir(&d, "multidrive.conf", "MULTIDRIVE_FS_ROOT=/viejo\n");
        guardar(&ruta, &[("MULTIDRIVE_FS_ROOT".into(), "/nuevo".into())]).unwrap();

        let texto = std::fs::read_to_string(&ruta).unwrap();
        assert_eq!(texto.matches("MULTIDRIVE_FS_ROOT").count(), 1, "{texto}");
        assert!(texto.contains("/nuevo"));
        assert!(!texto.contains("/viejo"));
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn se_reconoce_la_clave_escrita_como_la_escribe_la_gente() {
        // El lector acepta `export` y espacios alrededor del `=`. Si el escritor no
        // reconociera esas formas, anadiria una linea nueva y quedarian las dos.
        for original in [
            "export MULTIDRIVE_FS_ROOT=/viejo\n",
            "MULTIDRIVE_FS_ROOT = /viejo\n",
            "  MULTIDRIVE_FS_ROOT=/viejo\n",
        ] {
            let d = dir("formas");
            let ruta = escribir(&d, "multidrive.conf", original);
            guardar(&ruta, &[("MULTIDRIVE_FS_ROOT".into(), "/nuevo".into())]).unwrap();

            let c = Config::desde(std::slice::from_ref(&ruta)).unwrap();
            assert_eq!(
                c.get("MULTIDRIVE_FS_ROOT").as_deref(),
                Some("/nuevo"),
                "con {original:?}"
            );
            let texto = std::fs::read_to_string(&ruta).unwrap();
            assert_eq!(texto.matches("MULTIDRIVE_FS_ROOT").count(), 1, "{texto}");
            std::fs::remove_dir_all(d).ok();
        }
    }

    #[test]
    fn una_clave_comentada_no_se_pisa_sino_que_se_anade_al_final() {
        // Alguien que deja una linea comentada esta guardando una nota, no configurando.
        // Reescribirla borraria la nota y ademas dejaria la clave comentada, o sea sin
        // efecto: el proveedor no aparecera y no habra ni rastro de por que.
        let d = dir("comentada");
        let ruta = escribir(&d, "multidrive.conf", "# MULTIDRIVE_FS_ROOT=/lo-de-antes\n");
        guardar(&ruta, &[("MULTIDRIVE_FS_ROOT".into(), "/nuevo".into())]).unwrap();

        let texto = std::fs::read_to_string(&ruta).unwrap();
        assert!(
            texto.contains("# MULTIDRIVE_FS_ROOT=/lo-de-antes"),
            "{texto}"
        );
        let c = Config::desde(&[ruta]).unwrap();
        assert_eq!(c.get("MULTIDRIVE_FS_ROOT").as_deref(), Some("/nuevo"));
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn lo_que_habia_alrededor_sigue_ahi() {
        // El fichero es de quien lo escribe. Una herramienta que lo reformatea a su gusto
        // convierte cualquier anotacion en algo que no merece la pena escribir.
        let d = dir("alrededor");
        let ruta = escribir(
            &d,
            "multidrive.conf",
            "# La cuenta del trabajo, no tocar\nMULTIDRIVE_GDRIVE_CLIENT_ID=abc\n\n# fin\n",
        );
        guardar(&ruta, &[("MULTIDRIVE_FS_ROOT".into(), "/nuevo".into())]).unwrap();

        let texto = std::fs::read_to_string(&ruta).unwrap();
        assert!(
            texto.contains("# La cuenta del trabajo, no tocar"),
            "{texto}"
        );
        assert!(texto.contains("MULTIDRIVE_GDRIVE_CLIENT_ID=abc"), "{texto}");
        assert!(texto.contains("# fin"), "{texto}");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn no_queda_ningun_fichero_a_medias() {
        // Se escribe a un temporal y se renombra. Si quedara el temporal, un directorio de
        // configuracion acabaria lleno de restos con credenciales dentro.
        let d = dir("temporal");
        let ruta = d.join("multidrive.conf");
        guardar(&ruta, &[("MULTIDRIVE_FS_ROOT".into(), "/x".into())]).unwrap();

        let restos: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "multidrive.conf")
            .collect();
        assert!(restos.is_empty(), "quedaron restos: {restos:?}");
        std::fs::remove_dir_all(d).ok();
    }

    #[cfg(unix)]
    #[test]
    fn el_fichero_con_credenciales_no_lo_puede_leer_nadie_mas() {
        use std::os::unix::fs::PermissionsExt;

        let d = dir("permisos");
        let ruta = d.join("multidrive.conf");
        guardar(
            &ruta,
            &[("MULTIDRIVE_GDRIVE_CLIENT_SECRET".into(), "s".into())],
        )
        .unwrap();

        let modo = std::fs::metadata(&ruta).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            modo, 0o600,
            "aqui dentro hay acceso permanente a las cuentas"
        );
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn nunca_se_ofrece_el_fichero_de_la_maquina_para_escribir_credenciales() {
        // `ProgramData` y `/etc` los lee cualquier usuario del equipo. Un servicio necesita
        // ese fichero, pero eso se decide a mano y a sabiendas.
        if let (Some(usuario), Some(maquina)) = (fichero_de_usuario(), fichero_de_maquina()) {
            assert_ne!(usuario, maquina);
        }
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
            "\u{feff}# comentario\nMULTIDRIVE_FS_ROOT=/datos\n",
        );
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(c.get("MULTIDRIVE_FS_ROOT").as_deref(), Some("/datos"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn la_marca_tampoco_rompe_una_clave_en_la_primera_linea() {
        let d = dir("bom-clave");
        let f = escribir(&d, "a.conf", "\u{feff}MULTIDRIVE_FS_ROOT=/datos\n");
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(c.get("MULTIDRIVE_FS_ROOT").as_deref(), Some("/datos"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lee_claves_y_valores() {
        let d = dir("basico");
        let f = escribir(&d, "a.conf", "MULTIDRIVE_GDRIVE_ROOT=/volumen\n");
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(c.get("MULTIDRIVE_GDRIVE_ROOT").as_deref(), Some("/volumen"));
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
             export MULTIDRIVE_GDRIVE_CLIENT_ID=\"un-id\"\n\
             \n\
             MULTIDRIVE_GDRIVE_ROOT='/con espacios'\n",
        );
        let c = Config::desde(&[f]).unwrap();
        assert_eq!(
            c.get("MULTIDRIVE_GDRIVE_CLIENT_ID").as_deref(),
            Some("un-id")
        );
        assert_eq!(
            c.get("MULTIDRIVE_GDRIVE_ROOT").as_deref(),
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
        let c = Config::desde(&[PathBuf::from("/no/existe/multidrive.conf")]).unwrap();
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
