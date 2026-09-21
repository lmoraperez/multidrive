//! Montaje del volumen como una unidad del sistema.
//!
//! Es la capa que traduce lo que pide el sistema operativo a lo que sabe hacer
//! [`libredisk_core::tree::Volume`]. Las operaciones ya existen todas —`lookup`, `read_dir`,
//! `read_at`, `manifiesto`—; aqui solo se cambian de idioma.
//!
//! Por ahora **solo lectura y solo Windows**, con WinFsp. Escribir a traves del montaje exige
//! antes una cache con escritura diferida: sin ella, cada `write` del sistema seria una subida
//! sincrona a varias nubes, y ningun programa esta preparado para que escribir 4 KiB tarde
//! segundos.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{Montaje, montar};

#[cfg(not(windows))]
pub fn montar(
    _volumen: libredisk_core::tree::Volume,
    _punto: &str,
) -> anyhow::Result<std::convert::Infallible> {
    anyhow::bail!(
        "el montaje solo esta implementado en Windows (WinFsp).\n\
         En Linux hara falta FUSE, que es el siguiente objetivo."
    )
}
