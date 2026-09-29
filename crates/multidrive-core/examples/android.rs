//! Prueba de concepto: cuanto cuesta MultiDrive en un movil de verdad.
//!
//! No es una aplicacion de Android y no pretende serlo. Es un binario que se empuja con `adb`
//! a `/data/local/tmp` y se ejecuta desde `adb shell`, para responder a lo que ninguna
//! compilacion responde: **cuanto tarda esto en un telefono**.
//!
//! # Lo que mide, y por que en dos fases
//!
//! La primera fase **no toca la red ni necesita configuracion**: deriva la clave con Argon2id,
//! que es el unico coste de abrir un volumen que no depende de internet. Es tambien el unico
//! comparable de verdad contra el escritorio —mismo trabajo, otra CPU— y el que decide si la
//! aplicacion necesita otra parametrizacion o solo otra interfaz.
//!
//! La segunda **si toca la red**, y puede fallar por algo que no es culpa del volumen. Con esta
//! pila de dependencias, `reqwest` arrastra `rustls-platform-verifier`, que en Android va a
//! buscar el almacen de certificados del sistema **a traves de la JVM**: exige un componente
//! Kotlin y un puntero al `Context` de una aplicacion. Un binario lanzado desde `adb shell` no
//! tiene ni JVM ni `Context`, asi que ahi no hay TLS que valga.
//!
//! Eso no es un tropiezo de esta prueba: es un requisito real de la futura aplicacion, y vale
//! la pena saberlo ahora. La fase 2 se intenta igualmente porque el dia que exista ese
//! componente, esta misma prueba mide lo que falta.
//!
//! Solo lee. No escribe, no borra y no toca la maleta.
//!
//! ```sh
//! # En el equipo, con el NDK puesto (ver packaging/android/comprobar.ps1):
//! cargo build --release --target aarch64-linux-android -p multidrive-core --example android
//! adb push target/aarch64-linux-android/release/examples/android /data/local/tmp/mdprueba
//! adb shell chmod 700 /data/local/tmp/mdprueba
//! adb shell MULTIDRIVE_PASSPHRASE=lo-que-sea /data/local/tmp/mdprueba
//! adb shell rm -f /data/local/tmp/mdprueba
//! ```
//!
//! Para la fase 2 hace falta ademas empujar el fichero de configuracion, que lleva **tokens de
//! refresco en claro**: acceso permanente a las cuentas. Si se hace, se borra al terminar.

use std::time::Instant;

use multidrive_crypto::MasterKey;

const VUELTAS: usize = 7;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    println!("== MultiDrive en Android: que cuesta ==");
    println!("arquitectura  {}", std::env::consts::ARCH);
    println!("sistema       {}", std::env::consts::OS);
    println!();

    let frase = std::env::var("MULTIDRIVE_PASSPHRASE")
        .ok()
        .filter(|f| !f.trim().is_empty());
    let Some(frase) = frase else {
        eprintln!("falta MULTIDRIVE_PASSPHRASE. No hace falta que sea la de ningun volumen:");
        eprintln!("la fase 1 solo mide cuanto tarda la CPU en derivar una clave.");
        std::process::exit(1);
    };

    fase1(&frase);
    tls().await;
    fase2().await;
}

/// Solo TLS, sin credenciales de nadie.
///
/// Responde a la pregunta cara --si `rustls-platform-verifier` puede validar un certificado
/// sin JVM-- **sin necesidad de subir al telefono un fichero con tokens de refresco**. Una
/// peticion a un sitio publico basta: si el apreton de manos se completa, el verificador
/// funciona; si no, falla exactamente igual que le fallaria a una cuenta de nube.
async fn tls() {
    const DONDE: &str = "https://multidrive.net/";
    println!("-- Fase 1b: un HTTPS cualquiera, sin credenciales --");

    // En una tarea aparte porque en Android esto **no falla: revienta**.
    // `rustls-platform-verifier` hace `expect("Expect rustls-platform-verifier to be
    // initialized")` en cuanto se le pide verificar sin haberle dado el `Context` de la JVM, y
    // un panico se lleva el proceso por delante con las fases que falten. Recogido aqui, se
    // cuenta lo que paso y se sigue.
    let tarea = tokio::spawn(async move { intentar(DONDE).await });
    match tarea.await {
        Ok(()) => {}
        Err(e) if e.is_panic() => {
            println!("  {DONDE}  PANICO, no un error");
            println!();
            println!("  Es lo previsto, y peor de lo previsto: `rustls-platform-verifier` no");
            println!("  devuelve un fallo de certificado, aborta. Busca el almacen del sistema");
            println!("  a traves de la JVM y sin `Context` no sigue. Hace falta el componente");
            println!("  Kotlin, o sea una aplicacion de verdad.");
            println!();
            return;
        }
        Err(e) => println!("  la tarea no termino: {e}"),
    }
    println!();
}

