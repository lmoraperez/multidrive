//! Adaptador de WinFsp.
//!
//! WinFsp llama a estos metodos desde **sus propios hilos y de forma sincrona**, mientras que
//! todo el nucleo es asincrono. El puente es un `Handle` de tokio y `block_on`: se puede
//! hacer porque los hilos de WinFsp no son hilos del runtime, asi que bloquear en ellos no
//! para ninguna tarea.
//!
//! Y bloquear aqui es lo correcto conceptualmente: para el sistema operativo, un `read` es
//! sincrono por definicion.
//!
//! # Por que las escrituras se acumulan en un buffer
//!
//! Los trozos son inmutables: escribir dentro de un fichero significa bajar el trozo,
//! parchearlo y subir uno nuevo. Si cada `write` del sistema fuera directo al volumen,
//! copiar un fichero de 16 MiB en bloques de 1 MiB reescribiria el mismo trozo dieciseis
//! veces: 256 MiB subidos para 16 MiB de fichero.
//!
//! Por eso cada descriptor abierto acumula los tramos escritos, los fusiona cuando son
//! contiguos —que es lo que hace una copia normal— y los materializa de una vez al cerrar.
//! Una copia secuencial acaba siendo **una** escritura por trozo.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context as _, Result};
use libredisk_core::tree::{Kind, Volume};
use tokio::runtime::Handle;
use winfsp::U16CStr;
use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo,
    VolumeInfo, WideNameInfo,
};
use winfsp::host::{FileSystemHost, FileSystemParams, FineGuard, VolumeParams};

const ATTR_DIRECTORIO: u32 = 0x0000_0010;
const ATTR_SOLO_LECTURA: u32 = 0x0000_0001;
const ATTR_NORMAL: u32 = 0x0000_0080;

/// Bit de `create_options` que pide crear un directorio y no un fichero.
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

/// Bit de `cleanup` que dice que el fichero se va a borrar.
const CLEANUP_DELETE: u32 = 0x0000_0001;

// Codigos NTSTATUS. El crate no los reexporta y traerse todo `windows` solo por unas
// constantes no compensa.
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
const STATUS_OBJECT_NAME_COLLISION: i32 = 0xC000_0035u32 as i32;
const STATUS_END_OF_FILE: i32 = 0xC000_0011u32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BAu32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103u32 as i32;
const STATUS_IO_DEVICE_ERROR: i32 = 0xC000_0185u32 as i32;
const STATUS_MEDIA_WRITE_PROTECTED: i32 = 0xC000_00A2u32 as i32;
const STATUS_DIRECTORY_NOT_EMPTY: i32 = 0xC000_0101u32 as i32;

/// Nombre del sistema de ficheros, tal y como lo vera Windows en las propiedades.
const NOMBRE_FS: &str = "LibreDisk";

/// Cuanto puede cachear Windows la informacion de un fichero, en milisegundos.
///
/// Sin cachear nada, abrir una carpeta en el Explorador dispara decenas de consultas, y cada
/// una puede ser una peticion a la nube. Un segundo es seguro porque el volumen tiene **un
/// solo escritor**: nadie mas puede cambiarlo por detras.
const CACHE_MS: u32 = 1000;

/// Instante actual en formato FILETIME de Windows: intervalos de 100 ns desde 1601.
///
/// Sin esto el Explorador fecha todo en 1601, que parece corrupcion.
fn ahora_filetime() -> u64 {
    const DESDE_1601: u64 = 116_444_736_000_000_000;
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100 + DESDE_1601)
        .unwrap_or(DESDE_1601)
}

/// Tramos escritos que todavia no se han llevado al volumen.
#[derive(Default)]
struct Buffer {
    /// Por offset de inicio. Los contiguos o solapados se fusionan al escribir, asi que una
    /// escritura secuencial deja un unico tramo.
    tramos: BTreeMap<u64, Vec<u8>>,
}

