//! Monta un volumen de verdad con FUSE y lo usa como lo usaria un programa cualquiera.
//!
//! Los tests de `windows.rs` comprueban la traduccion de rutas sin montar nada, que es logica
//! pura. Esto es lo contrario: no mira ninguna funcion por dentro, solo abre, escribe, lista y
//! borra con `std::fs` sobre el punto de montaje. Si algo del adaptador esta mal, aqui se nota
//! como se notaria en la maquina de alguien.
//!
//! # Por que va marcado como ignorado
//!
//! Necesita `/dev/fuse` y permiso para montar, y eso no lo hay en cualquier sitio: en un
//! contenedor sin privilegios hace falta root. Un test que falla por el entorno y no por el
//! codigo entrena a la gente a ignorar los fallos, asi que se pide a mano:
//!
//! ```sh
//! cargo test -p libredisk-mount --test montaje_fuse -- --ignored --nocapture
//! ```
//!
//! # Por que el runtime es multihilo y las llamadas van en `spawn_blocking`
//!
//! No es ceremonia. Cada operacion sobre el montaje entra al nucleo, sale por un hilo de FUSE
//! y ese hilo bloquea sobre el runtime. Si el hilo que hace el `std::fs::read` fuera el unico
//! del runtime, se quedaria esperando a un trabajo que solo el podria atender: un interbloqueo
//! perfecto. Es la misma razon por la que funciona en produccion --los hilos de FUSE no son
//! del runtime-- y el test tiene que reproducir esa condicion o no prueba lo mismo.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use libredisk_backend::{Backend, OpendalBackend};
use libredisk_core::ChunkStore;
use libredisk_core::tree::Volume;
use libredisk_crypto::MasterKey;
use libredisk_raid::Layout;

/// Un directorio temporal que se borra solo, tambien si el test entra en panico.
struct Temporal(PathBuf);

