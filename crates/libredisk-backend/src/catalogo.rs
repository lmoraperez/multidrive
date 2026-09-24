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
    /// Lo consigue el flujo del navegador, no el usuario tecleando.
    ///
    /// Sigue siendo obligatorio: para cuando se guarda tiene que estar. Lo que cambia es
    /// **quien lo pone**, y por tanto que la interfaz ensene un boton en vez de una caja de
    /// texto. Nadie deberia teclear un token de 200 caracteres.
    pub navegador: bool,
    /// Identifica a **la aplicacion** y no al usuario.
    ///
    /// Son los mismos para todo el mundo y salen de registrar LibreDisk una vez, asi que
    /// pueden venir dentro del programa. Cuando vienen, la interfaz no los pide: pedirle a
    /// alguien el ID de una aplicacion que no es suya es pedirle un dato que no tiene.
    pub aplicacion: bool,
    /// Tiene un valor por defecto que sirve, asi que las interfaces no lo preguntan.
    ///
    /// Se sigue pudiendo poner en el fichero de configuracion. Lo que se evita es pedirle
    /// una decision a quien no tiene motivo para tomarla: la carpeta dentro de una cuenta de
    /// nube es siempre la misma y da igual como se llame, porque dentro solo hay blobs
    /// opacos que nadie va a abrir desde la web del proveedor.
    pub avanzado: bool,
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
            aplicacion: false,
            avanzado: false,
            navegador: false,
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
    /// No se teclea: lo trae el flujo del navegador.
    const fn del_navegador(mut self) -> Self {
        self.navegador = true;
        self
    }
    /// Identifica a LibreDisk, no al usuario: puede venir dentro del programa.
    const fn de_la_aplicacion(mut self) -> Self {
        self.aplicacion = true;
        self
    }
    /// Tiene un valor por defecto que sirve, asi que no se pregunta.
    const fn con_defecto(mut self) -> Self {
        self.avanzado = true;
        self.obligatorio = false;
        self
    }
    const fn en_grupo(mut self, grupo: &'static str) -> Self {
        self.grupo = Some(grupo);
        self.obligatorio = false;
        self
    }
}

/// Lo que hace falta para hablar OAuth2 con un proveedor.
///
/// Vive en el catalogo y no repartido por el codigo porque lo usan tres sitios: el flujo del
/// navegador que consigue el token, la consulta de cupo que lo gasta, y `live` al construir
/// el proveedor. Tres copias de una URL son tres copias que acaban discrepando.
#[derive(Debug, Clone, Copy)]
pub struct Oauth {
    /// Donde se manda el navegador para que el usuario de permiso.
    pub autorizar: &'static str,
    /// Donde se canjea el codigo por tokens, y donde se renuevan despues.
    ///
    /// Es **el mismo** que usa OpenDAL para renovar. Si aqui valiera otro, un token que
    /// sirve para escribir podria no servir para renovarse, y el fallo pareceria de permisos.
    pub token: &'static str,
    /// Que se pide poder hacer.
    pub ambito: &'static str,
    /// Donde preguntar cuanto espacio queda.
    pub cuota: &'static str,
    /// Donde preguntar **de quien es la cuenta**, y por que camino del JSON viene.
    ///
    /// Se usa para una sola cosa, y es importante: detectar que dos proveedores son la misma
    /// cuenta. Un `mirror:2` sobre dos veces la misma cuenta dice que aguanta una caida y no
    /// aguanta ninguna, porque el dia que se pierda esa cuenta se pierden las dos copias.
    ///
    /// Se pide un identificador opaco y no el correo. Sirve igual para comparar, y asi no hay
    /// que guardar ni imprimir la direccion de nadie.
    pub identidad: &'static str,
    pub campo_identidad: &'static [&'static str],
    /// Parametros que este proveedor exige en la URL de autorizacion y los demas no.
    pub extra: &'static [(&'static str, &'static str)],
}

/// Google pide tres cosas, y cada una arregla un fallo distinto:
///
/// - `access_type=offline`, que es lo que significa "quiero poder renovar". Sin el solo da un
///   token de acceso que caduca en una hora.
/// - `prompt=select_account`, que es lo que deja **elegir de que cuenta se trata**. Sin el,
///   Google usa la sesion que ya haya abierta, y anadir una segunda cuenta acabaria
///   conectando otra vez la primera.
/// - `prompt=consent`, porque si el usuario ya habia dado permiso antes, Google **no vuelve a
///   mandar el token de refresco**.
///
/// Los dos `prompt` van juntos en el mismo parametro separados por un espacio, que es como lo
/// define el estandar.
const GDRIVE_OAUTH: Oauth = Oauth {
    autorizar: "https://accounts.google.com/o/oauth2/v2/auth",
    token: "https://oauth2.googleapis.com/token",
    // Solo los ficheros que crea LibreDisk, que es todo lo que necesita: crea su propia
    // carpeta y todo lo que hay dentro. Pedir el Drive entero seria pedir permiso sobre las
    // fotos del usuario para guardar en una carpeta, y ademas Google lo clasifica como
    // ambito restringido, que exige una evaluacion de seguridad anual de terceros.
    ambito: "https://www.googleapis.com/auth/drive.file",
    cuota: "https://www.googleapis.com/drive/v3/about?fields=storageQuota",
    // `permissionId` es el identificador estable del usuario en Drive. Se pide en vez del
    // correo porque para comparar sirve igual y no hay que guardar la direccion de nadie.
    identidad: "https://www.googleapis.com/drive/v3/about?fields=user(permissionId)",
    campo_identidad: &["user", "permissionId"],
    extra: &[
        ("access_type", "offline"),
        ("prompt", "select_account consent"),
    ],
};

