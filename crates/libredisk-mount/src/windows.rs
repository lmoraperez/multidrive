//! Adaptador de WinFsp.
//!
//! WinFsp llama a estos metodos desde **sus propios hilos y de forma sincrona**, mientras que
//! todo el nucleo es asincrono. El puente es un `Handle` de tokio y `block_on`: se puede
//! hacer porque los hilos de WinFsp no son hilos del runtime, asi que bloquear en ellos no
//! para ninguna tarea.
//!
//! Y bloquear aqui es lo correcto conceptualmente: para el sistema operativo, un `read` es
//! sincrono por definicion. Lo que no es correcto es lo que tarda, y eso no lo arregla este
//! fichero sino la cache que todavia no existe.

use std::sync::Arc;

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

// Codigos NTSTATUS. El crate no los reexporta y traerse todo `windows` solo por cinco
// constantes no compensa.
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
const STATUS_END_OF_FILE: i32 = 0xC000_0011u32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BAu32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103u32 as i32;
const STATUS_IO_DEVICE_ERROR: i32 = 0xC000_0185u32 as i32;
const ATTR_SOLO_LECTURA: u32 = 0x0000_0001;

/// Nombre del sistema de ficheros, tal y como lo vera Windows en las propiedades.
const NOMBRE_FS: &str = "LibreDisk";

/// Cuanto puede cachear Windows la informacion de un fichero, en milisegundos.
///
/// Aqui no es un ajuste fino: sin cachear nada, abrir una carpeta en el Explorador dispara
/// decenas de consultas, y cada una son segundos contra la nube. Un segundo de cache es
/// seguro porque el volumen tiene **un solo escritor**: nadie mas puede cambiarlo por detras.
const CACHE_MS: u32 = 1000;

/// Lo que se guarda por cada fichero o directorio abierto.
pub struct Abierto {
    ruta: String,
    es_dir: bool,
    len: u64,
    /// Buffer de entradas de directorio, que WinFsp pagina por su cuenta.
    dir: DirBuffer,
}

/// El sistema de ficheros: un volumen mas la forma de esperar a sus operaciones.
pub struct LibreDiskFs {
    volumen: Arc<Volume>,
    rt: Handle,
}

impl LibreDiskFs {
    fn esperar<F, T>(&self, futuro: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.rt.block_on(futuro)
    }

    /// Traduce una ruta de Windows a una del volumen.
    ///
    /// WinFsp las entrega en UTF-16 y con barras invertidas; el volumen las quiere con barras
    /// normales. La raiz llega como `\` y se convierte en `/`.
    fn ruta(nombre: &U16CStr) -> String {
        let s = nombre.to_string_lossy().replace('\\', "/");
        if s.is_empty() { "/".to_string() } else { s }
    }

    /// Rellena la informacion que Windows espera de un fichero o directorio.
    fn rellenar(info: &mut FileInfo, es_dir: bool, len: u64) {
        info.file_attributes = if es_dir {
            ATTR_DIRECTORIO | ATTR_SOLO_LECTURA
        } else {
            ATTR_SOLO_LECTURA
        };
        info.file_size = if es_dir { 0 } else { len };
        // Sin cache local no hay nada que reservar por adelantado: lo asignado es lo que hay.
        info.allocation_size = info.file_size;
        info.index_number = 0;
        info.hard_links = 0;
    }

    /// Busca una ruta y dice si es directorio y cuanto mide.
    fn mirar(&self, ruta: &str) -> Option<(bool, u64)> {
        if ruta == "/" {
            return Some((true, 0));
        }
        match self.esperar(self.volumen.lookup(ruta)) {
            Ok(Some(entrada)) => Some((entrada.kind == Kind::Dir, entrada.len)),
            _ => None,
        }
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
            attributes: if es_dir {
                ATTR_DIRECTORIO | ATTR_SOLO_LECTURA
            } else {
                ATTR_SOLO_LECTURA
            },
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        // `FILE_ACCESS_RIGHTS` del binding es un alias de u32.
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let ruta = Self::ruta(file_name);
        let (es_dir, len) = self
            .mirar(&ruta)
            .ok_or(winfsp::FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND))?;

        Self::rellenar(file_info.as_mut(), es_dir, len);
        Ok(Abierto {
            ruta,
            es_dir,
            len,
            dir: DirBuffer::new(),
        })
    }

    fn close(&self, _context: Self::FileContext) {}

    fn get_file_info(
        &self,
        context: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        Self::rellenar(file_info, context.es_dir, context.len);
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

        let datos = self
            .esperar(self.volumen.read_at(&context.ruta, offset, buffer.len()))
            .map_err(|_| winfsp::FspError::NTSTATUS(STATUS_IO_DEVICE_ERROR))?;

        if datos.is_empty() {
            return Err(winfsp::FspError::NTSTATUS(STATUS_END_OF_FILE));
        }
        buffer[..datos.len()].copy_from_slice(&datos);
        Ok(datos.len() as u32)
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

        // Se llena una sola vez por handle abierto; despues WinFsp lo pagina solo.
        if let Ok(lock) = context.dir.acquire(marker.is_none(), None) {
            let entradas = self
                .esperar(self.volumen.read_dir(&context.ruta))
                .map_err(|_| winfsp::FspError::NTSTATUS(STATUS_IO_DEVICE_ERROR))?;

            for (nombre, entrada) in entradas {
                let mut info: DirInfo<255> = DirInfo::new();
                let es_dir = entrada.kind == Kind::Dir;
                Self::rellenar(info.file_info_mut(), es_dir, entrada.len);
                info.set_name(nombre.as_str())?;
                lock.write(&mut info)?;
            }
        }

        Ok(context.dir.read(marker, buffer))
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        // No hay forma honesta de dar un tamano: depende de la cuota de cada proveedor y de
        // la topologia. Se declara algo grande y se dice el mismo numero como libre, que es
        // lo que hacen los sistemas de ficheros de red cuando no lo saben.
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
pub fn montar(volumen: Arc<Volume>, punto: &str) -> Result<Montaje> {
    winfsp::winfsp_init()
        .context(
            "no se pudo inicializar WinFsp.\n\
             Comprueba que esta instalado y que su carpeta 'bin' esta en el PATH:\n\
               C:\\Program Files (x86)\\WinFsp\\bin",
        )
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let mut params = VolumeParams::new();
    params
        .filesystem_name(NOMBRE_FS)
        .sector_size(4096)
        .sectors_per_allocation_unit(1)
        .max_component_length(255)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .read_only_volume(true)
        .file_info_timeout(CACHE_MS);

    let fs = LibreDiskFs {
        volumen,
        rt: Handle::current(),
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