async fn intentar(donde: &str) {
    let cliente = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            println!("  no se pudo ni construir el cliente: {e}");
            return;
        }
    };

    let t = Instant::now();
    match cliente.get(donde).send().await {
        Ok(r) => {
            println!(
                "  {donde}  {} en {} ms",
                r.status(),
                t.elapsed().as_millis()
            );
            println!("  El verificador de certificados FUNCIONA sin JVM.");
        }
        Err(e) => {
            println!("  {donde}  FALLO tras {} ms", t.elapsed().as_millis());
            println!("  {e}");
        }
    }
}

/// Argon2id, sin red y sin configuracion. Es la cifra que decide la parametrizacion.
fn fase1(frase: &str) {
    println!("-- Fase 1: derivar la clave (Argon2id), sin red --");
    // Una sal cualquiera: el coste depende de los parametros y de la CPU, no de su contenido.
    let sal = [0x5au8; 16];

    let mut tiempos = Vec::with_capacity(VUELTAS);
    for i in 1..=VUELTAS {
        let t = Instant::now();
        match MasterKey::derive(frase, &sal) {
            Ok(_) => {
                let ms = t.elapsed().as_millis();
                tiempos.push(ms);
                println!("  vuelta {i}      {ms:>8} ms");
            }
            Err(e) => {
                println!("  vuelta {i}      FALLO: {e}");
                return;
            }
        }
    }

    // La mediana y no la media: una vuelta que pilla el telefono ocupado se lleva la media y
    // no dice nada de lo que costaria normalmente.
    tiempos.sort_unstable();
    let mediana = tiempos[tiempos.len() / 2];
    println!("  mediana       {mediana:>8} ms");
    println!();
    println!("  Es lo que se paga UNA vez al abrir el volumen, y no baja con mejor red.");
    println!();
}

/// La red. Puede fallar por el verificador de certificados, y si es eso se dice cual es.
async fn fase2() {
    println!("-- Fase 2: las cuentas, por la red --");

    let backends = match multidrive_backend::live::configurados() {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => {
            println!("  No hay proveedores configurados: nada que probar aqui.");
            println!("  (empuja el fichero de configuracion y pon MULTIDRIVE_CONFIG)");
            return;
        }
        Err(e) => {
            println!("  Configuracion invalida: {e}");
            return;
        }
    };

    let backends: Vec<std::sync::Arc<dyn multidrive_backend::Backend>> = backends
        .into_iter()
        .map(|b| std::sync::Arc::new(b) as std::sync::Arc<dyn multidrive_backend::Backend>)
        .collect();

    println!(
        "  cuentas       {}",
        backends
            .iter()
            .map(|b| b.name().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let t = Instant::now();
    match multidrive_core::header::read(&backends).await {
        Ok(Some(h)) => {
            println!("  cabecera      {:>8} ms", t.elapsed().as_millis());
            println!("  topologia     {}", h.layout);
            println!();
            println!("  La red funciona desde aqui. Lo que falta para una aplicacion es la");
            println!("  interfaz, no la pila.");
        }
        Ok(None) => println!(
            "  No hay ningun volumen en esas cuentas ({} ms)",
            t.elapsed().as_millis()
        ),
        Err(e) => {
            let dicho = e.to_string();
            println!("  cabecera      FALLO tras {} ms", t.elapsed().as_millis());
            println!("  {dicho}");
            // Se nombra el motivo concreto en vez de dejar un error de TLS a secas, porque
            // este es **esperado** y confundirlo con un fallo del volumen costaria una tarde.
            if dicho.contains("certificate")
                || dicho.contains("CaUsedAsEndEntity")
                || dicho.contains("verifier")
                || dicho.contains("tls")
                || dicho.contains("TLS")
            {
                println!();
                println!("  Esto es lo previsto: `rustls-platform-verifier` busca los");
                println!("  certificados del sistema a traves de la JVM, y aqui no hay JVM.");
                println!("  Hace falta el componente Kotlin, o sea una aplicacion de verdad.");
            }
        }
    }
}
