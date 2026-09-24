fn main() {
    // La carga diferida hay que pedirla **aqui**, no solo en multidrive-mount: los
    // `rustc-link-arg` de un build script valen para el crate que se compila, no se propagan
    // al binario que lo usa. Sin esto, `multidrive ls` exige tener WinFsp instalado aunque no
    // vaya a montar nada.
    #[cfg(windows)]
    winfsp::build::winfsp_link_delayload();
}
