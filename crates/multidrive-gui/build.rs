fn main() {
    // El icono de la ventana y del ejecutable se compila como recurso de Windows **aqui**, y
    // Cargo no vuelve a ejecutar este script si no se le dice que depende de el. Sin estas
    // dos lineas se cambia el icono, se reconstruye, y el `.exe` sigue llevando el de antes
    // sin que nadie se entere: paso al cambiar de marca, y solo se vio abriendo el binario a
    // buscar los bytes. Un `cargo clean` lo arreglaba, que es justo la clase de arreglo que
    // nadie descubre a tiempo.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=tauri.conf.json");

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
