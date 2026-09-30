//! La maleta: la configuracion sellada, guardada en las nubes del propio usuario.
//!
//! Cambiar de equipo obligaba a volver a enlazar cada cuenta a mano. Esto lo arregla sin que
//! aparezca ningun servidor nuestro: la configuracion se cifra con la frase de paso y se deja
//! **en las cuentas del usuario**, al lado de la cabecera del volumen. En el equipo nuevo se
//! enlaza una sola cuenta —por el navegador, como siempre—, se escribe la frase, y de ahi
//! salen los demas proveedores. El volumen aparece solo: su cabecera ya estaba en ellos.
//!
//! # Que lleva dentro, y por que importa
//!
//! Lleva `REFRESH_TOKEN`, `CLIENT_SECRET` y las claves de S3: **acceso permanente a las nubes
//! de quien la abra**. Por eso:
//!
//! - Va cifrada con XChaCha20-Poly1305 y una clave derivada con Argon2id de la frase de paso.
//!   Quien entre en la cuenta de Drive se lleva el bulto; lo unico que lo protege es la frase.
//!   Una frase corta aqui es una frase corta delante de un atacante con tiempo y el fichero
//!   en la mano.
//! - **La frase no viaja nunca dentro**. Ver [`viaja`], y el test que lo fija.
//! - No se sube sola. Publicarla es una orden explicita: subir las llaves de alguien a su
//!   nube, aunque sea suya y vayan cifradas, es una decision que toma el, no el programa.
//!
//! # Por que la sal es suya y no la del volumen
//!
//! El bulto trae su propia sal, no la de la cabecera. Asi se abre **sin depender de que haya
//! volumen**, que es justo el caso de un equipo recien instalado, y la clave que lo cifra no
//! es la misma que cifra los trozos aunque la frase sea la misma. Cada cosa con su clave.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::join_all;
use multidrive_backend::{Backend, BackendError};
use multidrive_crypto::{MasterKey, SALT_LEN, generate_salt};
use serde::{Deserialize, Serialize};

use crate::CoreError;

/// Donde vive la maleta dentro de cada proveedor, al lado de la cabecera.
pub const CLAVE: &str = "multidrive-config.sellado";

/// Version del envoltorio. Va **en claro**, que es lo unico que se puede leer sin la frase.
const VERSION: u8 = 1;

/// Se cifra junto al contenido sin formar parte de el: separa esto de cualquier otra cosa
/// sellada con la misma clave. Un trozo del volumen no puede colarse como maleta ni al reves.
const ETIQUETA: &[u8] = b"multidrive-maleta-v1";

/// El prefijo de todo lo que es configuracion de MultiDrive.
const PREFIJO: &str = "MULTIDRIVE_";

/// Lo que **nunca** entra en la maleta, aunque lleve el prefijo.
///
/// - `MULTIDRIVE_PASSPHRASE` es la frase de paso. Meterla dentro seria cerrar la caja fuerte
///   y pegar la llave a la tapa: quien abriera la maleta tendria ademas el volumen entero.
/// - `MULTIDRIVE_CONFIG` es una ruta de **este** equipo. En otro no existe, y apuntar ahi
///   dejaria la configuracion buscandose donde no esta.
const NUNCA: &[&str] = &["MULTIDRIVE_PASSPHRASE", "MULTIDRIVE_CONFIG"];

/// Si una clave de configuracion viaja en la maleta.
pub fn viaja(clave: &str) -> bool {
    clave.starts_with(PREFIJO) && !NUNCA.contains(&clave)
}

/// Las claves que viajan, leidas de **un fichero concreto** y de nada mas.
///
/// Existe por Android. Alli no hay entorno que exportar ni ninguno de los sitios donde
/// [`claves_de_este_equipo`] mira: la aplicacion guarda su configuracion en un fichero de su
/// propio directorio y ese fichero es toda la verdad que hay. Llamar a la otra desde el movil
/// devolveria un mapa vacio y se guardaria una maleta sin nada dentro, que es peor que no
/// guardarla: parece que se hizo.
pub fn claves_de(ruta: &std::path::Path) -> Result<BTreeMap<String, String>, CoreError> {
    let cfg = multidrive_backend::config::Config::desde(std::slice::from_ref(&ruta.to_path_buf()))?;
    Ok(cfg
        .claves()
        .filter(|c| viaja(c))
        .filter_map(|c| cfg.get(c).map(|v| (c.to_string(), v)))
        .collect())
}