impl Buffer {
    fn escribir(&mut self, offset: u64, datos: &[u8]) {
        if datos.is_empty() {
            return;
        }
        let mut ini = offset;
        let mut fin = offset + datos.len() as u64;

        // Todo lo que toque o pegue con el rango nuevo se absorbe.
        let afectados: Vec<u64> = self
            .tramos
            .iter()
            .filter(|(o, d)| {
                let (a, b) = (**o, **o + d.len() as u64);
                b >= ini && a <= fin
            })
            .map(|(o, _)| *o)
            .collect();

        for o in &afectados {
            let d = &self.tramos[o];
            ini = ini.min(*o);
            fin = fin.max(*o + d.len() as u64);
        }

        let mut fusionado = vec![0u8; (fin - ini) as usize];
        for o in &afectados {
            let d = self.tramos.remove(o).expect("estaba en la lista");
            let desde = (*o - ini) as usize;
            fusionado[desde..desde + d.len()].copy_from_slice(&d);
        }
        // Lo nuevo manda sobre lo que hubiera.
        let desde = (offset - ini) as usize;
        fusionado[desde..desde + datos.len()].copy_from_slice(datos);

        self.tramos.insert(ini, fusionado);
    }

    fn vaciar(&mut self) -> Vec<(u64, Vec<u8>)> {
        std::mem::take(&mut self.tramos).into_iter().collect()
    }

    fn vacio(&self) -> bool {
        self.tramos.is_empty()
    }
}

/// Lo que se guarda por cada fichero o directorio abierto.
pub struct Abierto {
    ruta: String,
    es_dir: bool,
    /// Longitud actual, que cambia segun se escribe.
    len: AtomicU64,
    /// Buffer de entradas de directorio, que WinFsp pagina por su cuenta.
    dir: DirBuffer,
    escritura: Mutex<Buffer>,
    /// El sistema ha pedido borrar esto al cerrar.
    marcado_para_borrar: AtomicBool,
}

impl Abierto {
    fn nuevo(ruta: String, es_dir: bool, len: u64) -> Self {
        Self {
            ruta,
            es_dir,
            len: AtomicU64::new(len),
            dir: DirBuffer::new(),
            escritura: Mutex::new(Buffer::default()),
            marcado_para_borrar: AtomicBool::new(false),
        }
    }

    fn len(&self) -> u64 {
        self.len.load(Ordering::Relaxed)
    }

    fn escribir(&self, offset: u64, datos: &[u8]) {
        self.escritura
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .escribir(offset, datos);
    }
}

/// El sistema de ficheros: un volumen mas la forma de esperar a sus operaciones.
pub struct LibreDiskFs {
    volumen: Arc<RwLock<Volume>>,
    rt: Handle,
    solo_lectura: bool,
    creado: u64,
}

impl LibreDiskFs {
    fn esperar<F, T>(&self, futuro: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.rt.block_on(futuro)
    }

    /// Traduce una ruta de Windows a una del volumen.
    fn ruta(nombre: &U16CStr) -> String {
        let s = nombre.to_string_lossy().replace('\\', "/");
        if s.is_empty() { "/".to_string() } else { s }
    }

    fn rellenar(&self, info: &mut FileInfo, es_dir: bool, len: u64) {
        let base = if es_dir { ATTR_DIRECTORIO } else { ATTR_NORMAL };
        info.file_attributes = base
            | if self.solo_lectura {
                ATTR_SOLO_LECTURA
            } else {
                0
            };
        info.file_size = if es_dir { 0 } else { len };
        info.allocation_size = info.file_size;
        info.creation_time = self.creado;
        info.last_access_time = self.creado;
        info.last_write_time = self.creado;
        info.change_time = self.creado;
        info.index_number = 0;
        info.hard_links = 0;
    }

    fn mirar(&self, ruta: &str) -> Option<(bool, u64)> {
        if ruta == "/" {
            return Some((true, 0));
        }
        let v = self.volumen.read().ok()?;
        match self.esperar(v.lookup(ruta)) {
            Ok(Some(entrada)) => Some((entrada.kind == Kind::Dir, entrada.len)),
            _ => None,
        }
    }

    fn no_escribible(&self) -> winfsp::FspError {
        winfsp::FspError::NTSTATUS(STATUS_MEDIA_WRITE_PROTECTED)
    }

    fn eio() -> winfsp::FspError {
        winfsp::FspError::NTSTATUS(STATUS_IO_DEVICE_ERROR)
    }

