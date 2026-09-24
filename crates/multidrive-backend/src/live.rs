//! Construccion de proveedores reales a partir de la configuracion.
//!
//! Las credenciales no se guardan en el repositorio. Salen del entorno del proceso o de un
//! fichero de configuracion (ver [`crate::config`]), y el entorno gana: quien exporta una
//! variable esta diciendo algo sobre **esta** ejecucion.
//!
//! El fichero existe porque un servicio no tiene consola: arranca como LocalSystem y ahi no
//! hay ningun `.env` que nadie haya exportado.
//!
//! Un proveedor "no configurado" se omite en silencio, pero uno **a medio configurar** es un
//! error ruidoso. Esa distincion importa: la forma clasica de tener pruebas que no prueban
//! nada es que un tipo tomado por el usuario se salte todo por una variable mal escrita.

use crate::catalogo;
use crate::config::Config;
use crate::{Backend, BackendError, OauthCreds, OpendalBackend};

const PREFIJO: &str = "MULTIDRIVE_";

/// El prefijo con el que nacio esto, cuando solo configuraba la suite de conformidad. Se
/// sigue aceptando: quien ya tenia un `.env` escrito no deberia tener que reescribirlo
/// porque el proyecto haya crecido. El nuevo gana si estan los dos.
const PREFIJO_VIEJO: &str = "MULTIDRIVE_TEST_";

/// Carpeta por defecto dentro de la cuenta.
///
/// Dedicada a proposito: dentro solo hay blobs opacos, y no deben compartir carpeta con los
/// archivos del usuario.
///
/// Se llamaba `/multidrive-pruebas` de cuando esto solo configuraba la suite de conformidad.
/// Ahora configura volumenes de verdad, y una carpeta llamada «pruebas» en la cuenta de
/// alguien invita a borrarla. **Quien tuviera un volumen en la carpeta vieja sin declararla
/// tiene que escribir `MULTIDRIVE_<PROVEEDOR>_ROOT=/multidrive-pruebas`**; si no, el programa
/// mirara en otro sitio y dira que no hay volumen.
const RAIZ_POR_DEFECTO: &str = "/multidrive";

/// Cuantas ranuras numeradas se buscan de cada tipo.
///
/// El limite existe para no preguntar indefinidamente; treinta y dos de cada tipo son mas
/// proveedores de los que tiene sentido repartir un volumen.
const MAX_RANURAS: usize = 32;

/// Las variables que reconoce, para poder decirselo al usuario cuando no hay ninguna.
///
/// Se construye del catalogo en vez de estar escrita a mano. Antes era una copia, y una copia
/// de una lista es una lista que acaba discrepando: cuando se numeraron todos los tipos, este
/// texto habria seguido diciendo que solo las carpetas locales se numeran.
pub fn ayuda() -> String {
    let mut s = format!(
        "Proveedores y claves reconocidas (prefijo {PREFIJO}; tambien se acepta el antiguo\n\
         {PREFIJO_VIEJO}). Se leen del entorno o del fichero de configuracion; el entorno gana.\n\
         \n\
         De cada tipo caben varios: a la clave se le pone un numero detras, de 1 a \
         {MAX_RANURAS}.\n\
         Asi, GDRIVE_* es la primera cuenta de Drive y GDRIVE2_* es otra distinta.\n"
    );
    for tipo in catalogo::TIPOS {
        s.push_str(&format!("\n  {}\n", tipo.etiqueta));
        for campo in tipo.campos {
            let marca = if campo.obligatorio {
                ""
            } else if campo.grupo.is_some() {
                "   (opcional, pero con su pareja)"
            } else {
                "   (opcional)"
            };
            s.push_str(&format!(
                "    {PREFIJO}{}_{}{marca}\n",
                tipo.clave, campo.clave
            ));
        }
    }
    s.push_str(
        "\nVer `multidrive config ruta` para saber donde se busca el fichero, y\n\
         docs/PRUEBAS-PROVEEDORES.md para obtener los tokens de OneDrive y Google Drive.\n",
    );
    s
}