impl Temporal {
    fn nuevo(nombre: &str) -> Self {
        let d = std::env::temp_dir().join(format!(
            "libredisk-fuse-{}-{nombre}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&d).expect("crear el directorio temporal");
        Self(d)
    }

    fn ruta(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temporal {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Un volumen en memoria con lo que se le diga dentro.
async fn volumen(ficheros: &[(&str, &[u8])]) -> Arc<RwLock<Volume>> {
    let backends: Vec<Arc<dyn Backend>> = vec![Arc::new(OpendalBackend::memory("prueba").unwrap())];
    // Argon2id con los parametros de verdad tarda segundos y aqui no aporta nada: lo que se
    // prueba es el montaje, no la derivacion de clave.
    let key = MasterKey::derive_with("frase", b"sal-de-prueba-16", 8, 1).unwrap();
    let store = ChunkStore::new(Layout::Concat, backends, key).unwrap();

    let (mut v, _) = Volume::create(store).await.unwrap();
    for (ruta, datos) in ficheros {
        v.write_file(ruta, *datos).await.unwrap();
    }
    Arc::new(RwLock::new(v))
}

/// Ejecuta las operaciones de fichero fuera del runtime, que es de donde llega FUSE.
async fn sobre_el_montaje<F, T>(f: F) -> T
where
    F: FnOnce(&Path) -> T + Send + 'static,
    T: Send + 'static,
{
    let punto = Temporal::nuevo("monte");
    let v = volumen(&[
        ("/chico.txt", b"hola desde fuera"),
        ("/dir/hondo/dato.bin", b"algo"),
    ])
    .await;

    let montaje = libredisk_mount::montar(v, &punto.ruta().to_string_lossy(), false)
        .expect("montar: hace falta /dev/fuse y permiso para montarlo");

    let ruta = punto.ruta().to_path_buf();
    let r = tokio::task::spawn_blocking(move || f(&ruta)).await;

    // Desmontar antes de borrar el directorio, o se borraria el punto de montaje con el
    // sistema de ficheros todavia encima.
    drop(montaje);
    r.expect("las operaciones sobre el montaje entraron en panico")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita /dev/fuse y permiso para montar"]
async fn se_monta_y_se_lee_lo_que_habia() {
    sobre_el_montaje(|m| {
        let mut nombres: Vec<String> = std::fs::read_dir(m)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        nombres.sort();
        assert_eq!(nombres, vec!["chico.txt", "dir"]);

        assert_eq!(
            std::fs::read(m.join("chico.txt")).unwrap(),
            b"hola desde fuera"
        );
        assert_eq!(
            std::fs::read(m.join("dir/hondo/dato.bin")).unwrap(),
            b"algo"
        );
        assert!(m.join("dir/hondo").is_dir());
    })
    .await;
}

/// Un fichero que cruza varios trozos, para que el camino de lectura por rangos entre de
/// verdad: con 4 KiB nunca se llega a pedir el segundo trozo.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita /dev/fuse y permiso para montar"]
async fn un_fichero_grande_va_y_vuelve_identico() {
    sobre_el_montaje(|m| {
        // Pseudoaleatorio y reproducible: un patron de ceros no distinguiria un trozo mal
        // colocado de uno bien colocado.
        let datos: Vec<u8> = (0..3_000_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();

        let destino = m.join("grande.bin");
        std::fs::write(&destino, &datos).unwrap();
        assert_eq!(
            std::fs::metadata(&destino).unwrap().len(),
            datos.len() as u64
        );
        assert_eq!(
            std::fs::read(&destino).unwrap(),
            datos,
            "el contenido cambio"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita /dev/fuse y permiso para montar"]
async fn crear_directorios_escribir_y_borrar() {
    sobre_el_montaje(|m| {
        std::fs::create_dir_all(m.join("a/b/c")).unwrap();
        std::fs::write(m.join("a/b/c/nuevo.txt"), b"escrito por fuse").unwrap();
        assert_eq!(
            std::fs::read(m.join("a/b/c/nuevo.txt")).unwrap(),
            b"escrito por fuse"
        );

        std::fs::remove_file(m.join("a/b/c/nuevo.txt")).unwrap();
        assert!(!m.join("a/b/c/nuevo.txt").exists());
        std::fs::remove_dir(m.join("a/b/c")).unwrap();
        assert!(!m.join("a/b/c").exists());
    })
    .await;
}

/// Renombrar un directorio tiene que arrastrar la tabla de inodos entera.
///
/// Es el caso que mas facil seria romper sin enterarse. **Se lee el hijo antes de renombrar a
/// proposito**: eso hace que el nucleo se quede con su inodo, y a partir de ahi ese numero
/// tiene que seguir valiendo. Si `renombrar` solo moviera la entrada del directorio, el hijo
/// quedaria apuntando a una ruta que ya no existe y el descriptor abierto sobre el empezaria a
/// dar «no existe» aunque el fichero siga ahi con otro nombre.
///
/// Renombrando primero y leyendo despues el test pasaria igual con la tabla rota, porque cada
/// busqueda nueva se recompone desde el padre. Por eso el orden importa.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita /dev/fuse y permiso para montar"]
async fn renombrar_un_directorio_no_pierde_lo_de_dentro() {
    sobre_el_montaje(|m| {
        use std::io::{Read, Seek, SeekFrom};

        // Primero se toca al hijo, para que el nucleo tenga su inodo.
        let hijo = m.join("dir/hondo/dato.bin");
        assert_eq!(std::fs::read(&hijo).unwrap(), b"algo");
        let mut abierto = std::fs::File::open(&hijo).unwrap();

        std::fs::rename(m.join("dir"), m.join("movido")).unwrap();

        // El descriptor que ya estaba abierto sigue sirviendo el mismo contenido.
        let mut leido = Vec::new();
        abierto.seek(SeekFrom::Start(0)).unwrap();
        abierto
            .read_to_end(&mut leido)
            .expect("el descriptor abierto dejo de servir tras renombrar el directorio padre");
        assert_eq!(leido, b"algo");

        assert!(!m.join("dir").exists());
        assert_eq!(
            std::fs::read(m.join("movido/hondo/dato.bin")).unwrap(),
            b"algo",
            "el contenido tenia que viajar con el directorio"
        );
    })
    .await;
}

/// Los codigos de error concretos, que no son un detalle: son los que distinguen a un `rm -r`
/// que funciona de uno que se rinde a la primera.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita /dev/fuse y permiso para montar"]
async fn los_errores_son_los_que_espera_posix() {
    sobre_el_montaje(|m| {
        use std::io::ErrorKind;

        let e = std::fs::remove_dir(m.join("dir")).unwrap_err();
        assert_eq!(
            e.raw_os_error(),
            Some(libc::ENOTEMPTY),
            "rmdir de un directorio con cosas dentro: {e}"
        );

        let e = std::fs::remove_dir(m.join("chico.txt")).unwrap_err();
        assert_eq!(
            e.raw_os_error(),
            Some(libc::ENOTDIR),
            "rmdir sobre un fichero: {e}"
        );

        let e = std::fs::remove_file(m.join("dir")).unwrap_err();
        assert_eq!(
            e.raw_os_error(),
            Some(libc::EISDIR),
            "unlink sobre un directorio: {e}"
        );

        let e = std::fs::read(m.join("no-existe")).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::NotFound);
    })
    .await;
}

/// En solo lectura, escribir tiene que fallar con EROFS y no con un error cualquiera: es lo
/// que hace que un programa distinga «no se puede escribir aqui» de «algo se rompio».
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "necesita /dev/fuse y permiso para montar"]
async fn en_solo_lectura_no_se_escribe() {
    let punto = Temporal::nuevo("solo-lectura");
    let v = volumen(&[("/chico.txt", b"intacto")]).await;
    let montaje = libredisk_mount::montar(v, &punto.ruta().to_string_lossy(), true)
        .expect("montar: hace falta /dev/fuse y permiso para montarlo");

    let ruta = punto.ruta().to_path_buf();
    let r = tokio::task::spawn_blocking(move || {
        assert_eq!(std::fs::read(ruta.join("chico.txt")).unwrap(), b"intacto");
        let e = std::fs::write(ruta.join("nuevo.txt"), b"x").unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::EROFS), "escribir: {e}");
    })
    .await;

    drop(montaje);
    r.expect("las operaciones sobre el montaje entraron en panico");
}
