fn main() {
    tauri_build::build();

    // Sin esto, el binario entero exige `winfsp-x64.dll` **al arrancar**, y como su carpeta
    // no esta en el PATH la ventana no llega ni a abrirse: el proceso muere sin decir nada.
    // Con carga diferida, la DLL solo hace falta al montar, que es cuando el adaptador ya la
    // ha localizado por el registro.
    //
    // Los argumentos de enlazado del script de una dependencia no se propagan al binario,
    // asi que hay que repetirlo aqui aunque `multidrive-mount` ya lo haga por su cuenta.
    #[cfg(windows)]
    winfsp::build::winfsp_link_delayload();
}