/// Lo que este equipo esta usando de verdad: el fichero de configuracion **y** el entorno.
///
/// No basta con leer el fichero. Quien exporta las variables en su terminal tiene proveedores
/// que funcionan y no estan escritos en ningun sitio, y una maleta que se los dejara fuera
/// seria una maleta que miente sobre lo que lleva. El entorno manda sobre el fichero, igual
/// que en todo lo demas.
pub fn claves_de_este_equipo() -> Result<BTreeMap<String, String>, CoreError> {
    let cfg = multidrive_backend::config::Config::cargar()?;
    let mut out: BTreeMap<String, String> = cfg
        .claves()
        .filter(|c| viaja(c))
        .filter_map(|c| cfg.get(c).map(|v| (c.to_string(), v)))
        .collect();

    for (k, v) in std::env::vars() {
        if viaja(&k) && !v.trim().is_empty() {
            out.insert(k, v.trim().to_string());
        }
    }
    Ok(out)
}

/// Lo que sale al abrir una maleta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Maleta {
    /// Cuando se hizo, en segundos desde 1970. Sirve para saber si la que hay es mas vieja
    /// que este equipo antes de pisar nada.
    pub hecha_en: u64,
    /// Las claves de configuracion, listas para escribirse en el fichero.
    pub claves: BTreeMap<String, String>,
}

/// Lo que se cifra. Va aparte del envoltorio porque **nada de esto se lee sin la frase**,
/// ni siquiera la fecha.
#[derive(Serialize, Deserialize)]
struct Contenido {
    hecha_en: u64,
    claves: BTreeMap<String, String>,
}