/// `offline_access` es lo que pide el token de refresco, y `Files.ReadWrite` es **el ambito
/// exacto que OpenDAL usa al renovar**: pedir aqui uno distinto daria un token que deja de
/// valer en la primera renovacion.
///
/// `prompt=select_account` por lo mismo que en Google: sin el, anadir una segunda cuenta
/// reconectaria la que ya estuviera abierta en el navegador.
const ONEDRIVE_OAUTH: Oauth = Oauth {
    autorizar: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    token: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
    ambito: "offline_access Files.ReadWrite",
    cuota: "https://graph.microsoft.com/v1.0/me/drive?$select=quota",
    // El identificador de la unidad, que es distinto para cada cuenta y no dice nada de quien
    // es. Se pide con el ambito que ya tenemos, sin anadir permisos sobre el perfil.
    identidad: "https://graph.microsoft.com/v1.0/me/drive?$select=id",
    campo_identidad: &["id"],
    extra: &[("response_mode", "query"), ("prompt", "select_account")],
};

/// Una clase de proveedor.
#[derive(Debug, Clone, Copy)]
pub struct Tipo {
    /// Lo que va en la variable: `FS`, `ONEDRIVE`, `GDRIVE`, `S3`, `WEBDAV`.
    pub clave: &'static str,
    pub etiqueta: &'static str,
    /// Que es, y sobre todo que sacrificio trae. Se ensena antes de elegirlo.
    pub explicacion: &'static str,
    pub campos: &'static [Campo],
    /// Si se conecta por navegador, con que datos.
    pub oauth: Option<&'static Oauth>,
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
        "Cuenta conectada",
        "Lo consigue el boton de conectar: se abre el navegador, entras en tu cuenta y das permiso. No se teclea nada.",
    )
    .secreto()
    .del_navegador(),
    Campo::nuevo(
        "CLIENT_ID",
        "ID de la aplicacion",
        "De la aplicacion registrada en la consola del proveedor, no de tu cuenta. Solo hace falta si esta copia de LibreDisk no trae la suya dentro.",
    )
    .de_la_aplicacion(),
    Campo::nuevo("CLIENT_SECRET", "Secreto de la aplicacion", "De la misma aplicacion.")
        .secreto()
        .de_la_aplicacion(),
    Campo::nuevo("ROOT", "Carpeta dentro de la cuenta", "")
        .con_ejemplo("/libredisk")
        .con_defecto(),
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
        .con_defecto(),
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
        .con_defecto(),
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
        oauth: None,
    },
    Tipo {
        clave: "ONEDRIVE",
        etiqueta: "OneDrive",
        explicacion: "Personal o de empresa. Se conecta desde el navegador: pulsas conectar, \
                      entras en tu cuenta y das permiso.",
        campos: CAMPOS_OAUTH,
        oauth: Some(&ONEDRIVE_OAUTH),
    },
    Tipo {
        clave: "GDRIVE",
        etiqueta: "Google Drive",
        explicacion: "Igual que OneDrive: se conecta desde el navegador. LibreDisk solo pide \
                      permiso sobre los ficheros que el mismo cree, no sobre el resto de tu \
                      Drive.",
        campos: CAMPOS_OAUTH,
        oauth: Some(&GDRIVE_OAUTH),
    },
    Tipo {
        clave: "S3",
        etiqueta: "S3 (Backblaze B2, Cloudflare R2, Wasabi, MinIO)",
        explicacion: "Cualquier servicio compatible con S3. No declara cupo: se factura por \
                      lo que ocupa, asi que no se llena.",
        campos: CAMPOS_S3,
        oauth: None,
    },
    Tipo {
        clave: "WEBDAV",
        etiqueta: "WebDAV (Nextcloud)",
        explicacion: "Un Nextcloud propio o cualquier servidor WebDAV.",
        campos: CAMPOS_WEBDAV,
        oauth: None,
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
    validar_con(
        tipo,
        valor,
        crate::aplicacion::configurada(tipo.clave).is_some(),
    )
}

