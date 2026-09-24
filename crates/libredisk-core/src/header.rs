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

use libredisk_crypto::MasterKey;
use libredisk_raid::Layout;

use crate::{ChunkStore, CoreError};

/// Clave conocida donde vive la cabecera, en claro, replicada en todos los proveedores.
pub const HEADER_KEY: &str = "libredisk-volume.json";

/// Version del formato. Sube cuando un volumen viejo deje de poder abrirse tal cual.
pub const HEADER_VERSION: u32 = 1;

/// Lo minimo para poder abrir un volumen sabiendo solo la frase de paso.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    /// Topologia tal y como se escribe: `rs:4,2`, `espejo:3`, `union:2`, `concat`.
    ///
    /// Puede traer nombres viejos --`mirror`, `stripe`-- de volumenes creados antes del
    /// cambio: `Layout::from_str` los sigue aceptando, y por eso esto se guarda tal cual en
    /// vez de reescribirse.
    ///
    /// Se guarda porque sin ella no se sabe cuantos fragmentos hacen falta ni como juntarlos,
    /// y equivocarse no da un error limpio: da datos que no descifran.
    pub layout: String,
    /// Sal de derivacion de la clave maestra.
    pub salt: Vec<u8>,
    /// Que reparto de fragmentos es el bueno.
    ///
    /// Cambiar la topologia de forma que los fragmentos se muevan obliga a escribir los
    /// nuevos sin destruir los viejos, porque el volumen se sigue leyendo mientras tanto. Un
    /// reparto distinto son claves distintas, y **esta cabecera es la que decide cual vale**:
    /// publicarla es lo que hace efectivo el cambio.
    ///
    /// Va con valor por defecto para que las cabeceras escritas antes de que esto existiera
    /// se sigan leyendo, que son reparto 0.
    #[serde(default)]
    pub reparto: u32,
    /// Nombres de los proveedores, **en orden**.
    ///
    /// El orden es parte del formato, no una comodidad: el indice de un fragmento es la
    /// posicion de su proveedor, asi que abrir el volumen con los mismos proveedores en otro
    /// orden leeria cada fragmento del sitio equivocado.
    pub backends: Vec<String>,
    /// Cuanto aporta cada proveedor, **en el mismo orden**.
    ///
    /// Solo lo usa `lineal`, que elige a que proveedor va cada trozo a partir de estos
    /// pesos. Va en la cabecera y no se recalcula al arrancar **a proposito**: si dependiera
    /// de lo que cada nube declare libre en cada momento, un trozo escrito ayer se buscaria
    /// hoy en otro sitio y el volumen se desmontaria solo.
    ///
    /// Vacio en las demas topologias, y con valor por defecto para que las cabeceras
    /// escritas antes de que esto existiera se sigan leyendo.
    #[serde(default)]
    pub pesos: Vec<u64>,
}

impl Header {
    /// Construye el almacen que describe esta cabecera.
    ///
    /// Reune las cuatro cosas que hay que acertar a la vez para poder leer un volumen: que los
    /// proveedores sean los suyos y en su orden, que la topologia sea la declarada, que la
    /// clave salga de **esta** sal, y que se busquen los fragmentos del reparto que vale.
    ///
    /// Vive aqui y no en quien llama porque olvidarse de una sola de las cuatro no da un error
    /// limpio: da un volumen que no se abre, o peor, uno que busca los fragmentos donde ya no
    /// estan. Ya paso con el reparto, que se quedo sin aplicar en tres sitios a la vez.
    pub fn almacen(
        &self,
        backends: Vec<Arc<dyn Backend>>,
        frase: &str,
        cache_bytes: usize,
    ) -> Result<ChunkStore, CoreError> {
        self.check_backends(&backends)?;
        let layout: Layout = self.layout.parse().map_err(|_| {
            CoreError::BackendMismatch(format!(
                "la cabecera declara una topologia invalida: {}",
                self.layout
            ))
        })?;
        let key = MasterKey::derive(frase, &self.salt)?;
        Ok(ChunkStore::new(layout, backends, key)?
            .with_cache(cache_bytes)
            .with_reparto(self.reparto)
            .with_pesos(self.pesos.clone()))
    }

