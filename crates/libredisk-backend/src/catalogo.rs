//! Que clases de proveedor hay y que hace falta para configurar cada una.
//!
//! Esto existe para que **solo haya un sitio** donde se sepa eso. Antes estaba repartido en
//! tres: el texto de ayuda de la linea de ordenes, el constructor de [`crate::live`] y lo que
//! cada interfaz decidiera pedirle al usuario. Tres copias de la misma lista son tres copias
//! que acaban discrepando, y la que discrepa es la que el usuario lee.
//!
//! De aqui salen los formularios de la ventana, la ayuda de `libredisk config ver` y la
//! comprobacion de que no falta nada antes de guardar.
//!
//! # Lo que no esta aqui
//!
//! **Como se consiguen las credenciales.** Sacar un token de refresco de Google o de
//! Microsoft es un baile por el navegador que no cabe en una etiqueta de formulario; esta en
//! `docs/PRUEBAS-PROVEEDORES.md` y las interfaces enlazan alli.
//!
//! **El token de acceso suelto.** [`crate::live`] lo sigue aceptando para no romper a quien
//! ya lo tenga escrito, pero no se ofrece al configurar: caduca en una hora, asi que un
//! volumen configurado con el deja de abrirse esa misma tarde. Ofrecerlo en un formulario
//! seria ofrecer una trampa.

/// Un campo que hay que rellenar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Campo {
    /// Sufijo de la variable, detras del proveedor: `ROOT` es `LIBREDISK_FS_ROOT`.
    pub clave: &'static str,
    pub etiqueta: &'static str,
    pub ayuda: &'static str,
    /// Que escribir, para que se vea la forma esperada sin tener que adivinarla.
    pub ejemplo: &'static str,
    /// No se ensena al teclearlo ni se imprime en ninguna parte.
    pub secreto: bool,
    pub obligatorio: bool,
    /// Campos que solo valen acompanados: o estan todos los del grupo o ninguno.
    ///
    /// Medio grupo es un error y no un descuido que se pueda ignorar: un `S3_ACCESS_KEY_ID`
    /// sin su secreto haria que el proveedor cayera en las credenciales del entorno de AWS,
    /// que es lo ultimo que espera quien acaba de teclear una clave.
    pub grupo: Option<&'static str>,
}

impl Campo {
    const fn nuevo(clave: &'static str, etiqueta: &'static str, ayuda: &'static str) -> Self {
        Self {
            clave,
            etiqueta,
            ayuda,
            ejemplo: "",
            secreto: false,
            obligatorio: true,
            grupo: None,
        }
    }
    const fn con_ejemplo(mut self, ejemplo: &'static str) -> Self {
        self.ejemplo = ejemplo;
        self
    }
    const fn secreto(mut self) -> Self {
        self.secreto = true;
        self
    }
    const fn opcional(mut self) -> Self {
        self.obligatorio = false;
        self
    }
    const fn en_grupo(mut self, grupo: &'static str) -> Self {
        self.grupo = Some(grupo);
        self.obligatorio = false;
        self
    }
}

/// Una clase de proveedor.
#[derive(Debug, Clone, Copy)]
pub struct Tipo {
    /// Lo que va en la variable: `FS`, `ONEDRIVE`, `GDRIVE`, `S3`, `WEBDAV`.
    pub clave: &'static str,
    pub etiqueta: &'static str,
    /// Que es, y sobre todo que sacrificio trae. Se ensena antes de elegirlo.
    pub explicacion: &'static str,
    pub campos: &'static [Campo],
}

const CAMPOS_FS: &[Campo] = &[Campo::nuevo(
    "ROOT",
    "Carpeta",
    "Donde guardar los fragmentos. Se crea si no existe.",
)
.con_ejemplo(if cfg!(windows) {
    "D:\\libredisk"
} else {
    "/mnt/disco/libredisk"
})];

/// Los dos servicios OAuth2 piden exactamente lo mismo, asi que comparten lista.
const CAMPOS_OAUTH: &[Campo] = &[
    Campo::nuevo(
        "REFRESH_TOKEN",
        "Token de refresco",
        "El que no caduca. Con el, el programa renueva el acceso solo.",
    )
    .secreto(),
    Campo::nuevo(
        "CLIENT_ID",
        "ID de la aplicacion",
        "De la aplicacion que registraste en la consola del proveedor.",
    ),
    Campo::nuevo("CLIENT_SECRET", "Secreto de la aplicacion", "De la misma.").secreto(),
    Campo::nuevo(
        "ROOT",
        "Carpeta dentro de la cuenta",
        "Dedicada: aqui solo van blobs opacos, y no deben mezclarse con tus archivos.",
    )
    .con_ejemplo("/libredisk")
    .opcional(),
];