/// Igual, diciendo por fuera si el programa trae aplicacion registrada.
///
/// Existe para poder probar las dos situaciones sin depender de con que credenciales se haya
/// compilado el binario, que es justo lo que no se puede controlar desde un test.
pub fn validar_con(
    tipo: &Tipo,
    valor: &dyn Fn(&str) -> Option<String>,
    hay_aplicacion: bool,
) -> Result<(), Vec<String>> {
    let puesto = |c: &Campo| valor(c.clave).is_some_and(|v| !v.trim().is_empty());
    let mut fallos = Vec::new();

    for campo in tipo.campos {
        // Lo que identifica a la aplicacion no se le exige al usuario si el programa ya lo
        // trae dentro: pedirle el ID de una aplicacion que no es suya es pedirle un dato que
        // no tiene.
        if campo.aplicacion && hay_aplicacion {
            continue;
        }
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

    /// Lo que de verdad cambia para quien prueba LibreDisk por primera vez.
    ///
    /// Con la aplicacion dentro del programa, conectar una cuenta de Drive no pide **nada**
    /// mas que pulsar el boton: el token lo trae el navegador y el ID y el secreto los trae
    /// el binario.
    #[test]
    fn con_aplicacion_propia_no_se_pide_nada_del_desarrollador() {
        let m = con(&[("REFRESH_TOKEN", "lo-que-trajo-el-navegador")]);
        let t = tipo("GDRIVE").unwrap();
        assert!(
            validar_con(t, &|c| m.get(c).cloned(), true).is_ok(),
            "con aplicacion propia, el token del navegador tiene que bastar"
        );
    }

    /// Y sin ella, se piden: es lo que pasa hoy, y el formulario lo dice.
    #[test]
    fn sin_aplicacion_propia_se_siguen_pidiendo() {
        let m = con(&[("REFRESH_TOKEN", "lo-que-trajo-el-navegador")]);
        let t = tipo("GDRIVE").unwrap();
        let fallos = validar_con(t, &|c| m.get(c).cloned(), false).unwrap_err();
        assert_eq!(fallos.len(), 2, "el ID y el secreto: {fallos:?}");
    }

    /// La cuenta sigue haciendo falta aunque el programa traiga aplicacion.
    ///
    /// Es la mitad que no se puede incorporar: la aplicacion es la misma para todos, la
    /// cuenta es de cada uno.
    #[test]
    fn la_aplicacion_propia_no_sustituye_a_conectar_la_cuenta() {
        let m = con(&[]);
        let t = tipo("GDRIVE").unwrap();
        let fallos = validar_con(t, &|c| m.get(c).cloned(), true).unwrap_err();
        assert_eq!(fallos, ["falta «Cuenta conectada»"]);
    }

    /// La carpeta dentro de la cuenta **no se pregunta**: es siempre la misma y da igual
    /// como se llame, porque dentro solo hay blobs opacos que nadie va a abrir desde la web
    /// del proveedor. Pedir una decision a quien no tiene motivo para tomarla es ruido.
    ///
    /// La excepcion es la carpeta local, donde la carpeta **es** el proveedor: ahi no hay
    /// defecto que valga y hay que decir cual.
    #[test]
    fn la_carpeta_solo_se_pregunta_donde_no_hay_defecto() {
        for t in TIPOS {
            for c in t.campos.iter().filter(|c| c.clave == "ROOT") {
                let se_pregunta = !c.avanzado;
                assert_eq!(
                    se_pregunta,
                    t.clave == "FS",
                    "{}_{} se pregunta: {se_pregunta}",
                    t.clave,
                    c.clave
                );
            }
        }
    }

    /// Un campo que no se pregunta no puede ser obligatorio: no habria forma de rellenarlo
    /// desde la ventana y el alta seria imposible.
    #[test]
    fn nada_obligatorio_se_esconde() {
        for t in TIPOS {
            for c in t.campos {
                assert!(
                    !(c.avanzado && c.obligatorio && !c.navegador),
                    "{}_{} es obligatorio y no se pregunta",
                    t.clave,
                    c.clave
                );
            }
        }
    }

    /// Solo son de la aplicacion los dos que identifican a LibreDisk.
    ///
    /// Marcar de mas seria dejar de pedir algo que el programa no puede aportar, y el
    /// proveedor lo rechazaria con un error de credenciales que no dice que falta.
    #[test]
    fn solo_el_id_y_el_secreto_son_de_la_aplicacion() {
        for t in TIPOS {
            for c in t.campos {
                let deberia = c.clave == "CLIENT_ID" || c.clave == "CLIENT_SECRET";
                assert_eq!(deberia, c.aplicacion, "{}_{}", t.clave, c.clave);
            }
        }
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
