//! Suite de conformidad que todo proveedor real debe pasar.
//!
//! Los tests unitarios corren contra memoria y no prueban nada de lo que de verdad rompe
//! un volumen en produccion: latencia, limites de peticiones, errores que el proveedor
//! traduce de otra manera, contenido que vuelve alterado. Esta suite se ejecuta contra la
//! cuenta de verdad y comprueba, una por una, las promesas del trait [`Backend`] de las
//! que depende el resto de MultiDrive.
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
    /// Que operacion de MultiDrive se rompe si esto falla.
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
    format!("multidrive-conformidad/{}-{nanos}", std::process::id())
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

/// Datos de prueba, generados **antes** de arrancar el cronometro.
///
/// Generar 4 MiB con el congruencial de arriba cuesta ~48 ms en una build de debug. Contra
/// una nube eso es ruido, pero contra un directorio local es la mayor parte de la medida: el
/// arnes acabaria midiendose a si mismo.
struct Cargas {
    ida_y_vuelta: Vec<u8>,
    sobrescritura_vieja: Vec<u8>,
    sobrescritura_nueva: Vec<u8>,
    exists: Vec<u8>,
    binaria: Vec<u8>,
    forma_de_fragmento: Vec<u8>,
    listado: Vec<u8>,
    fragmento: Vec<u8>,
    paralelo: Vec<u8>,
}