const CAMPOS_S3: &[Campo] = &[
    Campo::nuevo("BUCKET", "Bucket", "El cubo donde van los fragmentos.").con_ejemplo("mi-bucket"),
    Campo::nuevo(
        "ENDPOINT",
        "Endpoint",
        "La direccion del servicio. Cada uno tiene la suya.",
    )
    .con_ejemplo("https://s3.eu-central-003.backblazeb2.com"),
    Campo::nuevo("REGION", "Region", "La que diga el proveedor.").con_ejemplo("eu-central-003"),
    Campo::nuevo(
        "ACCESS_KEY_ID",
        "Clave de acceso",
        "Si se deja en blanco se usan las credenciales del entorno de AWS.",
    )
    .en_grupo("credenciales"),
    Campo::nuevo(
        "SECRET_ACCESS_KEY",
        "Clave secreta",
        "La pareja de la anterior.",
    )
    .secreto()
    .en_grupo("credenciales"),
    Campo::nuevo("ROOT", "Prefijo dentro del bucket", "")
        .con_ejemplo("/libredisk")
        .opcional(),
];

const CAMPOS_WEBDAV: &[Campo] = &[
    Campo::nuevo("ENDPOINT", "Direccion", "La raiz WebDAV del servidor.")
        .con_ejemplo("https://nube.ejemplo.com/remote.php/dav/files/luis"),
    Campo::nuevo("USERNAME", "Usuario", "").en_grupo("credenciales"),
    Campo::nuevo(
        "PASSWORD",
        "Contrasena",
        "En Nextcloud, mejor una contrasena de aplicacion que la de la cuenta.",
    )
    .secreto()
    .en_grupo("credenciales"),
    Campo::nuevo("ROOT", "Carpeta", "")
        .con_ejemplo("/libredisk")
        .opcional(),
];

/// Todas las clases, en el orden en que tiene sentido ofrecerlas.
///
/// La carpeta local va primera porque es la unica que se puede probar sin pelearse con
/// OAuth, y porque es la que deja ver como funciona todo lo demas en un minuto. Va con su
/// aviso: **no es redundancia de verdad**.
pub const TIPOS: &[Tipo] = &[
    Tipo {
        clave: "FS",
        etiqueta: "Carpeta local",
        explicacion: "Un directorio de esta maquina o un disco conectado. Sirve para probar \
                      sin configurar ninguna nube, pero cuidado: muere con la maquina y \
                      consume presupuesto de redundancia como si fuera un proveedor de \
                      verdad.",
        campos: CAMPOS_FS,
    },
    Tipo {
        clave: "ONEDRIVE",
        etiqueta: "OneDrive",
        explicacion: "Personal o de empresa. Hace falta registrar una aplicacion y conseguir \
                      un token de refresco: el procedimiento esta en \
                      docs/PRUEBAS-PROVEEDORES.md.",
        campos: CAMPOS_OAUTH,
    },
    Tipo {
        clave: "GDRIVE",
        etiqueta: "Google Drive",
        explicacion: "Mismo procedimiento que OneDrive, en la consola de Google Cloud.",
        campos: CAMPOS_OAUTH,
    },
    Tipo {
        clave: "S3",
        etiqueta: "S3 (Backblaze B2, Cloudflare R2, Wasabi, MinIO)",
        explicacion: "Cualquier servicio compatible con S3. No declara cupo: se factura por \
                      lo que ocupa, asi que no se llena.",
        campos: CAMPOS_S3,
    },
    Tipo {
        clave: "WEBDAV",
        etiqueta: "WebDAV (Nextcloud)",
        explicacion: "Un Nextcloud propio o cualquier servidor WebDAV.",
        campos: CAMPOS_WEBDAV,
    },
];

/// El tipo con esa clave, si existe.
pub fn tipo(clave: &str) -> Option<&'static Tipo> {
    TIPOS.iter().find(|t| t.clave == clave)
}

