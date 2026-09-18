//! Suite de conformidad que todo proveedor real debe pasar.
//!
//! Los tests unitarios corren contra memoria y no prueban nada de lo que de verdad rompe
//! un volumen en produccion: latencia, limites de peticiones, errores que el proveedor
//! traduce de otra manera, contenido que vuelve alterado. Esta suite se ejecuta contra la
//! cuenta de verdad y comprueba, una por una, las promesas del trait [`Backend`] de las
//! que depende el resto de LibreDisk.
//!
//! Cada comprobacion lleva escrito **por que** existe: cual es la operacion de `ChunkStore`
//! que se rompe si el proveedor no cumple. Un fallo aqui no es un detalle de la prueba,
//! es un proveedor que no sirve como pata de un RAID.
//!
//! Todas las claves cuelgan de un prefijo unico por ejecucion y se borran al terminar, para
//! que apuntar la suite a una carpeta con datos dentro no pueda tocarlos.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{Backend, BackendError};

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error(transparent)]
    Proveedor(#[from] BackendError),
    #[error("{0}")]
    Incumplido(String),
}

type CheckResult = Result<(), CheckError>;

fn incumplido(msg: impl Into<String>) -> CheckError {
    CheckError::Incumplido(msg.into())
}

/// Resultado de una comprobacion.
#[derive(Debug)]
pub struct Check {
    pub nombre: &'static str,
    /// Que operacion de LibreDisk se rompe si esto falla.
    pub porque: &'static str,
    pub duracion: Duration,
    /// `None` si paso.
    pub fallo: Option<String>,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.fallo.is_none()
    }
}

/// Lo que la suite dice de un proveedor.
#[derive(Debug)]
pub struct Informe {
    pub proveedor: String,
    /// Prefijo bajo el que se escribio todo. Si la limpieza fallo, es lo que hay que borrar.
    pub prefijo: String,
    pub checks: Vec<Check>,
    /// Error de la limpieza final, si lo hubo. No invalida las comprobaciones, pero deja
    /// basura en la cuenta del usuario y hay que decirlo.
    pub limpieza: Option<String>,
}

impl Informe {
    pub fn todo_ok(&self) -> bool {
        self.checks.iter().all(Check::ok) && self.limpieza.is_none()
    }

    pub fn fallidos(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| !c.ok())
    }

    /// Duracion de una comprobacion concreta, para comparar latencias entre proveedores.
    pub fn duracion_de(&self, nombre: &str) -> Option<Duration> {
        self.checks
            .iter()
            .find(|c| c.nombre == nombre)
            .map(|c| c.duracion)
    }
}

/// Prefijo unico por ejecucion: evita que dos pasadas simultaneas se pisen las claves.
fn prefijo_unico() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("libredisk-conformidad/{}-{nanos}", std::process::id())
}

/// Datos deterministas pero no compresibles, para que el proveedor no pueda "ganar"
/// comprimiendo: un fragmento real va cifrado y tiene esta pinta.
fn datos(len: usize, semilla: u64) -> Vec<u8> {
    let mut estado = semilla | 1;
    (0..len)
        .map(|_| {
            // Congruencial lineal; no hace falta calidad criptografica, solo entropia aparente.
            estado = estado
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (estado >> 33) as u8
        })
        .collect()
}

/// Lleva la cuenta de lo escrito para poder borrarlo todo al final.
struct Escenario {
    prefijo: String,
    escritas: Mutex<Vec<String>>,
}

impl Escenario {
    fn clave(&self, sufijo: &str) -> String {
        let clave = format!("{}/{sufijo}", self.prefijo);
        self.escritas
            .lock()
            .expect("mutex de claves envenenado")
            .push(clave.clone());
        clave
    }
}