/// Empaqueta la configuracion en un bulto cifrado.
///
/// De `claves` solo entra lo que [`viaja`] deja pasar. El resto se queda fuera en silencio: no
/// es un error tener la frase en el entorno, lo seria mandarla de viaje.
pub fn empaquetar(claves: &BTreeMap<String, String>, frase: &str) -> Result<Vec<u8>, CoreError> {
    let dentro: BTreeMap<String, String> = claves
        .iter()
        .filter(|(k, _)| viaja(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if dentro.is_empty() {
        return Err(CoreError::Maleta(
            "no hay ninguna configuracion que guardar".into(),
        ));
    }

    let hecha_en = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let claro = serde_json::to_vec(&Contenido {
        hecha_en,
        claves: dentro,
    })?;

    let sal = generate_salt();
    let clave = MasterKey::derive(frase, &sal)?;
    let sellado = clave.seal(&claro, ETIQUETA)?;

    let mut bulto = Vec::with_capacity(1 + SALT_LEN + sellado.len());
    bulto.push(VERSION);
    bulto.extend_from_slice(&sal);
    bulto.extend_from_slice(&sellado);
    Ok(bulto)
}

/// Abre un bulto con la frase de paso.
///
/// No distingue «la frase esta mal» de «esto esta corrompido», y no es por pereza: el que
/// prueba frases no deberia poder usar el mensaje para saber si va por buen camino.
pub fn desempaquetar(bulto: &[u8], frase: &str) -> Result<Maleta, CoreError> {
    if bulto.len() < 1 + SALT_LEN {
        return Err(CoreError::Maleta(
            "esto no es una maleta de MultiDrive: se queda corto".into(),
        ));
    }
    if bulto[0] != VERSION {
        return Err(CoreError::Maleta(format!(
            "esta maleta es de la version {} y esta copia entiende la {VERSION}. \
             Actualiza MultiDrive.",
            bulto[0]
        )));
    }

    let sal = &bulto[1..1 + SALT_LEN];
    let clave = MasterKey::derive(frase, sal)?;
    let claro = clave.open(&bulto[1 + SALT_LEN..], ETIQUETA).map_err(|_| {
        CoreError::Maleta(
            "la frase de paso no abre esta maleta. Tiene que ser la misma con la que se \
             guardo."
                .into(),
        )
    })?;

    let dentro: Contenido = serde_json::from_slice(&claro).map_err(|_| {
        CoreError::Maleta("la maleta se abrio pero lo de dentro no se entiende".into())
    })?;

    Ok(Maleta {
        hecha_en: dentro.hecha_en,
        claves: dentro.claves,
    })
}

/// Deja el bulto en todos los proveedores, y dice en cuales quedo.
///
/// Se replica igual que la cabecera y por lo mismo: perder una cuenta no puede dejar sin
/// configuracion a las demas. Que uno falle no aborta al resto; quien llama decide si con los
/// que quedan le vale.
pub async fn guardar(backends: &[Arc<dyn Backend>], bulto: &[u8]) -> Vec<(String, Option<String>)> {
    let escrituras = backends.iter().map(|b| {
        let datos = bulto.to_vec();
        async move {
            let fallo = b.put(CLAVE, datos).await.err().map(|e| e.to_string());
            (b.name().to_string(), fallo)
        }
    });
    join_all(escrituras).await
}

/// Si alguno de estos proveedores guarda una maleta.
///
/// **No hace falta la frase**: solo se pregunta si el bulto esta ahi, no se abre. Sirve para
/// el momento en que alguien acaba de enlazar una cuenta en un equipo nuevo: si esa cuenta ya
/// trae maleta, lo que toca es pedirle la frase y traerse el resto, no dejarle buscando un
/// boton que no sabe que existe.
///
/// Un proveedor que no contesta cuenta como que no la tiene. Aqui eso no hace daño: lo peor
/// que pasa es no ofrecer algo que se puede pedir a mano.
pub async fn hay(backends: &[Arc<dyn Backend>]) -> bool {
    join_all(
        backends
            .iter()
            .map(|b| async move { b.exists(CLAVE).await.unwrap_or(false) }),
    )
    .await
    .into_iter()
    .any(|x| x)
}
/// Busca la maleta en los proveedores y devuelve la primera que abra la frase.
///
/// Prueba todas a proposito. Si en una cuenta quedo una maleta vieja —de antes de cambiar la
/// frase— lo que corresponde es seguir buscando, no rendirse en la primera.
///
/// `Ok(None)` es que no hay ninguna, que es lo normal en cuentas donde nunca se publico.
pub async fn recoger(
    backends: &[Arc<dyn Backend>],
    frase: &str,
) -> Result<Option<(String, Maleta)>, CoreError> {
    let lecturas = backends.iter().map(|b| async move {
        let r = match b.get(CLAVE).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(BackendError::NotFound(_)) => Ok(None),
            Err(e) => Err(e.to_string()),
        };
        (b.name().to_string(), r)
    });

    let mut habia = false;
    let mut fallos = Vec::new();

    for (nombre, r) in join_all(lecturas).await {
        let bytes = match r {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(e) => {
                fallos.push(format!("{nombre}: {e}"));
                continue;
            }
        };
        habia = true;
        match desempaquetar(&bytes, frase) {
            Ok(m) => return Ok(Some((nombre, m))),
            Err(e) => fallos.push(format!("{nombre}: {e}")),
        }
    }

    if habia {
        return Err(CoreError::Maleta(format!(
            "hay maleta pero no se pudo abrir ninguna: {}",
            fallos.join("; ")
        )));
    }
    if !fallos.is_empty() {
        return Err(CoreError::Maleta(format!(
            "no se pudo mirar en todos los proveedores: {}",
            fallos.join("; ")
        )));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::backends;

    fn claves() -> BTreeMap<String, String> {
        [
            ("MULTIDRIVE_GDRIVE_REFRESH_TOKEN", "1//un-token-larguisimo"),
            ("MULTIDRIVE_GDRIVE1_REFRESH_TOKEN", "1//otro-token"),
            ("MULTIDRIVE_APP_GDRIVE_CLIENT_ID", "el-id.apps.google"),
            ("MULTIDRIVE_S3_SECRET_ACCESS_KEY", "un-secreto-de-s3"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn lo_que_se_guarda_es_lo_que_sale() {
        let bulto = empaquetar(&claves(), "la frase de paso").unwrap();
        let m = desempaquetar(&bulto, "la frase de paso").unwrap();
        assert_eq!(m.claves, claves());
        assert!(m.hecha_en > 1_700_000_000, "la fecha no se guardo");
    }

    /// **La frase de paso no viaja dentro.**
    ///
    /// Es lo unico que separa esta maleta de regalarle el volumen a quien entre en la cuenta
    /// de Drive. Se comprueba de las dos formas: que no sale al abrirla, y que sus bytes no
    /// estan en el bulto ni de casualidad.
    #[test]
    fn la_frase_de_paso_nunca_entra_en_la_maleta() {
        let mut c = claves();
        c.insert(
            "MULTIDRIVE_PASSPHRASE".into(),
            "esta-es-la-frase-del-volumen".into(),
        );
        c.insert(
            "MULTIDRIVE_CONFIG".into(),
            "C:/una/ruta/de/aqui.conf".into(),
        );

        let bulto = empaquetar(&c, "la frase de paso").unwrap();
        let m = desempaquetar(&bulto, "la frase de paso").unwrap();

        assert!(!m.claves.contains_key("MULTIDRIVE_PASSPHRASE"));
        assert!(!m.claves.contains_key("MULTIDRIVE_CONFIG"));
        assert_eq!(m.claves, claves(), "no deberia haber entrado nada mas");

        let aguja = b"esta-es-la-frase-del-volumen";
        assert!(
            !bulto.windows(aguja.len()).any(|v| v == aguja),
            "la frase aparece en el bulto"
        );
    }

    #[test]
    fn otra_frase_no_la_abre() {
        let bulto = empaquetar(&claves(), "la buena").unwrap();
        let e = desempaquetar(&bulto, "la mala").unwrap_err();
        assert!(matches!(e, CoreError::Maleta(_)), "{e}");
    }

    /// Ni el token ni ningun secreto pueden leerse sin la frase.
    #[test]
    fn el_bulto_no_ensena_nada_por_fuera() {
        let bulto = empaquetar(&claves(), "la frase").unwrap();
        for aguja in [
            &b"1//un-token-larguisimo"[..],
            &b"un-secreto-de-s3"[..],
            &b"MULTIDRIVE_GDRIVE_REFRESH_TOKEN"[..],
        ] {
            assert!(
                !bulto.windows(aguja.len()).any(|v| v == aguja),
                "se ve '{}' en claro",
                String::from_utf8_lossy(aguja)
            );
        }
    }

    /// Dos maletas del mismo contenido y la misma frase salen distintas.
    ///
    /// Si salieran iguales, quien viera dos copias sabria que la configuracion no ha cambiado
    /// entre una y otra, que ya es saber algo.
    #[test]
    fn dos_maletas_iguales_no_se_parecen() {
        let a = empaquetar(&claves(), "la frase").unwrap();
        let b = empaquetar(&claves(), "la frase").unwrap();
        assert_ne!(a, b);
        assert_eq!(
            desempaquetar(&a, "la frase").unwrap().claves,
            desempaquetar(&b, "la frase").unwrap().claves
        );
    }

    #[test]
    fn una_maleta_vacia_no_se_hace() {
        let sola = [("OTRA_COSA".to_string(), "x".to_string())]
            .into_iter()
            .collect();
        assert!(matches!(
            empaquetar(&sola, "la frase"),
            Err(CoreError::Maleta(_))
        ));
    }

    #[test]
    fn cualquier_cosa_no_es_una_maleta() {
        assert!(desempaquetar(b"", "x").is_err());
        assert!(desempaquetar(b"cuatro", "x").is_err());
        let mut futura = vec![9u8];
        futura.extend_from_slice(&[0; SALT_LEN + 40]);
        let e = desempaquetar(&futura, "x").unwrap_err();
        assert!(e.to_string().contains("version"), "{e}");
    }

    /// Saber si hay maleta **no necesita la frase**, que es lo que permite ofrecerla nada mas
    /// enlazar una cuenta, antes de pedirle nada a nadie.
    #[tokio::test]
    async fn se_ve_que_hay_maleta_sin_abrirla() {
        let bs = backends(2);
        assert!(
            !hay(&bs).await,
            "una cuenta recien enlazada no tiene maleta"
        );

        let bulto = empaquetar(&claves(), "la frase").unwrap();
        guardar(&bs[..1], &bulto).await;

        assert!(hay(&bs).await, "con una que la tenga basta");
        assert!(
            desempaquetar(&bulto, "otra frase").is_err(),
            "y sigue cerrada"
        );
    }
    #[test]
    fn viaja_deja_fuera_lo_que_no_es_configuracion() {
        assert!(viaja("MULTIDRIVE_GDRIVE_REFRESH_TOKEN"));
        assert!(!viaja("MULTIDRIVE_PASSPHRASE"));
        assert!(!viaja("MULTIDRIVE_CONFIG"));
        assert!(!viaja("PATH"));
        assert!(!viaja("AWS_SECRET_ACCESS_KEY"));
    }
}