/// Comprueba que estan los campos que hacen falta y que ningun grupo va a medias.
///
/// Devuelve **todos** los problemas, no el primero: rellenar un formulario de seis campos
/// descubriendo los fallos de uno en uno es una forma tonta de gastarle el tiempo a alguien.
pub fn validar(tipo: &Tipo, valor: &dyn Fn(&str) -> Option<String>) -> Result<(), Vec<String>> {
    let puesto = |c: &Campo| valor(c.clave).is_some_and(|v| !v.trim().is_empty());
    let mut fallos = Vec::new();

    for campo in tipo.campos {
        if campo.obligatorio && !puesto(campo) {
            fallos.push(format!("falta «{}»", campo.etiqueta));
        }
    }

    for grupo in grupos(tipo) {
        let del_grupo: Vec<&Campo> = tipo
            .campos
            .iter()
            .filter(|c| c.grupo == Some(grupo))
            .collect();
        let puestos = del_grupo.iter().filter(|c| puesto(c)).count();
        if puestos != 0 && puestos != del_grupo.len() {
            let faltan: Vec<&str> = del_grupo
                .iter()
                .filter(|c| !puesto(c))
                .map(|c| c.etiqueta)
                .collect();
            fallos.push(format!(
                "«{}» solo vale acompanado: falta {}",
                del_grupo
                    .iter()
                    .filter(|c| puesto(c))
                    .map(|c| c.etiqueta)
                    .collect::<Vec<_>>()
                    .join(", "),
                faltan.join(", ")
            ));
        }
    }

    if fallos.is_empty() {
        Ok(())
    } else {
        Err(fallos)
    }
}

fn grupos(tipo: &Tipo) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = tipo.campos.iter().filter_map(|c| c.grupo).collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn con(pares: &[(&str, &str)]) -> HashMap<String, String> {
        pares
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn valida(clave: &str, pares: &[(&str, &str)]) -> Result<(), Vec<String>> {
        let m = con(pares);
        validar(tipo(clave).unwrap(), &|c| m.get(c).cloned())
    }

    #[test]
    fn una_carpeta_solo_necesita_la_carpeta() {
        assert!(valida("FS", &[("ROOT", "D:\\libredisk")]).is_ok());
        assert!(valida("FS", &[]).is_err());
    }

    #[test]
    fn un_campo_en_blanco_no_cuenta_como_relleno() {
        // Un formulario devuelve la cadena vacia, no la ausencia de la clave. Tratarlos
        // distinto dejaria pasar un proveedor con la carpeta sin poner.
        assert!(valida("FS", &[("ROOT", "   ")]).is_err());
    }

    #[test]
    fn medio_grupo_de_credenciales_es_un_error() {
        // Sin las dos, el proveedor S3 cae en las credenciales del entorno de AWS, que es lo
        // ultimo que espera quien acaba de teclear una clave.
        let e = valida(
            "S3",
            &[
                ("BUCKET", "b"),
                ("ENDPOINT", "e"),
                ("REGION", "r"),
                ("ACCESS_KEY_ID", "algo"),
            ],
        )
        .unwrap_err();
        assert_eq!(e.len(), 1);
        assert!(e[0].contains("acompanado"), "{e:?}");
    }

    #[test]
    fn el_grupo_entero_o_ninguno_valen_los_dos() {
        let base = [("BUCKET", "b"), ("ENDPOINT", "e"), ("REGION", "r")];
        assert!(valida("S3", &base).is_ok(), "sin credenciales es valido");

        let mut con_creds = base.to_vec();
        con_creds.push(("ACCESS_KEY_ID", "a"));
        con_creds.push(("SECRET_ACCESS_KEY", "s"));
        assert!(valida("S3", &con_creds).is_ok());
    }

    #[test]
    fn se_devuelven_todos_los_fallos_y_no_solo_el_primero() {
        // Descubrir los fallos de uno en uno, recargando el formulario cada vez, es una forma
        // tonta de gastarle el tiempo a alguien.
        let e = valida("S3", &[]).unwrap_err();
        assert_eq!(e.len(), 3, "bucket, endpoint y region: {e:?}");
    }

    #[test]
    fn ningun_tipo_ofrece_el_token_de_acceso_suelto() {
        // Caduca en una hora: un volumen configurado asi deja de abrirse esa misma tarde.
        // `live` lo sigue aceptando por compatibilidad, pero ofrecerlo en un formulario seria
        // ofrecer una trampa.
        for t in TIPOS {
            assert!(
                !t.campos.iter().any(|c| c.clave == "ACCESS_TOKEN"),
                "{} ofrece ACCESS_TOKEN",
                t.clave
            );
        }
    }

    #[test]
    fn todo_lo_que_es_secreto_esta_marcado_como_tal() {
        // La marca es lo que hace que la ventana lo tape y que no se imprima en ningun sitio.
        // Un secreto sin marcar acaba en una captura de pantalla.
        for t in TIPOS {
            for c in t.campos {
                let parece = ["SECRET", "PASSWORD", "TOKEN"]
                    .iter()
                    .any(|p| c.clave.contains(p));
                assert_eq!(parece, c.secreto, "{}_{}", t.clave, c.clave);
            }
        }
    }
}