/// Proveedores configurados, vengan de donde vengan.
///
/// El entorno gana al fichero. Quien exporta una variable esta diciendo algo sobre **esta**
/// ejecucion, y eso es mas especifico que lo que dejo escrito en disco hace un mes.
pub fn configurados() -> Result<Vec<OpendalBackend>, BackendError> {
    let config = Config::cargar()?;
    construir(&|clave| std::env::var(clave).ok().or_else(|| config.get(clave)))
}

/// Igual que [`configurados`], pero leyendo de donde se le diga. Existe para poder probar
/// la logica de configuracion sin tocar el entorno del proceso, que es global y compartido
/// entre los hilos de los tests.
fn construir(leer: &dyn Fn(&str) -> Option<String>) -> Result<Vec<OpendalBackend>, BackendError> {
    // Se prueban los dos prefijos por orden: el nuevo manda.
    let campo = |proveedor: &str, nombre: &str| -> Option<String> {
        [PREFIJO, PREFIJO_VIEJO].into_iter().find_map(|p| {
            leer(&format!("{p}{proveedor}_{nombre}"))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
    };
    let mut backends = Vec::new();

    // El orden de esta lista **es** el indice de cada fragmento, y la cabecera lo comprueba
    // al abrir. Por eso se recorre el catalogo en su orden y, dentro de cada tipo, la ranura
    // sin numero antes que las numeradas: asi la lista sale siempre igual.
    //
    // Anadir un proveedor cambia el orden de los que vienen detras. Eso no corrompe nada
    // --`Header::check_backends` se niega a abrir un volumen cuyos proveedores no casen-- y
    // es lo que arregla `multidrive providers add`, que reescribe la cabecera.
    for tipo in catalogo::TIPOS {
        for ranura in ranuras(tipo.clave) {
            // Configurado a medias es un error ruidoso; sin configurar, un silencio. La
            // diferencia importa: la forma clasica de tener pruebas que no prueban nada es
            // que un proveedor se salte entero por una variable mal escrita.
            let hay_algo = tipo
                .campos
                .iter()
                .any(|c| campo(&ranura, c.clave).is_some())
                || campo(&ranura, "ACCESS_TOKEN").is_some();
            if !hay_algo {
                continue;
            }
            backends.push(uno(tipo, &ranura, &campo)?);
        }
    }

    Ok(backends)
}

/// Las ranuras de un tipo, en orden: primero la de siempre, sin numero, y luego las
/// numeradas.
///
/// Numeradas en vez de una lista separada por comas porque una ruta de Windows lleva `:` y
/// puede llevar `,`, asi que cualquier separador seria ambiguo. Y el nombre va pegado a la
/// variable, que es lo que evita el fallo silencioso de reordenar una lista y que cada
/// fragmento se lea del sitio equivocado.
///
/// Que **todos** los tipos esten numerados y no solo las carpetas locales no es un adorno:
/// sin ello caben cuatro nubes como mucho, y un `rs(4,2)` pide seis. La promesa del proyecto
/// es repartir entre cuentas distintas, y con una sola cuenta por servicio no se puede
/// cumplir.
pub fn ranuras(tipo: &str) -> impl Iterator<Item = String> + use<'_> {
    std::iter::once(tipo.to_string()).chain((1..=MAX_RANURAS).map(move |i| format!("{tipo}{i}")))
}

/// Construye un proveedor suelto con valores recien tecleados, sin pasar por la
/// configuracion.
///
/// Es lo que permite **probarlo antes de guardarlo**. Guardar primero y descubrir despues
/// que las credenciales estaban mal deja la configuracion con un proveedor que no responde,
/// y a partir de ahi todo lo que enumere o mida sale con un hueco que hay que ir a explicar.
pub fn construir_uno(
    tipo: &catalogo::Tipo,
    ranura: &str,
    valor: &dyn Fn(&str) -> Option<String>,
) -> Result<OpendalBackend, BackendError> {
    uno(tipo, ranura, &|_, clave| {
        valor(clave)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// La primera ranura de ese tipo que no tenga nada configurado.
///
/// `None` si estan las treinta y dos ocupadas, que es mas de lo que tiene sentido repartir.
pub fn hueco_libre(tipo: &catalogo::Tipo, leer: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    ranuras(tipo.clave).find(|ranura| {
        !tipo.campos.iter().any(|c| {
            [PREFIJO, PREFIJO_VIEJO].iter().any(|p| {
                leer(&format!("{p}{ranura}_{}", c.clave)).is_some_and(|v| !v.trim().is_empty())
            })
        })
    })
}

/// Igual que [`hueco_libre`], mirando donde mira todo lo demas: entorno y fichero.
pub fn hueco_libre_configurado(tipo: &catalogo::Tipo) -> Result<Option<String>, BackendError> {
    let config = Config::cargar()?;
    Ok(hueco_libre(tipo, &|clave| {
        std::env::var(clave).ok().or_else(|| config.get(clave))
    }))
}

/// Las claves que hay que escribir para dejar configurada esa ranura.
///
/// Los campos vacios **no se incluyen**: una clave presente pero vacia no es lo mismo que
/// una ausente para quien lea el fichero luego a ojo, y de las opcionales la ausencia es
/// justo lo que significa "el valor por defecto".
pub fn claves_de(
    tipo: &catalogo::Tipo,
    ranura: &str,
    valor: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    tipo.campos
        .iter()
        .filter_map(|c| {
            let v = valor(c.clave)?;
            let v = v.trim();
            (!v.is_empty()).then(|| (format!("{PREFIJO}{ranura}_{}", c.clave), v.to_string()))
        })
        .collect()
}

/// El nombre con el que se conoce a la ranura, que es el que guarda la cabecera.
pub fn nombre_de_ranura(ranura: &str) -> String {
    ranura.to_lowercase()
}

fn uno(
    tipo: &catalogo::Tipo,
    ranura: &str,
    campo: &dyn Fn(&str, &str) -> Option<String>,
) -> Result<OpendalBackend, BackendError> {
    let nombre = nombre_de_ranura(ranura);
    // Fugado a `'static` porque los mensajes de error lo llevan y el tipo del error lo pide
    // asi. Son como mucho `MAX_RANURAS` por tipo y se hace una vez al arrancar.
    let quien: &'static str = Box::leak(nombre.clone().into_boxed_str());
    let raiz = || campo(ranura, "ROOT").unwrap_or_else(|| RAIZ_POR_DEFECTO.to_string());

    match tipo.clave {
        "FS" => {
            let dir = requerida(quien, campo(ranura, "ROOT"), &format!("{ranura}_ROOT"))?;
            OpendalBackend::fs(nombre, &dir)
        }
        "ONEDRIVE" => {
            let creds = oauth(quien, tipo.clave, ranura, campo)?;
            OpendalBackend::onedrive(nombre, &raiz(), &creds)
        }
        "GDRIVE" => {
            let creds = oauth(quien, tipo.clave, ranura, campo)?;
            OpendalBackend::gdrive(nombre, &raiz(), &creds)
        }
        "S3" => {
            let bucket = requerida(quien, campo(ranura, "BUCKET"), &format!("{ranura}_BUCKET"))?;
            let endpoint = requerida(
                quien,
                campo(ranura, "ENDPOINT"),
                &format!("{ranura}_ENDPOINT"),
            )?;
            let region = requerida(quien, campo(ranura, "REGION"), &format!("{ranura}_REGION"))?;
            let creds = pareja(
                quien,
                campo(ranura, "ACCESS_KEY_ID"),
                campo(ranura, "SECRET_ACCESS_KEY"),
                &format!("{ranura}_ACCESS_KEY_ID"),
                &format!("{ranura}_SECRET_ACCESS_KEY"),
            )?;
            OpendalBackend::s3(
                nombre,
                &bucket,
                &endpoint,
                &region,
                creds.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
            )
        }
        "WEBDAV" => {
            let endpoint = requerida(
                quien,
                campo(ranura, "ENDPOINT"),
                &format!("{ranura}_ENDPOINT"),
            )?;
            let creds = pareja(
                quien,
                campo(ranura, "USERNAME"),
                campo(ranura, "PASSWORD"),
                &format!("{ranura}_USERNAME"),
                &format!("{ranura}_PASSWORD"),
            )?;
            OpendalBackend::webdav(
                nombre,
                &endpoint,
                &raiz(),
                creds.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
            )
        }
        otro => unreachable!("tipo del catalogo sin constructor: {otro}"),
    }
}

/// Las credenciales OAuth2 de una ranura.
///
/// El token de acceso suelto se sigue aceptando —quien ya lo tenia escrito no deberia tener
/// que reescribirlo— pero no se ofrece al configurar: caduca en una hora. Ver
/// [`crate::catalogo`].
fn oauth(
    quien: &'static str,
    tipo: &str,
    ranura: &str,
    campo: &dyn Fn(&str, &str) -> Option<String>,
) -> Result<OauthCreds, BackendError> {
    if let Some(token) = campo(ranura, "ACCESS_TOKEN") {
        return Ok(OauthCreds::Acceso(token));
    }

    // Lo que identifica a la aplicacion puede venir de tres sitios, y lo de esta ranura gana:
    // quien escribe `GDRIVE2_CLIENT_ID` esta diciendo "para esta cuenta, mi aplicacion". Ver
    // [`crate::aplicacion`].
    let de_la_app = crate::aplicacion::configurada(tipo);
    let client_id =
        campo(ranura, "CLIENT_ID").or_else(|| de_la_app.as_ref().map(|a| a.client_id.clone()));
    let client_secret = campo(ranura, "CLIENT_SECRET")
        .or_else(|| de_la_app.as_ref().map(|a| a.client_secret.clone()));

    let (Some(refresh_token), Some(client_id), Some(client_secret)) =
        (campo(ranura, "REFRESH_TOKEN"), client_id, client_secret)
    else {
        return Err(BackendError::EntornoIncompleto {
            proveedor: quien,
            falta: format!(
                "{PREFIJO}{ranura}_ACCESS_TOKEN, o bien {PREFIJO}{ranura}_REFRESH_TOKEN \
                 + {PREFIJO}{ranura}_CLIENT_ID + {PREFIJO}{ranura}_CLIENT_SECRET"
            ),
        });
    };
    Ok(OauthCreds::Refresco {
        refresh_token,
        client_id,
        client_secret,
    })
}

fn requerida(
    proveedor: &'static str,
    valor: Option<String>,
    nombre: &str,
) -> Result<String, BackendError> {
    valor.ok_or_else(|| BackendError::EntornoIncompleto {
        proveedor,
        falta: format!("{PREFIJO}{nombre}"),
    })
}

/// Dos variables que solo valen juntas: o las dos o ninguna.
fn pareja(
    proveedor: &'static str,
    uno: Option<String>,
    dos: Option<String>,
    nombre_uno: &str,
    nombre_dos: &str,
) -> Result<Option<(String, String)>, BackendError> {
    match (uno, dos) {
        (Some(a), Some(b)) => Ok(Some((a, b))),
        (None, None) => Ok(None),
        (Some(_), None) => Err(BackendError::EntornoIncompleto {
            proveedor,
            falta: format!("{PREFIJO}{nombre_dos}"),
        }),
        (None, Some(_)) => Err(BackendError::EntornoIncompleto {
            proveedor,
            falta: format!("{PREFIJO}{nombre_uno}"),
        }),
    }
}

/// Comprueba que la cuenta que se va a anadir no este ya configurada.
///
/// **Es la comprobacion que evita prometer una redundancia que no existe.** Dos proveedores
/// sobre la misma cuenta se ven como dos en la ventana, cuentan como dos al elegir topologia
/// y se anuncian como "aguanta perder uno"; pero el dia que se pierda esa cuenta se pierden
/// las dos copias a la vez. El usuario habria pagado el doble de espacio por cero margen.
///
/// Se pregunta al proveedor nuevo y a los ya configurados **del mismo tipo**, porque una
/// cuenta de Drive y una de OneDrive nunca van a coincidir y preguntarselo seria gastar
/// peticiones.
///
/// Un `None` no se trata como "distinta": es "no se sabe", y no se bloquea por ello. Los
/// proveedores sin cuenta que preguntar --una carpeta local, un bucket-- pueden estar
/// duplicados sin que nadie aqui pueda detectarlo, y eso se avisa en otro sitio.
pub async fn ya_configurada(
    tipo: &catalogo::Tipo,
    nuevo: &dyn Backend,
) -> Result<Option<String>, BackendError> {
    let Some(mia) = nuevo.identidad().await? else {
        return Ok(None);
    };

    for b in configurados()? {
        if !es_ranura_de(b.name(), tipo.clave) {
            continue;
        }
        // Si uno de los ya configurados no contesta, no se puede decir que sea otra cuenta.
        // Se deja pasar en vez de bloquear: negarse a anadir un proveedor porque otro esta
        // caido seria peor que el riesgo que se intenta evitar.
        if let Ok(Some(suya)) = b.identidad().await
            && suya == mia
        {
            return Ok(Some(b.name().to_string()));
        }
    }
    Ok(None)
}

/// Si ese nombre de proveedor es una ranura de ese tipo: `gdrive`, `gdrive2`, `gdrive17`.
pub fn es_ranura_de(nombre: &str, tipo: &str) -> bool {
    let tipo = tipo.to_lowercase();
    match nombre.strip_prefix(&tipo) {
        Some("") => true,
        Some(resto) => resto.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::Backend;

    fn entorno(pares: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let mapa: HashMap<String, String> = pares
            .iter()
            .map(|(k, v)| (format!("{PREFIJO}{k}"), v.to_string()))
            .collect();
        move |clave: &str| mapa.get(clave).cloned()
    }

    /// Lee con el prefijo que se le diga, para poder mezclarlos.
    fn entorno_crudo(pares: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let mapa: HashMap<String, String> = pares
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |clave: &str| mapa.get(clave).cloned()
    }

    /// El orden y los nombres exactos de las ranuras, que es lo que el usuario acaba viendo
    /// y lo que la cabecera guarda para siempre.
    ///
    /// La primera **no lleva numero**: `gdrive`, `gdrive1`, `gdrive2`. Es la convencion con
    /// la que nacieron las carpetas locales y cambiarla ahora reescribiria la configuracion
    /// de quien ya la tenga.
    #[test]
    fn la_primera_ranura_no_lleva_numero_y_las_demas_empiezan_en_uno() {
        let primeras: Vec<String> = ranuras("GDRIVE").take(4).collect();
        assert_eq!(primeras, ["GDRIVE", "GDRIVE1", "GDRIVE2", "GDRIVE3"]);
        let nombres: Vec<String> = primeras.iter().map(|r| nombre_de_ranura(r)).collect();
        assert_eq!(nombres, ["gdrive", "gdrive1", "gdrive2", "gdrive3"]);
    }

    /// Las ranuras de un tipo se reconocen por el nombre, que es lo que tiene la cabecera.
    #[test]
    fn se_reconoce_que_ranuras_son_del_mismo_tipo() {
        for (nombre, es) in [
            ("gdrive", true),
            ("gdrive2", true),
            ("gdrive17", true),
            ("onedrive", false),
            // La trampa: empieza igual pero no es una ranura numerada. Si esto se colara,
            // la comprobacion de cuenta repetida preguntaria al proveedor equivocado.
            ("gdrive-viejo", false),
            ("gdrivex", false),
            ("fs", false),
        ] {
            assert_eq!(es_ranura_de(nombre, "GDRIVE"), es, "{nombre}");
        }
    }

    /// Lo que de verdad desbloquea numerar todos los tipos y no solo las carpetas.
    ///
    /// Sin esto caben cuatro nubes como mucho, una por servicio, y un `rs(4,2)` pide seis.
    /// La promesa del proyecto es repartir entre cuentas distintas; con una sola cuenta por
    /// servicio no se puede cumplir.
    #[test]
    fn caben_varias_cuentas_de_la_misma_nube() {
        let leer = entorno(&[
            ("GDRIVE_REFRESH_TOKEN", "r1"),
            ("GDRIVE_CLIENT_ID", "i1"),
            ("GDRIVE_CLIENT_SECRET", "s1"),
            ("GDRIVE2_REFRESH_TOKEN", "r2"),
            ("GDRIVE2_CLIENT_ID", "i2"),
            ("GDRIVE2_CLIENT_SECRET", "s2"),
            ("GDRIVE7_REFRESH_TOKEN", "r7"),
            ("GDRIVE7_CLIENT_ID", "i7"),
            ("GDRIVE7_CLIENT_SECRET", "s7"),
        ]);
        let nombres: Vec<String> = construir(&leer)
            .unwrap()
            .iter()
            .map(|b| b.name().to_string())
            .collect();
        assert_eq!(nombres, ["gdrive", "gdrive2", "gdrive7"]);
    }

    /// El orden **es** el indice de cada fragmento, y la cabecera lo comprueba al abrir.
    ///
    /// Este test es un ancla: si alguien reordena el catalogo, los volumenes que ya existan
    /// dejan de abrirse. Que falle aqui es mucho mas barato que descubrirlo en una maquina
    /// con datos.
    #[test]
    fn el_orden_de_los_proveedores_no_se_mueve() {
        let leer = entorno(&[
            ("WEBDAV_ENDPOINT", "https://nube/dav"),
            ("S3_BUCKET", "b"),
            ("S3_ENDPOINT", "https://s3"),
            ("S3_REGION", "r"),
            ("GDRIVE_ACCESS_TOKEN", "t"),
            ("ONEDRIVE_ACCESS_TOKEN", "t"),
            ("FS2_ROOT", "/tmp/dos"),
            ("FS_ROOT", "/tmp/cero"),
        ]);
        let nombres: Vec<String> = construir(&leer)
            .unwrap()
            .iter()
            .map(|b| b.name().to_string())
            .collect();
        assert_eq!(
            nombres,
            ["fs", "fs2", "onedrive", "gdrive", "s3", "webdav"],
            "el orden lo fija el catalogo, y dentro de cada tipo la ranura sin numero va \
             primero"
        );
    }

    /// Una ranura numerada a medias falla igual que la de siempre.
    ///
    /// Es el caso que de verdad aprieta: quien anade su segunda cuenta de Drive y se deja
    /// una variable tiene que enterarse, no quedarse con un volumen de un proveedor menos
    /// del que cree.
    #[test]
    fn una_ranura_numerada_a_medias_es_un_error_ruidoso() {
        let leer = entorno(&[
            ("ONEDRIVE3_REFRESH_TOKEN", "r"),
            ("ONEDRIVE3_CLIENT_ID", "i"),
        ]);
        let e = construir(&leer).unwrap_err().to_string();
        assert!(e.contains("onedrive3"), "{e}");
        assert!(e.contains("CLIENT_SECRET"), "{e}");
    }
    /// El `.env` que ya existia sigue funcionando. Romperlo obligaria a reescribir la
    /// configuracion de todo el mundo por un cambio de nombre.
    #[test]
    fn el_prefijo_antiguo_se_sigue_aceptando() {
        let b = construir(&entorno_crudo(&[(
            "MULTIDRIVE_TEST_FS_ROOT",
            "/tmp/multidrive-viejo",
        )]))
        .unwrap();
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn el_prefijo_nuevo_gana_al_antiguo() {
        let b = construir(&entorno_crudo(&[
            ("MULTIDRIVE_GDRIVE_ACCESS_TOKEN", "nuevo"),
            ("MULTIDRIVE_TEST_GDRIVE_ACCESS_TOKEN", "viejo"),
            ("MULTIDRIVE_GDRIVE_ROOT", "/nueva"),
            ("MULTIDRIVE_TEST_GDRIVE_ROOT", "/vieja"),
        ]))
        .unwrap();
        assert_eq!(
            b.len(),
            1,
            "no puede construir dos veces el mismo proveedor"
        );
    }

    /// Un valor en blanco con el prefijo nuevo no debe tapar uno bueno con el antiguo: la
    /// forma tipica de tener eso es una linea a medio escribir en el fichero.
    #[test]
    fn un_valor_vacio_no_tapa_al_del_otro_prefijo() {
        let b = construir(&entorno_crudo(&[
            ("MULTIDRIVE_FS_ROOT", "   "),
            ("MULTIDRIVE_TEST_FS_ROOT", "/tmp/multidrive-bueno"),
        ]))
        .unwrap();
        assert_eq!(b.len(), 1, "el vacio se ignora y se cae al antiguo");
    }

    /// Varios directorios locales, para poder montar un volumen con redundancia sin nubes.
    #[test]
    fn varios_directorios_locales_numerados() {
        let b = construir(&entorno(&[
            ("FS1_ROOT", "/tmp/uno"),
            ("FS2_ROOT", "/tmp/dos"),
            ("FS3_ROOT", "/tmp/tres"),
        ]))
        .unwrap();
        let nombres: Vec<&str> = b.iter().map(Backend::name).collect();
        assert_eq!(nombres, vec!["fs1", "fs2", "fs3"]);
    }

    /// El nombre va pegado al numero de la variable, no a la posicion en que se lea.
    ///
    /// Es lo que impide el fallo silencioso: si el nombre dependiera del orden, quitar el
    /// segundo de tres renombraria al tercero y cada fragmento se leeria del sitio
    /// equivocado, sin que la cabecera se enterara.
    #[test]
    fn el_nombre_no_depende_de_cuales_falten() {
        let b = construir(&entorno(&[
            ("FS1_ROOT", "/tmp/uno"),
            ("FS7_ROOT", "/tmp/siete"),
        ]))
        .unwrap();
        let nombres: Vec<&str> = b.iter().map(Backend::name).collect();
        assert_eq!(nombres, vec!["fs1", "fs7"]);
    }

    /// El de siempre sigue llamandose `fs` y conviviendo con los numerados.
    #[test]
    fn el_directorio_sin_numero_sigue_valiendo() {
        let b = construir(&entorno(&[
            ("FS_ROOT", "/tmp/viejo"),
            ("FS1_ROOT", "/tmp/uno"),
        ]))
        .unwrap();
        let nombres: Vec<&str> = b.iter().map(Backend::name).collect();
        assert_eq!(nombres, vec!["fs", "fs1"]);
    }

    #[test]
    fn sin_variables_no_hay_proveedores() {
        assert!(construir(&entorno(&[])).unwrap().is_empty());
    }

    #[test]
    fn onedrive_con_token_de_acceso() {
        let b = construir(&entorno(&[("ONEDRIVE_ACCESS_TOKEN", "abc")])).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name(), "onedrive");
    }

    #[test]
    fn gdrive_con_token_de_refresco() {
        let b = construir(&entorno(&[
            ("GDRIVE_REFRESH_TOKEN", "r"),
            ("GDRIVE_CLIENT_ID", "i"),
            ("GDRIVE_CLIENT_SECRET", "s"),
        ]))
        .unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name(), "gdrive");
    }

    /// El caso que justifica todo este modulo: media configuracion debe doler, no colarse.
    #[test]
    fn configuracion_a_medias_es_error() {
        let err = construir(&entorno(&[("ONEDRIVE_REFRESH_TOKEN", "r")])).unwrap_err();
        let texto = err.to_string();
        assert!(texto.contains("ONEDRIVE"), "{texto}");
        assert!(texto.contains("CLIENT_ID"), "{texto}");
    }

    #[test]
    fn solo_la_raiz_tambien_cuenta_como_intento() {
        // Poner ONEDRIVE_ROOT y olvidarse del token no debe traducirse en "no configurado".
        assert!(construir(&entorno(&[("ONEDRIVE_ROOT", "/x")])).is_err());
    }

    #[test]
    fn pareja_de_credenciales_incompleta_es_error() {
        let err = construir(&entorno(&[
            ("WEBDAV_ENDPOINT", "https://nube/remote.php/dav"),
            ("WEBDAV_USERNAME", "luis"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("WEBDAV_PASSWORD"), "{err}");
    }

    #[test]
    fn webdav_sin_credenciales_es_valido() {
        let b = construir(&entorno(&[("WEBDAV_ENDPOINT", "http://localhost/dav")])).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name(), "webdav");
    }

    #[test]
    fn varios_proveedores_a_la_vez() {
        let b = construir(&entorno(&[
            ("ONEDRIVE_ACCESS_TOKEN", "a"),
            ("GDRIVE_ACCESS_TOKEN", "b"),
            ("WEBDAV_ENDPOINT", "http://localhost/dav"),
        ]))
        .unwrap();
        let nombres: Vec<_> = b.iter().map(|x| x.name()).collect();
        assert_eq!(nombres, ["onedrive", "gdrive", "webdav"]);
    }

    #[test]
    fn directorio_local_como_proveedor() {
        let b = construir(&entorno(&[("FS_ROOT", "/tmp/multidrive-arnes")])).unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].name(), "fs");
    }

    #[test]
    fn s3_incompleto_dice_que_falta() {
        let err = construir(&entorno(&[("S3_BUCKET", "cubo")])).unwrap_err();
        assert!(err.to_string().contains("S3_ENDPOINT"), "{err}");
    }
}