/// Ejecuta la suite completa contra un proveedor y borra lo que haya escrito.
pub async fn ejecutar(backend: &dyn Backend) -> Informe {
    let esc = Escenario {
        prefijo: prefijo_unico(),
        escritas: Mutex::new(Vec::new()),
    };
    let mut checks = Vec::new();

    macro_rules! check {
        ($nombre:literal, $porque:literal, $cuerpo:expr) => {{
            let t0 = Instant::now();
            let resultado = $cuerpo.await;
            checks.push(Check {
                nombre: $nombre,
                porque: $porque,
                duracion: t0.elapsed(),
                fallo: resultado.err().map(|e: CheckError| e.to_string()),
            });
        }};
    }

    check!(
        "ida_y_vuelta",
        "si un fragmento no vuelve byte a byte, el trozo entero es irrecuperable",
        ida_y_vuelta(backend, &esc)
    );
    check!(
        "sobrescritura",
        "repair reescribe fragmentos que ya existen; sin sobrescritura, repair miente",
        sobrescritura(backend, &esc)
    );
    check!(
        "not_found_distinguible",
        "scrub separa 'falta el fragmento' de 'el proveedor esta caido' por este error",
        not_found_distinguible(backend, &esc)
    );
    check!(
        "borrar_inexistente",
        "la limpieza tras un put parcial borra claves que quiza nunca llegaron a escribirse",
        borrar_inexistente(backend, &esc)
    );
    check!(
        "exists_refleja_el_borrado",
        "scrub pregunta exists antes de descargar nada: si miente, cuenta redundancia que no hay",
        exists_refleja_el_borrado(backend, &esc)
    );
    check!(
        "integridad_binaria",
        "los fragmentos van cifrados; cualquier reinterpretacion como texto los destruye",
        integridad_binaria(backend, &esc)
    );
    check!(
        "clave_con_forma_de_fragmento",
        "shard_key anida dos niveles y mete un punto en el nombre: no todo proveedor lo acepta",
        clave_con_forma_de_fragmento(backend, &esc)
    );
    check!(
        "fragmento_de_4_mib",
        "un trozo de 16 MiB en rs(4,2) da fragmentos de 4 MiB: ese es el tamano real de trabajo",
        fragmento_de_4_mib(backend, &esc)
    );
    check!(
        "seis_escrituras_en_paralelo",
        "el motor RAID escribe a los n proveedores a la vez; aqui asoman los limites de peticiones",
        seis_escrituras_en_paralelo(backend, &esc)
    );

    let limpieza = limpiar(backend, &esc).await.err().map(|e| e.to_string());

    Informe {
        proveedor: backend.name().to_string(),
        prefijo: esc.prefijo.clone(),
        checks,
        limpieza,
    }
}

/// Resumen de una comprobacion a lo largo de varias pasadas.
#[derive(Debug)]
pub struct CheckAgregado {
    pub nombre: &'static str,
    pub porque: &'static str,
    pub mediana: Duration,
    pub min: Duration,
    pub max: Duration,
    /// Cuantas pasadas la superaron. Menos que el total significa intermitencia, que es
    /// peor noticia que un fallo limpio: el proveedor cumple a veces.
    pub pasadas_ok: usize,
    /// Motivos distintos de fallo, sin repetir.
    pub fallos: Vec<String>,
}

impl CheckAgregado {
    pub fn ok(&self) -> bool {
        self.fallos.is_empty()
    }

    /// Dispersion relativa: `(max - min) / mediana`. Si es alta, una sola pasada no mide
    /// nada y cualquier cifra suelta es ruido.
    pub fn dispersion(&self) -> f64 {
        if self.mediana.is_zero() {
            return 0.0;
        }
        (self.max.as_secs_f64() - self.min.as_secs_f64()) / self.mediana.as_secs_f64()
    }
}

/// Lo que la suite dice de un proveedor tras varias pasadas.
#[derive(Debug)]
pub struct InformeAgregado {
    pub proveedor: String,
    pub pasadas: usize,
    pub checks: Vec<CheckAgregado>,
    /// Prefijos que no se pudieron limpiar, con su motivo.
    pub limpiezas_fallidas: Vec<(String, String)>,
}

impl InformeAgregado {
    pub fn todo_ok(&self) -> bool {
        self.checks.iter().all(CheckAgregado::ok) && self.limpiezas_fallidas.is_empty()
    }

    pub fn fallidos(&self) -> impl Iterator<Item = &CheckAgregado> {
        self.checks.iter().filter(|c| !c.ok())
    }
}

