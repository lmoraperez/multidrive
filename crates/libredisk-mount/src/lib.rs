//! Montaje del volumen como una unidad del sistema.
//!
//! Es la capa que traduce lo que pide el sistema operativo a lo que sabe hacer
//! [`libredisk_core::tree::Volume`]. Las operaciones ya existen todas —`lookup`, `read_dir`,
//! `read_at`, `manifiesto`—; aqui solo se cambian de idioma.
//!
//! Por ahora **solo Windows**, con WinFsp. Se puede montar en lectura o en lectura-escritura;
//! escribir acumula en un buffer por descriptor y materializa al cerrar, porque los trozos son
//! inmutables y escribir dentro significa reescribir el trozo entero.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{Montaje, montar};

#[cfg(not(windows))]
pub fn montar(
    _volumen: std::sync::Arc<std::sync::RwLock<libredisk_core::tree::Volume>>,
    _punto: &str,
    _solo_lectura: bool,
) -> anyhow::Result<std::convert::Infallible> {
    anyhow::bail!(
        "el montaje solo esta implementado en Windows (WinFsp).\n\
         En Linux hara falta FUSE, que es el siguiente objetivo."
    )
}
