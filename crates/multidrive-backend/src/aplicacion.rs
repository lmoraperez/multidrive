//! Las credenciales de **la aplicacion**, que no son las del usuario.
//!
//! Hay dos cosas distintas que se llaman "credenciales" y confundirlas es lo que hacia que el
//! formulario pidiera cosas de desarrollador:
//!
//! - Las **de la aplicacion** (`client_id` y `client_secret`) identifican a MultiDrive ante
//!   Google o Microsoft. Son las mismas para todo el mundo y salen de registrar la aplicacion
//!   una vez. **Aqui.**
//! - Las **del usuario** son su cuenta, y nunca pasan por este programa: van del navegador al
//!   proveedor, y de vuelta solo llega un token.
//!
//! # El secreto no es secreto
//!
//! Va dentro de un binario que se distribuye, asi que cualquiera puede sacarlo. No es un
//! descuido: Google lo dice con todas las letras para las aplicaciones instaladas, y es el
//! motivo de que exista PKCE. Lo que protege el flujo no es el secreto, es que el codigo de
//! autorizacion no sirve sin un valor que solo conoce el proceso que lo pidio.
//!
//! Lo que si trae de verdad compartir una aplicacion: **la cuota es compartida**. Si alguien
//! abusa de ella, el proveedor puede limitar o suspender el proyecto entero, y eso afecta a
//! todos los que usen esa version. Por eso se puede sustituir sin recompilar.
//!
//! # De donde salen, por orden
//!
//! 1. Lo que tenga configurado esa ranura concreta (`MULTIDRIVE_GDRIVE2_CLIENT_ID`). Quien
//!    escribe eso esta diciendo "para esta cuenta, usa mi aplicacion".
//! 2. Una sustitucion de esta maquina (`MULTIDRIVE_APP_GDRIVE_CLIENT_ID`), que vale para todas
//!    las cuentas de ese servicio. Es la salida para quien choque con la cuota compartida.
//! 3. Las que se compilaron dentro.
//!
//! Sin ninguna de las tres, el formulario vuelve a pedirlas, que es donde estabamos.

/// Lo que identifica a una aplicacion ante el proveedor.
#[derive(Clone, PartialEq, Eq)]
pub struct Credenciales {
    pub client_id: String,
    pub client_secret: String,
}

/// El secreto va redactado aunque no sea secreto de verdad: que esto acabe en un registro no
/// aporta nada y confunde a quien lo lea sobre que se puede pegar por ahi.
impl std::fmt::Debug for Credenciales {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credenciales")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redactado>")
            .finish()
    }
}

/// Las que se compilaron dentro del binario, si las hay.
///
/// Se leen del entorno **al compilar**, no al ejecutar, con `option_env!`. Asi el repositorio
/// no tiene que llevarlas escritas: quien construye una version para distribuir las pone en
/// el entorno de esa construccion.
///
/// Un `build.rs` se encarga de que cambiarlas obligue a recompilar. Sin eso, cambiar la
/// variable y reconstruir dejaria el binario con las de antes y nadie se enteraria.
fn compiladas(tipo: &str) -> Option<Credenciales> {
    let (id, secreto) = match tipo {
        "GDRIVE" => (
            option_env!("MULTIDRIVE_APP_GDRIVE_CLIENT_ID"),
            option_env!("MULTIDRIVE_APP_GDRIVE_CLIENT_SECRET"),
        ),
        "ONEDRIVE" => (
            option_env!("MULTIDRIVE_APP_ONEDRIVE_CLIENT_ID"),
            option_env!("MULTIDRIVE_APP_ONEDRIVE_CLIENT_SECRET"),
        ),
        _ => (None, None),
    };
    juntar(id.map(str::to_string), secreto.map(str::to_string))
}

/// Las de este tipo de proveedor, mirando primero lo que sustituya esta maquina.
///
/// `leer` es de donde salen las variables: el entorno y el fichero de configuracion, igual
/// que todo lo demas.
pub fn de(tipo: &str, leer: &dyn Fn(&str) -> Option<String>) -> Option<Credenciales> {
    let propia = juntar(
        leer(&format!("MULTIDRIVE_APP_{tipo}_CLIENT_ID")),
        leer(&format!("MULTIDRIVE_APP_{tipo}_CLIENT_SECRET")),
    );
    propia.or_else(|| compiladas(tipo))
}

/// Igual que [`de`], mirando donde mira todo lo demas.
pub fn configurada(tipo: &str) -> Option<Credenciales> {
    let config = crate::config::Config::cargar().ok();
    de(tipo, &|clave| {
        std::env::var(clave)
            .ok()
            .or_else(|| config.as_ref().and_then(|c| c.get(clave)))
    })
}

/// Las dos o ninguna, y ninguna de las dos en blanco.
///
/// Media pareja seria peor que nada: el proveedor rechazaria el canje con un error de
/// credenciales, y quien lo viera buscaria el fallo en su cuenta en vez de en la
/// configuracion.
fn juntar(id: Option<String>, secreto: Option<String>) -> Option<Credenciales> {
    let limpio = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match (limpio(id), limpio(secreto)) {
        (Some(client_id), Some(client_secret)) => Some(Credenciales {
            client_id,
            client_secret,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn entorno(pares: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let m: HashMap<String, String> = pares
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |c: &str| m.get(c).cloned()
    }

    #[test]
    fn una_sustitucion_de_la_maquina_gana_a_lo_compilado() {
        let leer = entorno(&[
            ("MULTIDRIVE_APP_GDRIVE_CLIENT_ID", "el-mio"),
            ("MULTIDRIVE_APP_GDRIVE_CLIENT_SECRET", "y-su-secreto"),
        ]);
        let c = de("GDRIVE", &leer).unwrap();
        assert_eq!(c.client_id, "el-mio");
    }

    #[test]
    fn media_pareja_no_vale() {
        // Con solo una, el proveedor rechazaria el canje con un error de credenciales y quien
        // lo viera buscaria el fallo en su cuenta en vez de en la configuracion.
        let leer = entorno(&[("MULTIDRIVE_APP_GDRIVE_CLIENT_ID", "suelto")]);
        assert!(de("GDRIVE", &leer).is_none());
    }

    #[test]
    fn un_valor_en_blanco_cuenta_como_ausente() {
        // Un fichero de configuracion con la clave puesta y sin valor es lo que deja alguien
        // a medio rellenar. No es una credencial.
        let leer = entorno(&[
            ("MULTIDRIVE_APP_GDRIVE_CLIENT_ID", "   "),
            ("MULTIDRIVE_APP_GDRIVE_CLIENT_SECRET", "algo"),
        ]);
        assert!(de("GDRIVE", &leer).is_none());
    }

    #[test]
    fn los_tipos_que_no_son_oauth_no_tienen_aplicacion() {
        let leer = entorno(&[]);
        for tipo in ["FS", "S3", "WEBDAV"] {
            assert!(de(tipo, &leer).is_none(), "{tipo}");
        }
    }

    #[test]
    fn el_secreto_no_sale_al_depurar() {
        // No es secreto de verdad --va dentro del binario-- pero que aparezca en un registro
        // no aporta nada y confunde sobre que se puede pegar por ahi.
        let c = Credenciales {
            client_id: "publico".into(),
            client_secret: "no-mirar".into(),
        };
        let texto = format!("{c:?}");
        assert!(texto.contains("publico"));
        assert!(!texto.contains("no-mirar"), "{texto}");
    }
}
