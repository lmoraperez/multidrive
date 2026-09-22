//! Montaje del volumen como una unidad del sistema.
//!
//! Es la capa que traduce lo que pide el sistema operativo a lo que sabe hacer
//! [`libredisk_core::tree::Volume`]. Las operaciones ya existen todas —`lookup`, `read_dir`,
//! `read_at`, `manifiesto`—; aqui solo se cambian de idioma.
//!
//! Hay dos adaptadores y la misma interfaz para los dos: WinFsp en Windows y FUSE en el resto.
//! Comparten las reglas que importan —bloquear en los hilos del sistema es correcto porque no
//! son hilos del runtime, y escribir se acumula por descriptor y se materializa al cerrar,
//! porque los trozos son inmutables— y se diferencian en lo que impone cada sistema: WinFsp
//! habla de rutas y no distingue mayusculas, FUSE habla de inodos y si distingue.
//!
//! Lo que **no** se diferencia es el nucleo: no hay ni un `cfg` de sistema operativo por
//! debajo de esta capa.

mod capacidad;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{Montaje, montar};

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::{Montaje, montar};

#[cfg(not(any(windows, unix)))]
pub fn montar(
    _volumen: std::sync::Arc<std::sync::RwLock<libredisk_core::tree::Volume>>,
    _punto: &str,
    _solo_lectura: bool,
) -> anyhow::Result<std::convert::Infallible> {
    anyhow::bail!("no hay adaptador de montaje para este sistema")
}
