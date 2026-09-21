fn main() {
    // WinFsp se carga con demora: sin esto, el enlazador no genera los thunks y el
    // programa muere al primer simbolo con un 1285 que no dice nada.
    #[cfg(windows)]
    winfsp::build::winfsp_link_delayload();
}
