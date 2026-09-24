//! Adaptador de FUSE.
//!
//! Hace lo mismo que el de WinFsp y con las mismas reglas —bloquear en los hilos del kernel
//! es correcto, escribir se acumula por descriptor y se materializa al cerrar— pero con una
//! diferencia que lo cambia todo: **FUSE no habla de rutas, habla de inodos.**
//!
//! WinFsp entrega la ruta completa en cada llamada. FUSE entrega un numero, y espera que el
//! sistema de ficheros sepa a que corresponde. Como el arbol de MultiDrive esta indexado por
//! ruta, hace falta una tabla que traduzca en los dos sentidos, y mantenerla al dia cuando
//! algo se renombra o se borra. Eso es [`Inodos`], y es la unica pieza que no tiene
//! equivalente en el lado de Windows.
//!
//! # Lo que aqui no hace falta
//!
//! Traducir mayusculas. En Linux un sistema de ficheros distingue, el arbol distingue, y no
//! hay nada que reconciliar. Que el nucleo se mantuviera sensible a mayusculas —y que la
//! traduccion viviera en la capa de montaje de Windows y no mas abajo— es lo que hace que
//! este fichero no tenga que saber nada del asunto.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};
use fuser::{
    BackgroundSession, Errno, FileAttr, FileHandle, FileType, Generation, INodeNo, MountOption,
    OpenFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry,
    ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use multidrive_core::tree::{Kind, Volume};
use tokio::runtime::Handle;

use crate::capacidad::{Capacidad, DESCONOCIDO};

/// Nombre con el que aparece en `mount` y en `df`.
const NOMBRE_FS: &str = "multidrive";

/// El inodo de la raiz lo fija el protocolo: siempre 1.
const RAIZ: u64 = 1;

/// Cuanto puede fiarse el nucleo de lo que le decimos, igual que en Windows y por lo mismo:
/// el volumen tiene **un solo escritor**, asi que nadie puede cambiarlo por detras.
const TTL: Duration = Duration::from_secs(1);

/// Tamano de bloque que se declara. No cambia nada de como se guarda: es lo que `stat` divide
/// para dar el numero de bloques.
const BLOQUE: u32 = 4096;

/// Tramos escritos que todavia no se han llevado al volumen.
///
/// Identico en proposito al de Windows: los trozos son inmutables, asi que escribir dentro de
/// un fichero significa bajar el trozo, parchearlo y subir otro. Sin acumular, copiar 16 MiB
/// en bloques de 128 KiB reescribiria el mismo trozo ciento veintiocho veces.
#[derive(Default)]
struct Buffer {
    tramos: std::collections::BTreeMap<u64, Vec<u8>>,
}

impl Buffer {
    fn escribir(&mut self, offset: u64, datos: &[u8]) {
        if datos.is_empty() {
            return;
        }
        let mut ini = offset;
        let mut fin = offset + datos.len() as u64;

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

/// La traduccion entre los numeros que usa FUSE y las rutas que usa el arbol.
///
/// Un inodo, una vez entregado al nucleo, tiene que seguir valiendo hasta que el nucleo lo
/// olvide: puede tener un descriptor abierto sobre el. Por eso renombrar **mueve** la entrada
/// en vez de crear otra, y borrar no reutiliza el numero.
#[derive(Default)]
struct Inodos {
    por_ruta: HashMap<String, u64>,
    por_ino: HashMap<u64, String>,
    siguiente: u64,
}

impl Inodos {
    fn nuevo() -> Self {
        let mut i = Self {
            siguiente: RAIZ + 1,
            ..Default::default()
        };
        i.por_ruta.insert("/".to_string(), RAIZ);
        i.por_ino.insert(RAIZ, "/".to_string());
        i
    }

    /// El numero de una ruta, creandolo si es la primera vez que se ve.
    fn asignar(&mut self, ruta: &str) -> u64 {
        if let Some(n) = self.por_ruta.get(ruta) {
            return *n;
        }
        let n = self.siguiente;
        self.siguiente += 1;
        self.por_ruta.insert(ruta.to_string(), n);
        self.por_ino.insert(n, ruta.to_string());
        n
    }

    fn ruta(&self, ino: u64) -> Option<String> {
        self.por_ino.get(&ino).cloned()
    }

    fn olvidar(&mut self, ruta: &str) {
        if let Some(n) = self.por_ruta.remove(ruta) {
            self.por_ino.remove(&n);
        }
    }

    /// Mueve una ruta y, si es un directorio, **todo lo que colgaba de ella**.
    ///
    /// Sin lo segundo, renombrar una carpeta dejaria a sus hijos apuntando a una ruta que ya
    /// no existe: cualquier descriptor abierto sobre ellos empezaria a dar «no existe» aunque
    /// el fichero siga ahi con otro nombre.
    fn renombrar(&mut self, viejo: &str, nuevo: &str) {
        let prefijo = format!("{}/", viejo.trim_end_matches('/'));
        let afectadas: Vec<String> = self
            .por_ruta
            .keys()
            .filter(|r| r.as_str() == viejo || r.starts_with(&prefijo))
            .cloned()
            .collect();

        for vieja in afectadas {
            let Some(n) = self.por_ruta.remove(&vieja) else {
                continue;
            };
            let nueva = if vieja == viejo {
                nuevo.to_string()
            } else {
                format!("{nuevo}{}", &vieja[viejo.len()..])
            };
            self.por_ruta.insert(nueva.clone(), n);
            self.por_ino.insert(n, nueva);
        }
    }
}

/// Un descriptor abierto.
struct Abierto {
    ruta: String,
    escritura: Mutex<Buffer>,
}

/// El sistema de ficheros.
pub struct MultiDriveFuse {
    volumen: Arc<RwLock<Volume>>,
    /// El tamano que se le declara al sistema, medido de fondo. Ver [`crate::capacidad`].
    capacidad: Capacidad,
    rt: Handle,
    solo_lectura: bool,
    inodos: Mutex<Inodos>,
    abiertos: Mutex<HashMap<u64, Arc<Abierto>>>,
    siguiente_fh: AtomicU64,
    creado: SystemTime,
    uid: u32,
    gid: u32,
}

impl MultiDriveFuse {
    fn esperar<F, T>(&self, futuro: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.rt.block_on(futuro)
    }

    /// Une un directorio con un nombre, sin dejar barras dobles.
    fn unir(padre: &str, nombre: &str) -> String {
        if padre == "/" {
            format!("/{nombre}")
        } else {
            format!("{padre}/{nombre}")
        }
    }

    fn ruta_de(&self, ino: INodeNo) -> Option<String> {
        self.inodos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ruta(ino.0)
    }

    fn ino_de(&self, ruta: &str) -> u64 {
        self.inodos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .asignar(ruta)
    }

    fn atributos(&self, ino: u64, es_dir: bool, len: u64) -> FileAttr {
        FileAttr {
            ino: INodeNo(ino),
            size: if es_dir { 0 } else { len },
            blocks: if es_dir {
                0
            } else {
                len.div_ceil(u64::from(BLOQUE))
            },
            atime: self.creado,
            mtime: self.creado,
            ctime: self.creado,
            crtime: self.creado,
            kind: if es_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            // Sin bits de grupo ni de otros: el volumen es de quien lo monta, y va cifrado de
            // extremo a extremo. Poner permisos que el sistema no puede hacer cumplir seria
            // prometer un control de acceso que no existe.
            perm: match (es_dir, self.solo_lectura) {
                (true, true) => 0o500,
                (true, false) => 0o700,
                (false, true) => 0o400,
                (false, false) => 0o600,
            },
            nlink: if es_dir { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: BLOQUE,
            flags: 0,
        }
    }

    /// Mira una ruta en el arbol y devuelve lo que necesita `stat`.
    fn mirar(&self, ruta: &str) -> Option<(bool, u64)> {
        if ruta == "/" {
            return Some((true, 0));
        }
        let v = self.volumen.read().ok()?;
        match self.esperar(v.lookup(ruta)) {
            Ok(Some(e)) => Some((e.kind == Kind::Dir, e.len)),
            _ => None,
        }
    }

    /// Lleva al volumen lo que haya acumulado un descriptor.
    fn materializar(&self, abierto: &Abierto) -> Result<(), Errno> {
        let tramos = {
            let mut b = abierto.escritura.lock().unwrap_or_else(|e| e.into_inner());
            if b.vacio() {
                return Ok(());
            }
            b.vaciar()
        };

        let mut v = self.volumen.write().map_err(|_| Errno::EIO)?;
        for (offset, datos) in tramos {
            self.esperar(v.write_at(&abierto.ruta, offset, &datos))
                .map_err(|_| Errno::EIO)?;
        }
        Ok(())
    }

    fn abierto(&self, fh: FileHandle) -> Option<Arc<Abierto>> {
        self.abiertos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&fh.0)
            .cloned()
    }

    fn abrir_descriptor(&self, ruta: String) -> FileHandle {
        let fh = self.siguiente_fh.fetch_add(1, Ordering::Relaxed);
        self.abiertos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                fh,
                Arc::new(Abierto {
                    ruta,
                    escritura: Mutex::new(Buffer::default()),
                }),
            );
        FileHandle(fh)
    }

    /// Escribir con el volumen montado en solo lectura.
    fn de_solo_lectura(&self) -> bool {
        self.solo_lectura
    }
}

impl fuser::Filesystem for MultiDriveFuse {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(padre) = self.ruta_de(parent) else {
            return reply.error(Errno::ENOENT);
        };
        let Some(nombre) = name.to_str() else {
            // El arbol guarda los nombres como cadenas de texto. Un nombre que no es UTF-8
            // valido no es que no exista: es que no cabe, y decir que no existe es la
            // respuesta honesta.
            return reply.error(Errno::ENOENT);
        };
        let ruta = Self::unir(&padre, nombre);
        let Some((es_dir, len)) = self.mirar(&ruta) else {
            return reply.error(Errno::ENOENT);
        };
        let ino = self.ino_de(&ruta);
        reply.entry(&TTL, &self.atributos(ino, es_dir, len), Generation(0));
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let Some(ruta) = self.ruta_de(ino) else {
            return reply.error(Errno::ENOENT);
        };
        let Some((es_dir, len)) = self.mirar(&ruta) else {
            return reply.error(Errno::ENOENT);
        };
        reply.attr(&TTL, &self.atributos(ino.0, es_dir, len));
    }

    /// De todo lo que `setattr` puede cambiar, aqui solo significa algo el tamano.
    ///
    /// Los permisos y el dueno no se guardan —el volumen no tiene metadatos POSIX— y las
    /// fechas tampoco. Fallar por eso rompeatajos tan normales como `touch` o `cp -p`, asi
    /// que se aceptan sin guardarlos y se devuelve lo que si es verdad.
    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let Some(ruta) = self.ruta_de(ino) else {
            return reply.error(Errno::ENOENT);
        };

        if let Some(nueva) = size {
            if self.de_solo_lectura() {
                return reply.error(Errno::EROFS);
            }
            // Lo pendiente en el buffer es anterior al truncado: materializarlo despues lo
            // desharia, escribiendo mas alla del nuevo final.
            if let Some(fh) = fh
                && let Some(a) = self.abierto(fh)
                && let Err(e) = self.materializar(&a)
            {
                return reply.error(e);
            }
            let Ok(mut v) = self.volumen.write() else {
                return reply.error(Errno::EIO);
            };
            if self.esperar(v.truncate(&ruta, nueva)).is_err() {
                return reply.error(Errno::EIO);
            }
        }

        let Some((es_dir, len)) = self.mirar(&ruta) else {
            return reply.error(Errno::ENOENT);
        };
        reply.attr(&TTL, &self.atributos(ino.0, es_dir, len));
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let Some(ruta) = self.ruta_de(ino) else {
            return reply.error(Errno::ENOENT);
        };
        let Ok(v) = self.volumen.read() else {
            return reply.error(Errno::EIO);
        };
        let Ok(entradas) = self.esperar(v.read_dir(&ruta)) else {
            return reply.error(Errno::ENOTDIR);
        };
        drop(v);

        // `.` y `..` no estan en el arbol pero el nucleo los espera, y `ls -a` los ensena.
        let mut todas: Vec<(u64, FileType, String)> = vec![
            (ino.0, FileType::Directory, ".".to_string()),
            (ino.0, FileType::Directory, "..".to_string()),
        ];
        for (nombre, e) in entradas {
            let hijo = Self::unir(&ruta, &nombre);
            let n = self.ino_de(&hijo);
            let tipo = if e.kind == Kind::Dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            todas.push((n, tipo, nombre));
        }

        // El offset que se entrega es el de la **siguiente** entrada: es por donde se
        // reanudaria si el buffer se llena a mitad.
        for (i, (n, tipo, nombre)) in todas.into_iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(n), i as u64 + 1, tipo, &nombre) {
                break;
            }
        }
        reply.ok();
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let Some(ruta) = self.ruta_de(ino) else {
            return reply.error(Errno::ENOENT);
        };
        if self.de_solo_lectura() && flags.acc_mode() != fuser::OpenAccMode::O_RDONLY {
            return reply.error(Errno::EROFS);
        }
        reply.opened(self.abrir_descriptor(ruta), fuser::FopenFlags::empty());
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let Some(ruta) = self.ruta_de(ino) else {
            return reply.error(Errno::ENOENT);
        };
        // Lo escrito y aun sin materializar no esta en el volumen: leer sin bajarlo antes
        // devolveria el contenido viejo.
        if let Some(a) = self.abierto(fh)
            && let Err(e) = self.materializar(&a)
        {
            return reply.error(e);
        }

        let Ok(v) = self.volumen.read() else {
            return reply.error(Errno::EIO);
        };
        match self.esperar(v.read_at(&ruta, offset, size as usize)) {
            Ok(datos) => reply.data(&datos),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        if self.de_solo_lectura() {
            return reply.error(Errno::EROFS);
        }
        let Some(a) = self.abierto(fh) else {
            return reply.error(Errno::EBADF);
        };
        a.escritura
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .escribir(offset, data);
        reply.written(data.len() as u32);
    }

    /// `flush` se llama en cada `close()`, y puede haber varios por fichero.
    ///
    /// Es donde el nucleo espera que los datos esten a salvo dentro del sistema de ficheros
    /// —que aqui significa «en el volumen»—, no necesariamente subidos a los proveedores: de
    /// eso se encarga el sincronizador, igual que en Windows.
    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: fuser::LockOwner,
        reply: ReplyEmpty,
    ) {
        match self.abierto(fh) {
            Some(a) => match self.materializar(&a) {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(e),
            },
            None => reply.ok(),
        }
    }

    fn fsync(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _d: bool, reply: ReplyEmpty) {
        match self.abierto(fh) {
            Some(a) => match self.materializar(&a) {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(e),
            },
            None => reply.ok(),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let a = self
            .abiertos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&fh.0);
        // Aunque `flush` ya deberia haberlo hecho: perder datos por confiar en el orden de
        // las llamadas seria un mal negocio.
        if let Some(a) = a
            && let Err(e) = self.materializar(&a)
        {
            return reply.error(e);
        }
        reply.ok();
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        if self.de_solo_lectura() {
            return reply.error(Errno::EROFS);
        }
        let (Some(padre), Some(nombre)) = (self.ruta_de(parent), name.to_str()) else {
            return reply.error(Errno::ENOENT);
        };
        let ruta = Self::unir(&padre, nombre);

        let Ok(mut v) = self.volumen.write() else {
            return reply.error(Errno::EIO);
        };
        if self.esperar(v.create_file(&ruta)).is_err() {
            return reply.error(Errno::EIO);
        }
        drop(v);

        let ino = self.ino_de(&ruta);
        reply.created(
            &TTL,
            &self.atributos(ino, false, 0),
            Generation(0),
            self.abrir_descriptor(ruta),
            fuser::FopenFlags::empty(),
        );
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        if self.de_solo_lectura() {
            return reply.error(Errno::EROFS);
        }
        let (Some(padre), Some(nombre)) = (self.ruta_de(parent), name.to_str()) else {
            return reply.error(Errno::ENOENT);
        };
        let ruta = Self::unir(&padre, nombre);
        if self.mirar(&ruta).is_some() {
            return reply.error(Errno::EEXIST);
        }

        let Ok(mut v) = self.volumen.write() else {
            return reply.error(Errno::EIO);
        };
        if self.esperar(v.mkdir_p(&ruta)).is_err() {
            return reply.error(Errno::EIO);
        }
        drop(v);

        let ino = self.ino_de(&ruta);
        reply.entry(&TTL, &self.atributos(ino, true, 0), Generation(0));
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.borrar(parent, name, false, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.borrar(parent, name, true, reply);
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: fuser::RenameFlags,
        reply: ReplyEmpty,
    ) {
        if self.de_solo_lectura() {
            return reply.error(Errno::EROFS);
        }
        let (Some(padre), Some(nombre), Some(padre2), Some(nombre2)) = (
            self.ruta_de(parent),
            name.to_str(),
            self.ruta_de(newparent),
            newname.to_str(),
        ) else {
            return reply.error(Errno::ENOENT);
        };
        let origen = Self::unir(&padre, nombre);
        let destino = Self::unir(&padre2, nombre2);

        // `RENAME_NOREPLACE` pide fallar si el destino existe; sin el, un destino ocupado se
        // reemplaza, que es lo que espera `mv`.
        let reemplazar = !flags.contains(fuser::RenameFlags::RENAME_NOREPLACE);
        if !reemplazar && self.mirar(&destino).is_some() {
            return reply.error(Errno::EEXIST);
        }
        // `RENAME_EXCHANGE` intercambia los dos, y el arbol no sabe hacerlo de una pieza.
        // Simularlo con tres renombrados no seria atomico, que es justo lo que la bandera
        // pide, asi que se dice que no en vez de hacerlo mal.
        if flags.contains(fuser::RenameFlags::RENAME_EXCHANGE) {
            return reply.error(Errno::ENOSYS);
        }

        let Ok(mut v) = self.volumen.write() else {
            return reply.error(Errno::EIO);
        };
        if self
            .esperar(v.rename(&origen, &destino, reemplazar))
            .is_err()
        {
            return reply.error(Errno::EIO);
        }
        drop(v);

        self.inodos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renombrar(&origen, &destino);
        reply.ok();
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        // Dos cargas atomicas, sin red: `statfs` se llama en cada `df` y en cada copia con
        // barra de progreso. Quien mide es la tarea de fondo de `crate::capacidad`.
        //
        // Lo que se declara es el tamano de **las cuentas** sobre las que vive el volumen, no
        // lo que ocupa el volumen: `statfs` tiene sitio para dos cifras y aqui hay tres. El
        // desglose esta en `multidrive espacio`.
        //
        // Sin medida todavia se declara lo mismo que declara el adaptador de Windows: un
        // numero deliberadamente absurdo. El terabyte fijo que habia aqui antes era peor
        // precisamente por ser creible: un tamano que se confunde con una medida es mas
        // enganoso que uno que se ve a la legua que no lo es.
        let (total, libre) = self.capacidad.leer().unwrap_or((DESCONOCIDO, DESCONOCIDO));
        let bloques = |bytes: u64| bytes / BLOQUE as u64;
        reply.statfs(
            bloques(total),
            bloques(libre),
            bloques(libre),
            0,
            0,
            BLOQUE,
            255,
            BLOQUE,
        );
    }
}