impl Cargas {
    fn nuevas() -> Self {
        Self {
            ida_y_vuelta: datos(64 * 1024, 1),
            sobrescritura_vieja: datos(4096, 2),
            sobrescritura_nueva: datos(8192, 3),
            exists: datos(512, 4),
            // Los 256 valores de byte, incluidos nulos, saltos de linea y secuencias UTF-8
            // invalidas.
            binaria: (0..=255u8).cycle().take(4096).collect(),
            forma_de_fragmento: datos(1024, 5),
            listado: datos(1024, 8),
            fragmento: datos(4 * 1024 * 1024, 6),
            paralelo: datos(256 * 1024, 7),
        }
    }
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
    let cargas = Cargas::nuevas();
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
        ida_y_vuelta(backend, &esc, &cargas)
    );
    check!(
        "sobrescritura",
        "repair reescribe fragmentos que ya existen; sin sobrescritura, repair miente",
        sobrescritura(backend, &esc, &cargas)
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
        exists_refleja_el_borrado(backend, &esc, &cargas)
    );
    check!(
        "integridad_binaria",
        "los fragmentos van cifrados; cualquier reinterpretacion como texto los destruye",
        integridad_binaria(backend, &esc, &cargas)
    );
    check!(
        "clave_con_forma_de_fragmento",
        "shard_key anida dos niveles y mete un punto en el nombre: no todo proveedor lo acepta",
        clave_con_forma_de_fragmento(backend, &esc, &cargas)
    );
    check!(
        "fragmento_de_4_mib",
        "un trozo de 16 MiB en rs(4,2) da fragmentos de 4 MiB: ese es el tamano real de trabajo",
        fragmento_de_4_mib(backend, &esc, &cargas)
    );
    check!(
        "seis_escrituras_en_paralelo",
        "el motor RAID escribe a los n proveedores a la vez; aqui asoman los limites de peticiones",
        seis_escrituras_en_paralelo(backend, &esc, &cargas)
    );
    check!(
        "listado_enumera_lo_escrito",
        "scrub enumera en vez de preguntar: sin list, revisar 1 TB en Drive serian semanas",
        listado_enumera_lo_escrito(backend, &esc, &cargas)
    );
    check!(
        "listado_de_prefijo_vacio",
        "un volumen recien creado no tiene nada que enumerar, y eso no es un fallo suyo",
        listado_de_prefijo_vacio(backend, &esc)
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

async fn ida_y_vuelta(b: &dyn Backend, esc: &Escenario, c: &Cargas) -> CheckResult {
    let clave = esc.clave("ida-y-vuelta");
    b.put(&clave, c.ida_y_vuelta.clone()).await?;
    let leido = b.get(&clave).await?;
    if leido != c.ida_y_vuelta {
        return Err(incumplido(format!(
            "lo leido difiere de lo escrito ({} bytes escritos, {} leidos)",
            c.ida_y_vuelta.len(),
            leido.len()
        )));
    }
    Ok(())
}

async fn sobrescritura(b: &dyn Backend, esc: &Escenario, c: &Cargas) -> CheckResult {
    let clave = esc.clave("sobrescritura");
    b.put(&clave, c.sobrescritura_vieja.clone()).await?;
    b.put(&clave, c.sobrescritura_nueva.clone()).await?;
    if b.get(&clave).await? != c.sobrescritura_nueva {
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

async fn exists_refleja_el_borrado(b: &dyn Backend, esc: &Escenario, c: &Cargas) -> CheckResult {
    let clave = esc.clave("exists");
    if b.exists(&clave).await? {
        return Err(incumplido(
            "exists dice que si sobre una clave recien inventada",
        ));
    }
    b.put(&clave, c.exists.clone()).await?;
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

async fn integridad_binaria(b: &dyn Backend, esc: &Escenario, c: &Cargas) -> CheckResult {
    let clave = esc.clave("binario");
    b.put(&clave, c.binaria.clone()).await?;
    let leido = b.get(&clave).await?;
    if leido != c.binaria {
        let primero = leido
            .iter()
            .zip(&c.binaria)
            .position(|(a, b)| a != b)
            .map(|i| format!("primera diferencia en el byte {i}"))
            .unwrap_or_else(|| {
                format!("longitud distinta: {} vs {}", leido.len(), c.binaria.len())
            });
        return Err(incumplido(format!(
            "el contenido volvio alterado; {primero}"
        )));
    }
    Ok(())
}

async fn clave_con_forma_de_fragmento(b: &dyn Backend, esc: &Escenario, c: &Cargas) -> CheckResult {
    // Misma forma que produce `ChunkId::shard_key`: chunks/<2 hex>/<64 hex>.<indice>
    let hash = "a".repeat(64);
    let clave = esc.clave(&format!("chunks/aa/{hash}.000"));
    b.put(&clave, c.forma_de_fragmento.clone()).await?;
    if b.get(&clave).await?.len() != c.forma_de_fragmento.len() {
        return Err(incumplido(
            "la clave con forma de fragmento no volvio intacta",
        ));
    }
    Ok(())
}

async fn fragmento_de_4_mib(b: &dyn Backend, esc: &Escenario, c: &Cargas) -> CheckResult {
    let clave = esc.clave("fragmento-4mib");
    b.put(&clave, c.fragmento.clone()).await?;
    let leido = b.get(&clave).await?;
    if leido.len() != c.fragmento.len() {
        return Err(incumplido(format!(
            "se escribieron {} bytes y volvieron {}",
            c.fragmento.len(),
            leido.len()
        )));
    }
    if leido != c.fragmento {
        return Err(incumplido("el fragmento de 4 MiB volvio corrupto"));
    }
    Ok(())
}

async fn seis_escrituras_en_paralelo(b: &dyn Backend, esc: &Escenario, c: &Cargas) -> CheckResult {
    let claves: Vec<String> = (0..6)
        .map(|i| esc.clave(&format!("paralelo-{i}")))
        .collect();
    let carga = &c.paralelo;

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

async fn listado_enumera_lo_escrito(b: &dyn Backend, esc: &Escenario, c: &Cargas) -> CheckResult {
    // Subprefijo propio: lo que enumere esta comprobacion no debe depender de lo que las
    // demas hayan dejado escrito, ni al reves.
    let hash = "b".repeat(64);
    let claves: Vec<String> = (0..3)
        .map(|i| esc.clave(&format!("listado/chunks/{}/{hash}.{i:03}", &hash[..2])))
        .collect();
    for clave in &claves {
        b.put(clave, c.listado.clone()).await?;
    }

    let vistas = b.list(&format!("{}/listado/", esc.prefijo)).await?;
    let nombres: Vec<&str> = vistas.iter().map(|o| o.clave.as_str()).collect();

    let faltan: Vec<&str> = claves
        .iter()
        .filter(|k| !nombres.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    if !faltan.is_empty() {
        return Err(incumplido(format!(
            "list no devolvio {} de {} claves recien escritas: {}",
            faltan.len(),
            claves.len(),
            faltan.join(", ")
        )));
    }

    // El tamano viene en el mismo listado en los seis proveedores soportados, y de ahi sale
    // lo que ocupa el volumen en cada uno. Un proveedor que lo devolviera a cero haria que
    // MultiDrive informase de una ocupacion de cero sobre datos que existen, que es
    // exactamente la clase de mentira que este proyecto no se permite. Mejor fallar aqui.
    let esperado = c.listado.len() as u64;
    if let Some(malo) = vistas.iter().find(|o| o.bytes != esperado) {
        return Err(incumplido(format!(
            "list devolvio '{}' con {} bytes cuando se escribieron {esperado}: sin el tamano \
             en el listado no se puede medir lo que ocupa el volumen sin descargarlo entero",
            malo.clave, malo.bytes
        )));
    }

    // Enumerar tiene que devolver claves usables tal cual. Si el proveedor cuela directorios
    // o rutas relativas al prefijo, scrub las compara contra shard_key y no casan: contaria
    // como ausentes fragmentos que estan ahi.
    let intrusas: Vec<&str> = nombres
        .iter()
        .filter(|k| !claves.iter().any(|c| c == *k))
        .copied()
        .collect();
    if !intrusas.is_empty() {
        return Err(incumplido(format!(
            "list devolvio {} entradas que no son objetos escritos (directorios o rutas \
             relativas): {}",
            intrusas.len(),
            intrusas
                .iter()
                .take(3)
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(())
}

async fn listado_de_prefijo_vacio(b: &dyn Backend, esc: &Escenario) -> CheckResult {
    // No se registra como escrita: nunca llega a existir.
    let vistas = b.list(&format!("{}/nunca-escrito/", esc.prefijo)).await?;
    if !vistas.is_empty() {
        return Err(incumplido(format!(
            "enumerar un prefijo sin nada debajo devolvio {} entradas en vez de ninguna",
            vistas.len()
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

/// Cuanto se espera a cada operacion de la comprobacion.
///
/// Existe porque sin el la ventana se queda en «Comprobando que escribe y lee…» para
/// siempre, sin decir en que paso ni por que. Una pantalla que espera indefinidamente es un
/// fallo por si misma, aunque debajo haya otro: no deja ni reintentar ni entender.
///
/// Minuto y medio por operacion es de sobra para escribir 34 bytes en cualquier nube, con
/// margen para los reintentos que hace la capa de red por su cuenta.
const PACIENCIA: std::time::Duration = std::time::Duration::from_secs(90);

/// Una operacion de la comprobacion, con limite de tiempo y diciendo cual era.
async fn paso<T>(
    que: &str,
    f: impl std::future::Future<Output = Result<T, BackendError>>,
) -> Result<T, String> {
    match tokio::time::timeout(PACIENCIA, f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("{que}: {e}")),
        Err(_) => Err(format!(
            "{que}: el proveedor no contesto en {} segundos. No se ha guardado nada",
            PACIENCIA.as_secs()
        )),
    }
}

/// Comprobacion minima para decidir si un proveedor recien tecleado sirve.
///
/// No es la suite: son cuatro operaciones y unos pocos segundos, porque esto se ejecuta con
/// alguien mirando un formulario. La suite completa —once comprobaciones, minutos contra una
/// nube— sigue estando en `multidrive check`, y es la que hay que pasar antes de fiarle datos
/// de verdad a un proveedor nuevo.
///
/// Lo que se comprueba aqui es lo que descarta los fallos que importan al configurar:
/// credenciales malas, carpeta inexistente, permisos de solo lectura y, sobre todo, **que lo
/// leido sea lo escrito**. Un proveedor que acepta escrituras y devuelve otra cosa es peor
/// que uno que falla, porque el fallo aparece mucho mas tarde y lejos.
///
/// Cada paso lleva su propio limite de tiempo. Sin eso, un proveedor que no contesta deja la
/// interfaz esperando sin decir en que se quedo.
///
/// Limpia lo que escribe. Si no pudiera borrarlo, lo dice: dejar restos en la cuenta de
/// alguien sin avisar no esta bien, y ademas indica que el proveedor no sirve para un
/// volumen, donde borrar es parte del funcionamiento normal.
pub async fn prueba_rapida(b: &dyn Backend) -> Result<String, String> {
    let clave = format!(
        "multidrive-prueba/{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    let contenido = b"MultiDrive comprobando el proveedor".to_vec();

    paso("no se pudo escribir", b.put(&clave, contenido.clone())).await?;

    let leido = match paso("se escribio, pero no se pudo leer de vuelta", b.get(&clave)).await {
        Ok(v) => v,
        Err(e) => {
            b.delete(&clave).await.ok();
            return Err(e);
        }
    };
    if leido != contenido {
        b.delete(&clave).await.ok();
        return Err(format!(
            "lo leido no es lo escrito ({} bytes escritos, {} leidos). Este proveedor no \
             sirve para guardar datos: el fallo aparecería mucho después y lejos",
            contenido.len(),
            leido.len()
        ));
    }

    paso(
        "se escribio y se leyo, pero no se pudo borrar",
        b.delete(&clave),
    )
    .await?;

    Ok(format!(
        "escribe, lee lo mismo que escribio y borra. Comprobado con {} bytes.\n\
         Esto descarta credenciales malas y permisos, no que el proveedor cumpla el contrato \
         entero: para eso está `multidrive check`.",
        contenido.len()
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Objeto, OpendalBackend};

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
        assert_eq!(informe.checks.len(), 11);
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
            async fn list(&self, prefix: &str) -> Result<Vec<Objeto>, BackendError> {
                self.0.list(prefix).await
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