    /// Lleva al volumen lo que haya en el buffer de un descriptor.
    ///
    /// Aqui es donde se paga la escritura de verdad. Un tramo contiguo se aplica de una sola
    /// vez, que es lo que hace que copiar un fichero escriba cada trozo una unica vez.
    fn materializar(&self, contexto: &Abierto) -> winfsp::Result<()> {
        let tramos = {
            let mut b = contexto.escritura.lock().unwrap_or_else(|e| e.into_inner());
            if b.vacio() {
                return Ok(());
            }
            b.vaciar()
        };

        let mut v = self.volumen.write().map_err(|_| Self::eio())?;
        for (offset, datos) in tramos {
            self.esperar(v.write_at(&contexto.ruta, offset, &datos))
                .map_err(|_| Self::eio())?;
        }
        Ok(())
    }
}

impl FileSystemContext for LibreDiskFs {
    type FileContext = Abierto;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [std::ffi::c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let ruta = Self::ruta(file_name);
        let (es_dir, _) = self
            .mirar(&ruta)
            .ok_or(winfsp::FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND))?;

        Ok(FileSecurity {
            reparse: false,
            // Sin ACLs: el volumen es de un solo usuario y va cifrado de extremo a extremo.
            sz_security_descriptor: 0,
            attributes: if es_dir { ATTR_DIRECTORIO } else { ATTR_NORMAL },
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let ruta = Self::ruta(file_name);
        let (es_dir, len) = self
            .mirar(&ruta)
            .ok_or(winfsp::FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND))?;

        self.rellenar(file_info.as_mut(), es_dir, len);
        Ok(Abierto::nuevo(ruta, es_dir, len))
    }

    #[allow(clippy::too_many_arguments)]
    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: u32,
        _file_attributes: u32,
        _security_descriptor: Option<&[std::ffi::c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        if self.solo_lectura {
            return Err(self.no_escribible());
        }

        let ruta = Self::ruta(file_name);
        if self.mirar(&ruta).is_some() {
            return Err(winfsp::FspError::NTSTATUS(STATUS_OBJECT_NAME_COLLISION));
        }

        let es_dir = create_options & FILE_DIRECTORY_FILE != 0;
        {
            let mut v = self.volumen.write().map_err(|_| Self::eio())?;
            let hecho = if es_dir {
                self.esperar(v.mkdir_p(&ruta))
            } else {
                self.esperar(v.create_file(&ruta))
            };
            hecho.map_err(|_| Self::eio())?;
        }

        self.rellenar(file_info.as_mut(), es_dir, 0);
        Ok(Abierto::nuevo(ruta, es_dir, 0))
    }

    fn close(&self, context: Self::FileContext) {
        // Por si acaso: `cleanup` ya deberia haberlo hecho, pero perder datos por confiar en
        // el orden de las llamadas seria un mal negocio.
        let _ = self.materializar(&context);
    }

    fn get_file_info(
        &self,
        context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        self.rellenar(file_info, context.es_dir, context.len());
        Ok(())
    }