impl MultiDriveFuse {
    /// El tronco comun de `unlink` y `rmdir`.
    ///
    /// Se comprueba el tipo aunque el arbol tambien lo haria: `rmdir` sobre un fichero tiene
    /// que dar `ENOTDIR` y `unlink` sobre un directorio `EISDIR`, y esos codigos concretos son
    /// los que distinguen a un `rm -r` que funciona de uno que se rinde.
    fn borrar(&self, parent: INodeNo, name: &OsStr, quiero_dir: bool, reply: ReplyEmpty) {
        if self.de_solo_lectura() {
            return reply.error(Errno::EROFS);
        }
        let (Some(padre), Some(nombre)) = (self.ruta_de(parent), name.to_str()) else {
            return reply.error(Errno::ENOENT);
        };
        let ruta = Self::unir(&padre, nombre);

        let Some((es_dir, _)) = self.mirar(&ruta) else {
            return reply.error(Errno::ENOENT);
        };
        match (quiero_dir, es_dir) {
            (true, false) => return reply.error(Errno::ENOTDIR),
            (false, true) => return reply.error(Errno::EISDIR),
            _ => {}
        }

        let Ok(mut v) = self.volumen.write() else {
            return reply.error(Errno::EIO);
        };
        match self.esperar(v.remove(&ruta)) {
            Ok(_) => {
                drop(v);
                self.inodos
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .olvidar(&ruta);
                reply.ok();
            }
            // El arbol se niega a borrar un directorio que no esta vacio, y ese es el codigo
            // por el que `rm -r` sabe que tiene que bajar primero.
            Err(_) if es_dir => reply.error(Errno::ENOTEMPTY),
            Err(_) => reply.error(Errno::EIO),
        }
    }
}