/// Varias pasadas de la suite, con mediana y dispersion.
///
/// Una sola pasada no vale como medida: contra Google Drive se observaron diferencias del
/// 43 % entre dos ejecuciones seguidas. Lo que aguanta es la mediana y el rango, y el numero
/// de pasadas que cada comprobacion supera.
pub async fn ejecutar_varias(backend: &dyn Backend, pasadas: usize) -> InformeAgregado {
    let pasadas = pasadas.max(1);
    let mut informes = Vec::with_capacity(pasadas);
    for _ in 0..pasadas {
        informes.push(ejecutar(backend).await);
    }
    agregar(backend.name(), &informes)
}

fn agregar(proveedor: &str, informes: &[Informe]) -> InformeAgregado {
    let mut checks = Vec::new();

    for (i, referencia) in informes[0].checks.iter().enumerate() {
        // Todas las pasadas ejecutan las mismas comprobaciones en el mismo orden, pero se
        // compara el nombre para no alinear mal si eso cambiara.
        let muestras: Vec<&Check> = informes
            .iter()
            .filter_map(|inf| inf.checks.get(i))
            .filter(|c| c.nombre == referencia.nombre)
            .collect();

        let duraciones: Vec<Duration> = muestras.iter().map(|c| c.duracion).collect();
        let mut fallos: Vec<String> = muestras.iter().filter_map(|c| c.fallo.clone()).collect();
        fallos.sort();
        fallos.dedup();

        checks.push(CheckAgregado {
            nombre: referencia.nombre,
            porque: referencia.porque,
            mediana: mediana(&duraciones),
            min: duraciones.iter().copied().min().unwrap_or_default(),
            max: duraciones.iter().copied().max().unwrap_or_default(),
            pasadas_ok: muestras.iter().filter(|c| c.ok()).count(),
            fallos,
        });
    }

    InformeAgregado {
        proveedor: proveedor.to_string(),
        pasadas: informes.len(),
        checks,
        limpiezas_fallidas: informes
            .iter()
            .filter_map(|inf| inf.limpieza.clone().map(|e| (inf.prefijo.clone(), e)))
            .collect(),
    }
}

fn mediana(duraciones: &[Duration]) -> Duration {
    if duraciones.is_empty() {
        return Duration::ZERO;
    }
    let mut ordenadas = duraciones.to_vec();
    ordenadas.sort_unstable();
    let n = ordenadas.len();
    if n % 2 == 1 {
        ordenadas[n / 2]
    } else {
        (ordenadas[n / 2 - 1] + ordenadas[n / 2]) / 2
    }
}

async fn ida_y_vuelta(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    let clave = esc.clave("ida-y-vuelta");
    let datos = datos(64 * 1024, 1);
    b.put(&clave, datos.clone()).await?;
    let leido = b.get(&clave).await?;
    if leido != datos {
        return Err(incumplido(format!(
            "lo leido difiere de lo escrito ({} bytes escritos, {} leidos)",
            datos.len(),
            leido.len()
        )));
    }
    Ok(())
}

async fn sobrescritura(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    let clave = esc.clave("sobrescritura");
    b.put(&clave, datos(4096, 2)).await?;
    let nuevo = datos(8192, 3);
    b.put(&clave, nuevo.clone()).await?;
    if b.get(&clave).await? != nuevo {
        return Err(incumplido(
            "tras sobrescribir se sigue leyendo el contenido viejo",
        ));
    }
    Ok(())
}

async fn not_found_distinguible(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    // No se registra como escrita: no existe y no hay que borrarla.
    let clave = format!("{}/fantasma", esc.prefijo);
    match b.get(&clave).await {
        Err(BackendError::NotFound(_)) => Ok(()),
        Ok(_) => Err(incumplido("leer una clave inexistente devolvio contenido")),
        Err(otro) => Err(incumplido(format!(
            "leer una clave inexistente dio un error que no es NotFound: {otro}"
        ))),
    }
}

async fn borrar_inexistente(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    b.delete(&format!("{}/nunca-escrito", esc.prefijo)).await?;
    Ok(())
}

