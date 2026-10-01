//! Como se construye un cliente HTTP, que en Android no es como en los demas sitios.
//!
//! # El problema
//!
//! `reqwest` trae `rustls-platform-verifier`, que valida los certificados contra el almacen de
//! confianza del sistema. Es lo que se quiere en todas partes menos en una: **en Android ese
//! verificador comprueba la revocacion, y un certificado sin OCSP ni CRL no se puede
//! comprobar**. El resultado no es «no se pudo comprobar»: es `Revoked`.
//!
//! Comprobado en un telefono el 29 de septiembre de 2026, contra tres sitios:
//!
//! ```text
//! multidrive.net              invalid peer certificate: Revoked
//! oauth2.googleapis.com       invalid peer certificate: Revoked
//! login.microsoftonline.com   200 OK
//! ```
//!
//! Ninguno estaba revocado. Let's Encrypt ya no emite OCSP ni CRL, y Google Trust Services
//! tampoco; Microsoft todavia si, y por eso el tercero pasa. Como Google Drive es el proveedor
//! principal, eso deja la aplicacion de Android sin poder hablar con las nubes.
//!
//! # Lo que se hace, y lo que cuesta
//!
//! En Android, y **solo** en Android, el almacen de confianza pasa a ser `webpki-roots`: las
//! raices de Mozilla, compiladas dentro del binario.
//!
//! Lo que se pierde hay que tenerlo presente: ese almacen es **nuestro y no el del sistema**,
//! asi que no recoge las actualizaciones del sistema operativo ni las raices que haya anadido
//! una empresa o el propio usuario, y mantenerlo al dia pasa a ser cosa nuestra. Se acepta
//! porque la alternativa es que la aplicacion no funcione.
//!
//! En el escritorio no se toca nada: ahi el verificador de la plataforma funciona, recoge las
//! decisiones de confianza del sistema y es mejor que cualquier lista que llevemos nosotros.

use std::sync::OnceLock;
use std::time::Duration;

/// Cliente compartido.
///
/// Uno solo para todo el proceso: reutiliza conexiones y, sobre todo, no rehace el saludo TLS
/// en cada peticion. Contra una nube con ~6,5 s de latencia por operacion, eso no es una
/// optimizacion menor.
pub fn cliente() -> &'static reqwest::Client {
    static CLIENTE: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENTE.get_or_init(|| construir(Duration::from_secs(20)))
}

/// El transporte que se le da a OpenDAL, sobre el mismo cliente.
///
/// OpenDAL se fabrica uno por su cuenta si no se le da ninguno, y ese no pasaria por lo de
/// aqui. En el escritorio daria igual; en Android seria la diferencia entre hablar con las
/// nubes y no.
pub fn transporte() -> opendal::HttpTransporter {
    opendal::HttpTransporter::new(opendal_http_transport_reqwest::ReqwestTransport::new(
        cliente().clone(),
    ))
}

/// Un cliente con el tiempo de espera que se pida, por si algo necesita otro.
pub fn construir(espera: Duration) -> reqwest::Client {
    let b = reqwest::Client::builder().timeout(espera);
    afinar(b).build().unwrap_or_default()
}

#[cfg(target_os = "android")]
fn afinar(b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    // `with_no_client_auth` y no autenticacion de cliente: ninguna de las nubes la usa, y
    // pedirla seria prometer algo que no hay.
    let raices = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(raices)
        .with_no_client_auth();
    b.tls_backend_preconfigured(config)
}

#[cfg(not(target_os = "android"))]
fn afinar(b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    // El verificador de la plataforma, que es el que trae `reqwest` por defecto. Recoge las
    // decisiones de confianza del sistema --incluidas las de una empresa o las del usuario--
    // y se actualiza con el sistema operativo. Ninguna lista nuestra puede decir lo mismo.
    b
}

/// Un error de red **con sus causas**.
///
/// `reqwest` dice «error sending request for url (...)» y guarda el motivo de verdad --que no
/// hay DNS, que el certificado no valida, que la conexion se rechazo-- en la cadena de causas,
/// que `to_string()` no imprime. Sin ellas, el mensaje que llega a quien lo sufre y a quien
/// tiene que arreglarlo no distingue entre un telefono sin datos y un almacen de confianza mal
/// armado, que son dos problemas sin nada en comun.
///
/// Costo una tarde averiguarlo en un telefono prestado. El mensaje decia exactamente lo mismo
/// en los dos casos.
pub fn con_causas(e: &dyn std::error::Error) -> String {
    let mut dicho = e.to_string();
    let mut fuente = e.source();
    while let Some(f) = fuente {
        let trozo = f.to_string();
        // Las capas repiten el mensaje de la de abajo mas de lo que parece; repetirlo aqui
        // convertiria la causa en ruido.
        if !dicho.contains(&trozo) {
            dicho.push_str(": ");
            dicho.push_str(&trozo);
        }
        fuente = f.source();
    }
    dicho
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Que se pueda construir es lo unico comprobable sin red, y no es poco: en Android la
    /// configuracion de TLS se arma a mano, y armarla mal se veria aqui.
    #[test]
    fn el_cliente_se_construye() {
        let _ = cliente();
        let _ = construir(Duration::from_secs(1));
    }

    #[derive(Debug)]
    struct Capa(&'static str, Option<Box<Capa>>);

    impl std::fmt::Display for Capa {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Capa {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1
                .as_deref()
                .map(|c| c as &(dyn std::error::Error + 'static))
        }
    }

    /// La causa de abajo llega arriba. Es lo unico que distingue «sin datos» de «el
    /// certificado no valida», y las dos capas de arriba dicen lo mismo en los dos casos.
    #[test]
    fn se_dicen_las_causas() {
        let e = Capa(
            "error sending request",
            Some(Box::new(Capa(
                "invalid peer certificate",
                Some(Box::new(Capa("Revoked", None))),
            ))),
        );
        assert_eq!(
            con_causas(&e),
            "error sending request: invalid peer certificate: Revoked"
        );
    }

    /// Una capa que ya repite lo de abajo no lo dice dos veces.
    #[test]
    fn no_se_repite_lo_que_ya_estaba() {
        let e = Capa(
            "fallo de red: sin ruta",
            Some(Box::new(Capa("sin ruta", None))),
        );
        assert_eq!(con_causas(&e), "fallo de red: sin ruta");
    }
}