/// Un montaje vivo. Al soltarlo se desmonta.
pub struct Montaje {
    _sesion: BackgroundSession,
}

/// Monta el volumen en un directorio.
pub fn montar(volumen: Arc<RwLock<Volume>>, punto: &str, solo_lectura: bool) -> Result<Montaje> {
    let destino = std::path::Path::new(punto);
    if !destino.is_dir() {
        anyhow::bail!(
            "el punto de montaje '{punto}' no existe o no es un directorio.\n\
             En Linux se monta sobre un directorio vacio, no sobre una letra de unidad."
        );
    }

    let mut opciones = vec![
        MountOption::FSName(NOMBRE_FS.to_string()),
        MountOption::Subtype(NOMBRE_FS.to_string()),
        // Sin `suid` ni dispositivos: lo que hay dentro viene de la nube, y ejecutar con
        // privilegios algo que ha viajado por ahi no es una funcionalidad que nadie pida.
        MountOption::NoSuid,
        MountOption::NoDev,
        MountOption::NoAtime,
    ];
    opciones.push(if solo_lectura {
        MountOption::RO
    } else {
        MountOption::RW
    });

    // El tamano se mide de fondo. Aqui no se puede preguntar: `statfs` se llama en cada `df`
    // y en cada barra de progreso de un gestor de ficheros.
    let store = volumen
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .store()
        .clone();

    let fs = MultiDriveFuse {
        volumen,
        capacidad: Capacidad::arrancar(store),
        rt: Handle::current(),
        solo_lectura,
        inodos: Mutex::new(Inodos::nuevo()),
        abiertos: Mutex::new(HashMap::new()),
        siguiente_fh: AtomicU64::new(1),
        creado: SystemTime::now(),
        // El volumen no guarda dueno, asi que todo pertenece a quien lo monta. Cualquier otra
        // cosa serian permisos inventados.
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
    };

    let mut config = fuser::Config::default();
    config.mount_options = opciones;

    let sesion = fuser::spawn_mount(fs, destino, &config).with_context(|| {
        format!(
            "no se pudo montar en '{punto}'.\n\
             Hace falta que exista /dev/fuse y que el usuario pueda abrirlo. En un contenedor \
             LXC eso significa 'features: fuse=1' en su configuracion."
        )
    })?;

    Ok(Montaje { _sesion: sesion })
}