async fn exists_refleja_el_borrado(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    let clave = esc.clave("exists");
    if b.exists(&clave).await? {
        return Err(incumplido(
            "exists dice que si sobre una clave recien inventada",
        ));
    }
    b.put(&clave, datos(512, 4)).await?;
    if !b.exists(&clave).await? {
        return Err(incumplido(
            "exists dice que no sobre una clave recien escrita",
        ));
    }
    b.delete(&clave).await?;
    if b.exists(&clave).await? {
        return Err(incumplido(
            "exists sigue diciendo que si despues de borrar (consistencia eventual)",
        ));
    }
    Ok(())
}

async fn integridad_binaria(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    let clave = esc.clave("binario");
    // Los 256 valores de byte, incluidos nulos, saltos de linea y secuencias UTF-8 invalidas.
    let datos: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    b.put(&clave, datos.clone()).await?;
    let leido = b.get(&clave).await?;
    if leido != datos {
        let primero = leido
            .iter()
            .zip(&datos)
            .position(|(a, b)| a != b)
            .map(|i| format!("primera diferencia en el byte {i}"))
            .unwrap_or_else(|| format!("longitud distinta: {} vs {}", leido.len(), datos.len()));
        return Err(incumplido(format!(
            "el contenido volvio alterado; {primero}"
        )));
    }
    Ok(())
}

async fn clave_con_forma_de_fragmento(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    // Misma forma que produce `ChunkId::shard_key`: chunks/<2 hex>/<64 hex>.<indice>
    let hash = "a".repeat(64);
    let clave = esc.clave(&format!("chunks/aa/{hash}.000"));
    b.put(&clave, datos(1024, 5)).await?;
    if b.get(&clave).await?.len() != 1024 {
        return Err(incumplido(
            "la clave con forma de fragmento no volvio intacta",
        ));
    }
    Ok(())
}

async fn fragmento_de_4_mib(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    let clave = esc.clave("fragmento-4mib");
    let datos = datos(4 * 1024 * 1024, 6);
    b.put(&clave, datos.clone()).await?;
    let leido = b.get(&clave).await?;
    if leido.len() != datos.len() {
        return Err(incumplido(format!(
            "se escribieron {} bytes y volvieron {}",
            datos.len(),
            leido.len()
        )));
    }
    if leido != datos {
        return Err(incumplido("el fragmento de 4 MiB volvio corrupto"));
    }
    Ok(())
}

async fn seis_escrituras_en_paralelo(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    let claves: Vec<String> = (0..6)
        .map(|i| esc.clave(&format!("paralelo-{i}")))
        .collect();
    let carga = datos(256 * 1024, 7);

    let (r0, r1, r2, r3, r4, r5) = tokio::join!(
        b.put(&claves[0], carga.clone()),
        b.put(&claves[1], carga.clone()),
        b.put(&claves[2], carga.clone()),
        b.put(&claves[3], carga.clone()),
        b.put(&claves[4], carga.clone()),
        b.put(&claves[5], carga.clone()),
    );

    let fallos: Vec<String> = [r0, r1, r2, r3, r4, r5]
        .into_iter()
        .filter_map(|r| r.err().map(|e| e.to_string()))
        .collect();
    if !fallos.is_empty() {
        return Err(incumplido(format!(
            "{} de 6 escrituras simultaneas fallaron: {}",
            fallos.len(),
            fallos.join(" | ")
        )));
    }
    Ok(())
}

