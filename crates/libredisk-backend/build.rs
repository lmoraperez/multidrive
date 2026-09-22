//! Que cambiar las credenciales de la aplicacion obligue a recompilar.
//!
//! `option_env!` se resuelve al compilar. Sin esto, cambiar la variable y volver a construir
//! dejaria el binario con las de antes y nadie se enteraria hasta que un usuario no pudiera
//! conectar su cuenta.
fn main() {
    for tipo in ["GDRIVE", "ONEDRIVE"] {
        for campo in ["CLIENT_ID", "CLIENT_SECRET"] {
            println!("cargo:rerun-if-env-changed=LIBREDISK_APP_{tipo}_{campo}");
        }
    }
}