    /// La topologia que declara, ya interpretada.
    pub fn layout(&self) -> Result<Layout, CoreError> {
        self.layout.parse().map_err(|_| {
            CoreError::BackendMismatch(format!(
                "la cabecera declara una topologia invalida: {}",
                self.layout
            ))
        })
    }

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

/// Crea la cabecera de un volumen nuevo y devuelve el almacen listo para estrenarlo.
///
/// Quien llama termina con [`crate::tree::Volume::create`], que publica la primera raiz.
///
/// # Por que la cabecera va primero
///
/// Porque lleva la sal, y sin la sal nadie puede volver a derivar la clave. Una raiz
/// publicada antes que su cabecera seria ilegible para siempre, incluso sabiendo la frase.
///
/// # Por que no se sobrescribe nunca
///
/// Una cabecera nueva trae una sal nueva, y con otra sal la clave es otra: todo lo que
/// hubiera escrito antes quedaria inalcanzable sin ningun aviso. Si una creacion quedo a
/// medias, borrar el objeto a mano tiene que ser una decision de quien lo hace.
pub async fn crear(
    backends: Vec<Arc<dyn Backend>>,
    layout: Layout,
    frase: &str,
    pesos: Vec<u64>,
) -> Result<ChunkStore, CoreError> {
    if backends.len() != layout.shard_count() {
        return Err(CoreError::BackendCount {
            layout,
            needed: layout.shard_count(),
            given: backends.len(),
        });
    }
    if read(&backends).await?.is_some() {
        return Err(CoreError::YaHayVolumen);
    }

    let salt = libredisk_crypto::generate_salt();
    let header = Header {
        version: HEADER_VERSION,
        layout: layout.to_string(),
        reparto: 0,
        salt: salt.to_vec(),
        backends: backends.iter().map(|b| b.name().to_string()).collect(),
        pesos: pesos.clone(),
    };
    write(&backends, &header).await?;

    let key = MasterKey::derive(frase, &salt)?;
    Ok(ChunkStore::new(layout, backends, key)?.with_pesos(pesos))
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
/// Lee la cabecera, que va replicada en todos los proveedores.
///
/// Distingue **tres** situaciones que antes se confundian en una:
///
/// - La cabecera no esta en ningun sitio: no hay volumen. `Ok(None)`.
/// - Esta, y todos los que contestaron dicen lo mismo: `Ok(Some(_))`.
/// - **No se pudo preguntar**, y nadie contesto con una cabecera: eso no es "no hay
///   volumen", es "no se sabe". Antes se devolvia `Ok(None)` y el programa decia que no
///   habia ningun volumen, que ante un token caducado o un corte de red es una afirmacion
///   falsa y bastante alarmante: parece que los datos han desaparecido.
///
/// Si alguno falla pero otro si trae la cabecera, se sigue adelante: para eso va replicada.
pub async fn read(backends: &[Arc<dyn Backend>]) -> Result<Option<Header>, CoreError> {
    let lecturas = backends.iter().map(|b| async move {
        let r = match b.get(HEADER_KEY).await {
            Ok(bytes) => Ok(Some(bytes)),
            // Que no este es lo normal en un proveedor recien anadido.
            Err(BackendError::NotFound(_)) => Ok(None),
            Err(e) => Err(e.to_string()),
        };
        (b.name().to_string(), r)
    });

    let mut encontrada: Option<(String, Header)> = None;
    let mut fallos: Vec<String> = Vec::new();

    for (nombre, r) in join_all(lecturas).await {
        let bytes = match r {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(e) => {
                fallos.push(format!("{nombre}: {e}"));
                continue;
            }
        };
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

    if encontrada.is_none() && !fallos.is_empty() {
        return Err(CoreError::CabeceraIlegible(fallos.join("; ")));
    }
    Ok(encontrada.map(|(_, h)| h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::NoResponde;
    use libredisk_backend::OpendalBackend;

    /// Un proveedor que no se deja leer **no significa que no haya volumen**.
    ///
    /// Antes se devolvia lo mismo en los dos casos, y el programa decia «no hay ningun
    /// volumen en estos proveedores». Ante un token caducado o un corte de red, eso es una
    /// afirmacion falsa y de las que asustan: parece que los datos han desaparecido.
    #[tokio::test]
    async fn no_poder_leer_no_es_lo_mismo_que_no_haber() {
        let mudo: Vec<Arc<dyn Backend>> = vec![Arc::new(NoResponde(
            OpendalBackend::memory("caido").unwrap(),
        ))];
        let e = read(&mudo).await.unwrap_err().to_string();
        assert!(e.contains("no se sabe"), "{e}");
        assert!(e.contains("caido"), "{e}");
    }

    /// Y si uno falla pero otro trae la cabecera, se sigue adelante: para eso va replicada.
    #[tokio::test]
    async fn con_uno_caido_basta_que_otro_la_traiga() {
        let bueno: Arc<dyn Backend> = Arc::new(OpendalBackend::memory("bueno").unwrap());
        let h = Header {
            version: 1,
            layout: "espejo:2".into(),
            reparto: 0,
            salt: vec![1, 2, 3],
            backends: vec!["bueno".into(), "caido".into()],
            pesos: Vec::new(),
        };
        write(std::slice::from_ref(&bueno), &h).await.unwrap();

        let mezcla: Vec<Arc<dyn Backend>> = vec![
            bueno,
            Arc::new(NoResponde(OpendalBackend::memory("caido").unwrap())),
        ];
        assert_eq!(read(&mezcla).await.unwrap(), Some(h));
    }
    use crate::testutil::backends;

    fn cabecera(n: usize) -> Header {
        Header {
            version: HEADER_VERSION,
            layout: "rs:4,2".into(),
            reparto: 0,
            salt: vec![7u8; 16],
            backends: (0..n).map(|i| format!("proveedor-{i}")).collect(),
            pesos: Vec::new(),
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