/// Borra todo lo escrito. Devuelve el primer error, pero intenta borrarlo todo igualmente.
async fn limpiar(b: &dyn Backend, esc: &Escenario) -> Result<(), BackendError> {
    let claves = esc
        .escritas
        .lock()
        .expect("mutex de claves envenenado")
        .clone();
    let mut primero = None;
    for clave in claves {
        if let Err(e) = b.delete(&clave).await
            && primero.is_none()
        {
            primero = Some(e);
        }
    }
    match primero {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpendalBackend;

    /// El proveedor en memoria cumple el contrato por construccion: si la suite falla aqui,
    /// el fallo esta en la suite, no en el proveedor.
    #[tokio::test]
    async fn memoria_pasa_la_suite_entera() {
        let b = OpendalBackend::memory("memoria").unwrap();
        let informe = ejecutar(&b).await;
        let fallos: Vec<_> = informe
            .fallidos()
            .map(|c| format!("{}: {}", c.nombre, c.fallo.clone().unwrap_or_default()))
            .collect();
        assert!(fallos.is_empty(), "fallaron: {fallos:?}");
        assert!(informe.limpieza.is_none());
    }

    #[tokio::test]
    async fn la_suite_deja_el_proveedor_limpio() {
        let b = OpendalBackend::memory("memoria").unwrap();
        let informe = ejecutar(&b).await;
        // Tras limpiar, ninguna clave del prefijo debe seguir viva.
        assert!(
            !b.exists(&format!("{}/ida-y-vuelta", informe.prefijo))
                .await
                .unwrap()
        );
        assert!(
            !b.exists(&format!("{}/fragmento-4mib", informe.prefijo))
                .await
                .unwrap()
        );
    }

    #[test]
    fn mediana_de_un_numero_par_de_muestras() {
        let ms = |n| Duration::from_millis(n);
        assert_eq!(mediana(&[]), Duration::ZERO);
        assert_eq!(mediana(&[ms(7)]), ms(7));
        assert_eq!(mediana(&[ms(30), ms(10)]), ms(20));
        assert_eq!(mediana(&[ms(30), ms(10), ms(50)]), ms(30));
    }

    #[tokio::test]
    async fn varias_pasadas_resumen_mediana_y_rango() {
        let b = OpendalBackend::memory("memoria").unwrap();
        let informe = ejecutar_varias(&b, 3).await;

        assert_eq!(informe.pasadas, 3);
        assert!(informe.todo_ok());
        assert_eq!(informe.checks.len(), 9);
        for c in &informe.checks {
            assert_eq!(c.pasadas_ok, 3, "{}", c.nombre);
            assert!(c.min <= c.mediana && c.mediana <= c.max, "{}", c.nombre);
        }
    }

    #[tokio::test]
    async fn cero_pasadas_se_trata_como_una() {
        let b = OpendalBackend::memory("memoria").unwrap();
        assert_eq!(ejecutar_varias(&b, 0).await.pasadas, 1);
    }

    /// Un proveedor que silenciosamente no sobrescribe debe ser cazado por la suite.
    #[tokio::test]
    async fn la_suite_caza_un_proveedor_que_no_sobrescribe() {
        struct NoSobrescribe(OpendalBackend);

        #[async_trait::async_trait]
        impl Backend for NoSobrescribe {
            fn name(&self) -> &str {
                self.0.name()
            }
            async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackendError> {
                if self.0.exists(key).await? {
                    return Ok(()); // acepta la escritura y la descarta: el peor caso
                }
                self.0.put(key, data).await
            }
            async fn get(&self, key: &str) -> Result<Vec<u8>, BackendError> {
                self.0.get(key).await
            }
            async fn delete(&self, key: &str) -> Result<(), BackendError> {
                self.0.delete(key).await
            }
            async fn exists(&self, key: &str) -> Result<bool, BackendError> {
                self.0.exists(key).await
            }
        }

        let b = NoSobrescribe(OpendalBackend::memory("tramposo").unwrap());
        let informe = ejecutar(&b).await;
        let nombres: Vec<_> = informe.fallidos().map(|c| c.nombre).collect();
        assert!(
            nombres.contains(&"sobrescritura"),
            "la suite no detecto la falta de sobrescritura; fallaron: {nombres:?}"
        );

        // Y el resumen de varias pasadas debe decir que fallo en todas, no solo que fallo.
        let agregado = ejecutar_varias(&b, 2).await;
        let sobrescritura = agregado
            .checks
            .iter()
            .find(|c| c.nombre == "sobrescritura")
            .expect("falta la comprobacion");
        assert_eq!(sobrescritura.pasadas_ok, 0);
        assert_eq!(
            sobrescritura.fallos.len(),
            1,
            "motivos repetidos sin deduplicar"
        );
    }
}