    fn read(
        &self,
        context: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> winfsp::Result<u32> {
        if context.es_dir {
            return Err(winfsp::FspError::NTSTATUS(STATUS_FILE_IS_A_DIRECTORY));
        }

        // Lo escrito y todavia sin materializar no esta en el volumen: hay que bajarlo antes
        // de leer, o se devolveria el contenido viejo.
        self.materializar(context)?;

        let v = self.volumen.read().map_err(|_| Self::eio())?;
        let datos = self
            .esperar(v.read_at(&context.ruta, offset, buffer.len()))
            .map_err(|_| Self::eio())?;

        if datos.is_empty() {
            return Err(winfsp::FspError::NTSTATUS(STATUS_END_OF_FILE));
        }
        buffer[..datos.len()].copy_from_slice(&datos);
        Ok(datos.len() as u32)
    }

    fn write(
        &self,
        context: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        if self.solo_lectura {
            return Err(self.no_escribible());
        }
        if context.es_dir {
            return Err(winfsp::FspError::NTSTATUS(STATUS_FILE_IS_A_DIRECTORY));
        }

        let len = context.len();
        let destino = if write_to_eof { len } else { offset };

        // `constrained_io` significa que el fichero no puede crecer: se escribe lo que quepa.
        let datos = if constrained_io {
            if destino >= len {
                self.rellenar(file_info, false, len);
                return Ok(0);
            }
            let cabe = (len - destino) as usize;
            &buffer[..cabe.min(buffer.len())]
        } else {
            buffer
        };

        context.escribir(destino, datos);
        let nueva = len.max(destino + datos.len() as u64);
        context.len.store(nueva, Ordering::Relaxed);

        self.rellenar(file_info, false, nueva);
        Ok(datos.len() as u32)
    }

    fn overwrite(
        &self,
        context: &Self::FileContext,
        _file_attributes: u32,
        _replace_file_attributes: bool,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if self.solo_lectura {
            return Err(self.no_escribible());
        }

        // Lo que hubiera en el buffer sobra: se va a truncar a cero de todas formas.
        context
            .escritura
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .vaciar();

        let mut v = self.volumen.write().map_err(|_| Self::eio())?;
        self.esperar(v.truncate(&context.ruta, 0))
            .map_err(|_| Self::eio())?;

        context.len.store(0, Ordering::Relaxed);
        self.rellenar(file_info, false, 0);
        Ok(())
    }

    fn set_file_size(
        &self,
        context: &Self::FileContext,
        new_size: u64,
        set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if self.solo_lectura {
            return Err(self.no_escribible());
        }

        // El tamano reservado no significa nada aqui: no hay nada que reservar por adelantado.
        if set_allocation_size {
            self.rellenar(file_info, false, context.len());
            return Ok(());
        }

        self.materializar(context)?;
        let mut v = self.volumen.write().map_err(|_| Self::eio())?;
        self.esperar(v.truncate(&context.ruta, new_size))
            .map_err(|_| Self::eio())?;

        context.len.store(new_size, Ordering::Relaxed);
        self.rellenar(file_info, false, new_size);
        Ok(())
    }

    fn flush(
        &self,
        context: Option<&Self::FileContext>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        if let Some(c) = context {
            self.materializar(c)?;
            self.rellenar(file_info, c.es_dir, c.len());
        }
        Ok(())
    }

    fn set_delete(
        &self,
        context: &Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> winfsp::Result<()> {
        if self.solo_lectura {
            return Err(self.no_escribible());
        }
        // Un directorio con cosas dentro no se borra, y hay que decirlo **ahora**: en
        // `cleanup` ya no hay forma de devolver un error.
        if delete_file && context.es_dir {
            let v = self.volumen.read().map_err(|_| Self::eio())?;
            let vacio = self
                .esperar(v.read_dir(&context.ruta))
                .map(|e| e.is_empty())
                .unwrap_or(false);
            if !vacio {
                return Err(winfsp::FspError::NTSTATUS(STATUS_DIRECTORY_NOT_EMPTY));
            }
        }
        context
            .marcado_para_borrar
            .store(delete_file, Ordering::Relaxed);
        Ok(())
    }

    fn cleanup(&self, context: &Self::FileContext, _file_name: Option<&U16CStr>, flags: u32) {
        let borrar =
            flags & CLEANUP_DELETE != 0 || context.marcado_para_borrar.load(Ordering::Relaxed);

        if borrar && !self.solo_lectura {
            if let Ok(mut v) = self.volumen.write() {
                let _ = self.esperar(v.remove(&context.ruta));
            }
            return;
        }
        // Aqui es donde una copia normal materializa: WinFsp llama a `cleanup` al cerrar el
        // ultimo descriptor.
        let _ = self.materializar(context);
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if !context.es_dir {
            return Err(winfsp::FspError::NTSTATUS(STATUS_NOT_A_DIRECTORY));
        }

        if let Ok(lock) = context.dir.acquire(marker.is_none(), None) {
            let v = self.volumen.read().map_err(|_| Self::eio())?;
            let entradas = self
                .esperar(v.read_dir(&context.ruta))
                .map_err(|_| Self::eio())?;

            for (nombre, entrada) in entradas {
                let mut info: DirInfo<255> = DirInfo::new();
                let es_dir = entrada.kind == Kind::Dir;
                self.rellenar(info.file_info_mut(), es_dir, entrada.len);
                info.set_name(nombre.as_str())?;
                lock.write(&mut info)?;
            }
        }

        Ok(context.dir.read(marker, buffer))
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        // No hay forma honesta de dar un tamano: depende de la cuota de cada proveedor y de
        // la topologia. Se declara algo grande, que es lo que hacen los sistemas de ficheros
        // de red cuando no lo saben.
        out_volume_info.total_size = u64::MAX / 2;
        out_volume_info.free_size = u64::MAX / 2;
        out_volume_info.set_volume_label(NOMBRE_FS);
        Ok(())
    }
}

/// Un montaje vivo. Al soltarlo, se desmonta.
pub struct Montaje {
    // FineGuard explicito: hay dos estrategias y las dos tienen `start`, asi que sin
    // anotarlo el tipo queda ambiguo. FineGuard permite operaciones concurrentes, que es lo
    // que interesa cuando cada una espera a la red.
    host: FileSystemHost<LibreDiskFs, FineGuard>,
}

impl Drop for Montaje {
    fn drop(&mut self) {
        self.host.unmount();
        self.host.stop();
    }
}

/// Monta el volumen en un punto de montaje: una letra (`Z:`) o un directorio vacio.
pub fn montar(volumen: Arc<RwLock<Volume>>, punto: &str, solo_lectura: bool) -> Result<Montaje> {
    winfsp::winfsp_init()
        .map_err(|e| anyhow::anyhow!("{e:?}"))
        .context(
            "no se pudo inicializar WinFsp.\n\
             Comprueba que esta instalado y que su carpeta 'bin' esta en el PATH:\n\
               C:\\Program Files (x86)\\WinFsp\\bin",
        )?;

    let mut params = VolumeParams::new();
    params
        .filesystem_name(NOMBRE_FS)
        .sector_size(4096)
        .sectors_per_allocation_unit(1)
        .max_component_length(255)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .read_only_volume(solo_lectura)
        .file_info_timeout(CACHE_MS);

    let fs = LibreDiskFs {
        volumen,
        rt: Handle::current(),
        solo_lectura,
        creado: ahora_filetime(),
    };

    let mut host: FileSystemHost<LibreDiskFs, FineGuard> = FileSystemHost::new_with_options(
        FileSystemParams {
            use_dir_info_by_name: false,
            volume_params: params,
            debug_mode: Default::default(),
        },
        fs,
    )
    .map_err(|e| anyhow::anyhow!("no se pudo crear el sistema de ficheros: {e:?}"))?;

    host.mount(punto)
        .map_err(|e| anyhow::anyhow!("no se pudo montar en '{punto}': {e:?}"))?;
    host.start()
        .map_err(|e| anyhow::anyhow!("no se pudo arrancar el montaje: {e:?}"))?;

    Ok(Montaje { host })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El buffer es lo que evita reescribir el mismo trozo una vez por bloque.
    #[test]
    fn los_tramos_contiguos_se_fusionan() {
        let mut b = Buffer::default();
        b.escribir(0, b"hola ");
        b.escribir(5, b"mundo");

        let tramos = b.vaciar();
        assert_eq!(tramos.len(), 1, "una copia secuencial deja un solo tramo");
        assert_eq!(tramos[0], (0, b"hola mundo".to_vec()));
    }

    #[test]
    fn los_tramos_separados_no_se_fusionan() {
        let mut b = Buffer::default();
        b.escribir(0, b"aaa");
        b.escribir(100, b"bbb");

        assert_eq!(b.vaciar().len(), 2);
    }

    #[test]
    fn lo_nuevo_pisa_lo_viejo_al_solaparse() {
        let mut b = Buffer::default();
        b.escribir(0, b"aaaaaaaa");
        b.escribir(2, b"XX");

        let tramos = b.vaciar();
        assert_eq!(tramos.len(), 1);
        assert_eq!(tramos[0].1, b"aaXXaaaa".to_vec());
    }

    /// Escribir hacia atras tambien tiene que fusionar, no dejar dos tramos.
    #[test]
    fn fusiona_tambien_hacia_atras() {
        let mut b = Buffer::default();
        b.escribir(5, b"mundo");
        b.escribir(0, b"hola ");

        let tramos = b.vaciar();
        assert_eq!(tramos.len(), 1);
        assert_eq!(tramos[0], (0, b"hola mundo".to_vec()));
    }

    #[test]
    fn vaciar_deja_el_buffer_limpio() {
        let mut b = Buffer::default();
        b.escribir(0, b"x");
        assert!(!b.vacio());
        b.vaciar();
        assert!(b.vacio());
    }
}
