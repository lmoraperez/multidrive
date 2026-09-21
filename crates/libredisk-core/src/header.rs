//! Cabecera del volumen: lo unico que se guarda **en claro**.
//!
//! Hay un problema de arranque en un sistema cifrado de extremo a extremo: para descifrar
//! algo hace falta la clave, la clave se deriva de la frase de paso **y de una sal**, y la
//! sal tiene que salir de algun sitio. No puede ir cifrada, porque para descifrarla haria
//! falta la clave que todavia no existe.
//!
//! La cabecera resuelve eso, y de paso guarda las dos cosas mas que hacen falta para abrir un
//! volumen sin adivinarlas: la topologia y el orden de los proveedores.
//!
//! Nada de esto es secreto. Una sal no lo es —su trabajo es que dos volumenes con la misma
//! frase de paso no compartan clave, no esconderse—, y la topologia y los nombres tampoco
//! dicen nada del contenido. Lo que **si** revela la cabecera es que aqui hay un volumen de
//! LibreDisk, cuantos proveedores tiene y como se llaman.

use std::sync::Arc;

use futures::future::join_all;
use libredisk_backend::{Backend, BackendError};
use serde::{Deserialize, Serialize};

use crate::CoreError;

/// Clave conocida donde vive la cabecera, en claro, replicada en todos los proveedores.
pub const HEADER_KEY: &str = "libredisk-volume.json";

/// Version del formato. Sube cuando un volumen viejo deje de poder abrirse tal cual.
pub const HEADER_VERSION: u32 = 1;

/// Lo minimo para poder abrir un volumen sabiendo solo la frase de paso.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    /// Topologia tal y como se escribe: `rs:4,2`, `mirror:3`, `concat`.
    ///
    /// Se guarda porque sin ella no se sabe cuantos fragmentos hacen falta ni como juntarlos,
    /// y equivocarse no da un error limpio: da datos que no descifran.
    pub layout: String,
    /// Sal de derivacion de la clave maestra.
    pub salt: Vec<u8>,
    /// Nombres de los proveedores, **en orden**.
    ///
    /// El orden es parte del formato, no una comodidad: el indice de un fragmento es la
    /// posicion de su proveedor, asi que abrir el volumen con los mismos proveedores en otro
    /// orden leeria cada fragmento del sitio equivocado.
    pub backends: Vec<String>,
}

impl Header {
    /// Comprueba que estos proveedores son los del volumen, en el mismo orden.
    pub fn check_backends(&self, backends: &[Arc<dyn Backend>]) -> Result<(), CoreError> {
        let actuales: Vec<&str> = backends.iter().map(|b| b.name()).collect();
        if actuales.len() != self.backends.len() {
            return Err(CoreError::BackendMismatch(format!(
                "el volumen tiene {} proveedores y se han configurado {}",
                self.backends.len(),
                actuales.len()
            )));
        }
        for (i, (esperado, actual)) in self.backends.iter().zip(&actuales).enumerate() {
            if esperado != actual {
                return Err(CoreError::BackendMismatch(format!(
                    "en la posicion {i} el volumen espera '{esperado}' y hay '{actual}'; \
                     el orden es parte del formato"
                )));
            }
        }
        Ok(())
    }
}

/// Escribe la cabecera en todos los proveedores.
pub async fn write(backends: &[Arc<dyn Backend>], header: &Header) -> Result<(), CoreError> {
    let bytes = serde_json::to_vec_pretty(header)?;

    let escrituras = backends.iter().map(|b| {
        let datos = bytes.clone();
        async move { b.put(HEADER_KEY, datos).await }
    });
    let logradas = join_all(escrituras)
        .await
        .into_iter()
        .filter(Result::is_ok)
        .count();

    if logradas == 0 {
        return Err(CoreError::HeaderNotWritten);
    }
    Ok(())
}

/// Lee la cabecera. `None` si no hay volumen.
///
/// Si dos proveedores devuelven cabeceras distintas se da error en vez de elegir una. Eso
/// significa que se estan mezclando dos volumenes, y adivinar cual es el bueno solo serviria
/// para escribir encima del otro.
pub async fn read(backends: &[Arc<dyn Backend>]) -> Result<Option<Header>, CoreError> {
    let lecturas = backends.iter().map(|b| async move {
        match b.get(HEADER_KEY).await {
            Ok(bytes) => Some((b.name().to_string(), bytes)),
            // Que no este es lo normal en un proveedor recien anadido.
            Err(BackendError::NotFound(_)) => None,
            Err(_) => None,
        }
    });

    let mut encontrada: Option<(String, Header)> = None;
    for (nombre, bytes) in join_all(lecturas).await.into_iter().flatten() {
        let header: Header = serde_json::from_slice(&bytes)?;
        match &encontrada {
            None => encontrada = Some((nombre, header)),
            Some((otro, previa)) if *previa != header => {
                return Err(CoreError::HeaderConflict(format!(
                    "'{otro}' y '{nombre}' describen volumenes distintos"
                )));
            }
            Some(_) => {}
        }
    }

    Ok(encontrada.map(|(_, h)| h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::backends;

    fn cabecera(n: usize) -> Header {
        Header {
            version: HEADER_VERSION,
            layout: "rs:4,2".into(),
            salt: vec![7u8; 16],
            backends: (0..n).map(|i| format!("proveedor-{i}")).collect(),
        }
    }

    #[tokio::test]
    async fn ida_y_vuelta() {
        let bs = backends(6);
        assert!(read(&bs).await.unwrap().is_none(), "todavia no hay volumen");

        let h = cabecera(6);
        write(&bs, &h).await.unwrap();
        assert_eq!(read(&bs).await.unwrap().unwrap(), h);
    }

    /// Va en espejo: tiene que poder leerse aunque solo quede un proveedor.
    #[tokio::test]
    async fn se_lee_aunque_solo_quede_un_proveedor() {
        let bs = backends(6);
        let h = cabecera(6);
        write(&bs, &h).await.unwrap();

        for b in bs.iter().take(5) {
            b.delete(HEADER_KEY).await.unwrap();
        }
        assert_eq!(read(&bs).await.unwrap().unwrap(), h);
    }

    /// Mezclar dos volumenes es un error, no una eleccion.
    #[tokio::test]
    async fn cabeceras_distintas_dan_conflicto() {
        let bs = backends(2);
        let mut otra = cabecera(2);
        otra.salt = vec![9u8; 16];

        write(&bs[..1], &cabecera(2)).await.unwrap();
        write(&bs[1..], &otra).await.unwrap();

        assert!(matches!(read(&bs).await, Err(CoreError::HeaderConflict(_))));
    }

    #[test]
    fn el_orden_de_los_proveedores_es_parte_del_formato() {
        let bs = backends(3);
        let h = Header {
            backends: vec![
                "proveedor-0".into(),
                "proveedor-1".into(),
                "proveedor-2".into(),
            ],
            ..cabecera(3)
        };
        h.check_backends(&bs).unwrap();

        // Los mismos, al reves: leeria cada fragmento del sitio equivocado.
        let dados_la_vuelta: Vec<_> = bs.iter().rev().cloned().collect();
        assert!(matches!(
            h.check_backends(&dados_la_vuelta),
            Err(CoreError::BackendMismatch(_))
        ));

        // Y uno de menos tambien se caza.
        assert!(matches!(
            h.check_backends(&bs[..2]),
            Err(CoreError::BackendMismatch(_))
        ));
    }
}
