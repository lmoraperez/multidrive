//! Motor RAID de LibreDisk: reparte un trozo entre varios proveedores y lo reconstruye
//! cuando faltan algunos.
//!
//! Un *fragmento* (shard) es la parte que va a parar a un proveedor concreto. La posicion
//! del fragmento en el vector importa: el codigo Reed-Solomon necesita saber cual falta,
//! por eso las funciones de reensamblado reciben `Vec<Option<Vec<u8>>>` y no una lista
//! compacta de los que sobrevivieron.

use reed_solomon_erasure::ReedSolomon;
use reed_solomon_erasure::galois_8::Field;

#[derive(Debug, thiserror::Error)]
pub enum RaidError {
    #[error("topologia invalida: {0}")]
    InvalidLayout(String),
    #[error("se necesitan al menos {needed} fragmentos de {total} y solo hay {available}")]
    NotEnoughShards {
        needed: usize,
        available: usize,
        total: usize,
    },
    #[error("se esperaban {expected} fragmentos y llegaron {got}")]
    ShardCount { expected: usize, got: usize },
    #[error("los fragmentos tienen tamanos distintos")]
    UnevenShards,
    #[error("fallo la reconstruccion Reed-Solomon: {0}")]
    Backend(#[from] reed_solomon_erasure::Error),
}

/// Como se reparte cada trozo entre los proveedores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Sin redundancia: el trozo va entero a un solo proveedor.
    Concat,
    /// Copia completa en cada uno de los `n` proveedores.
    Mirror { n: usize },
    /// Reed-Solomon: `k` fragmentos de datos y `m` de paridad, repartidos en `k + m`
    /// proveedores. Sobrevive a la perdida de `m` cualesquiera.
    ReedSolomon { k: usize, m: usize },
}

impl Layout {
    pub fn validate(&self) -> Result<(), RaidError> {
        match *self {
            Layout::Concat => Ok(()),
            Layout::Mirror { n } if n >= 1 => Ok(()),
            Layout::Mirror { n } => Err(RaidError::InvalidLayout(format!(
                "mirror necesita n >= 1, se dio {n}"
            ))),
            Layout::ReedSolomon { k, m } if k >= 1 && m >= 1 && k + m <= 256 => Ok(()),
            Layout::ReedSolomon { k, m } => Err(RaidError::InvalidLayout(format!(
                "rs necesita k >= 1, m >= 1 y k + m <= 256, se dio k={k} m={m}"
            ))),
        }
    }

    /// Cuantos proveedores consume esta topologia.
    pub fn shard_count(&self) -> usize {
        match *self {
            Layout::Concat => 1,
            Layout::Mirror { n } => n,
            Layout::ReedSolomon { k, m } => k + m,
        }
    }

    /// Cuantos fragmentos hacen falta para poder reconstruir el trozo.
    pub fn shards_needed(&self) -> usize {
        match *self {
            Layout::Concat => 1,
            Layout::Mirror { .. } => 1,
            Layout::ReedSolomon { k, .. } => k,
        }
    }

    /// Cuantos proveedores pueden caerse sin perder datos.
    pub fn tolerated_failures(&self) -> usize {
        self.shard_count() - self.shards_needed()
    }

    /// Multiplicador de espacio ocupado respecto al tamano real de los datos.
    pub fn storage_overhead(&self) -> f64 {
        self.shard_count() as f64 / self.shards_needed() as f64
    }
}

impl std::fmt::Display for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Layout::Concat => write!(f, "concat"),
            Layout::Mirror { n } => write!(f, "mirror:{n}"),
            Layout::ReedSolomon { k, m } => write!(f, "rs:{k},{m}"),
        }
    }
}

impl std::str::FromStr for Layout {
    type Err = RaidError;

    /// Acepta `concat`, `mirror:3` y `rs:4,2`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let layout = match s.split_once(':') {
            None if s == "concat" => Layout::Concat,
            Some(("mirror", n)) => Layout::Mirror {
                n: n.parse()
                    .map_err(|_| RaidError::InvalidLayout(format!("n invalido: {n}")))?,
            },
            Some(("rs", km)) => {
                let (k, m) = km
                    .split_once(',')
                    .ok_or_else(|| RaidError::InvalidLayout("rs espera k,m".into()))?;
                Layout::ReedSolomon {
                    k: k.parse()
                        .map_err(|_| RaidError::InvalidLayout(format!("k invalido: {k}")))?,
                    m: m.parse()
                        .map_err(|_| RaidError::InvalidLayout(format!("m invalido: {m}")))?,
                }
            }
            _ => return Err(RaidError::InvalidLayout(format!("no reconocido: {s}"))),
        };
        layout.validate()?;
        Ok(layout)
    }
}

/// Reparte `data` en los fragmentos que exige la topologia.
///
/// En Reed-Solomon los datos se rellenan con ceros hasta un multiplo de `k`, porque el
/// codigo exige fragmentos del mismo tamano. El relleno se descarta en [`join`], que para
/// eso recibe la longitud original.
pub fn split(layout: Layout, data: &[u8]) -> Result<Vec<Vec<u8>>, RaidError> {
    layout.validate()?;
    match layout {
        Layout::Concat => Ok(vec![data.to_vec()]),
        Layout::Mirror { n } => Ok(vec![data.to_vec(); n]),
        Layout::ReedSolomon { k, m } => {
            let shard_len = data.len().div_ceil(k).max(1);

            let mut shards: Vec<Vec<u8>> = Vec::with_capacity(k + m);
            for i in 0..k {
                let start = (i * shard_len).min(data.len());
                let end = ((i + 1) * shard_len).min(data.len());
                let mut shard = vec![0u8; shard_len];
                shard[..end - start].copy_from_slice(&data[start..end]);
                shards.push(shard);
            }
            shards.extend(std::iter::repeat_n(vec![0u8; shard_len], m));

            ReedSolomon::<Field>::new(k, m)?.encode(&mut shards)?;
            Ok(shards)
        }
    }
}

/// Reensambla un trozo a partir de los fragmentos que hayan sobrevivido.
///
/// `shards` debe conservar las posiciones originales: `None` marca el fragmento perdido.
/// `original_len` es la longitud del dato antes de repartirlo, necesaria para quitar el
/// relleno de ceros.
pub fn join(
    layout: Layout,
    mut shards: Vec<Option<Vec<u8>>>,
    original_len: usize,
) -> Result<Vec<u8>, RaidError> {
    layout.validate()?;

    if shards.len() != layout.shard_count() {
        return Err(RaidError::ShardCount {
            expected: layout.shard_count(),
            got: shards.len(),
        });
    }
    let available = shards.iter().filter(|s| s.is_some()).count();
    if available < layout.shards_needed() {
        return Err(RaidError::NotEnoughShards {
            needed: layout.shards_needed(),
            available,
            total: shards.len(),
        });
    }

    match layout {
        // Basta con el primero que haya llegado: todas las copias son identicas.
        Layout::Concat | Layout::Mirror { .. } => {
            let mut data = shards
                .into_iter()
                .flatten()
                .next()
                .expect("ya comprobado que hay uno");
            data.truncate(original_len);
            Ok(data)
        }
        Layout::ReedSolomon { k, m } => {
            let shard_len = shards
                .iter()
                .flatten()
                .map(|s| s.len())
                .next()
                .expect("ya comprobado que hay uno");
            if shards.iter().flatten().any(|s| s.len() != shard_len) {
                return Err(RaidError::UnevenShards);
            }

            ReedSolomon::<Field>::new(k, m)?.reconstruct(&mut shards)?;

            let mut data = Vec::with_capacity(k * shard_len);
            for shard in shards.into_iter().take(k) {
                data.extend_from_slice(&shard.expect("reconstruct rellena todos los huecos"));
            }
            data.truncate(original_len);
            Ok(data)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datos(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    /// Quita los fragmentos indicados, simulando proveedores caidos.
    fn perder(shards: Vec<Vec<u8>>, caidos: &[usize]) -> Vec<Option<Vec<u8>>> {
        shards
            .into_iter()
            .enumerate()
            .map(|(i, s)| if caidos.contains(&i) { None } else { Some(s) })
            .collect()
    }

    #[test]
    fn parseo_de_topologias() {
        assert_eq!("concat".parse::<Layout>().unwrap(), Layout::Concat);
        assert_eq!(
            "mirror:3".parse::<Layout>().unwrap(),
            Layout::Mirror { n: 3 }
        );
        assert_eq!(
            "rs:4,2".parse::<Layout>().unwrap(),
            Layout::ReedSolomon { k: 4, m: 2 }
        );
        assert_eq!(Layout::ReedSolomon { k: 4, m: 2 }.to_string(), "rs:4,2");
        assert!("rs:0,2".parse::<Layout>().is_err());
        assert!("raid5".parse::<Layout>().is_err());
    }

    #[test]
    fn propiedades_declaradas() {
        let rs = Layout::ReedSolomon { k: 4, m: 2 };
        assert_eq!(rs.shard_count(), 6);
        assert_eq!(rs.shards_needed(), 4);
        assert_eq!(rs.tolerated_failures(), 2);
        assert_eq!(rs.storage_overhead(), 1.5);

        let mirror = Layout::Mirror { n: 3 };
        assert_eq!(mirror.tolerated_failures(), 2);
        assert_eq!(mirror.storage_overhead(), 3.0);
    }

    #[test]
    fn concat_ida_y_vuelta() {
        let d = datos(1000);
        let shards = split(Layout::Concat, &d).unwrap();
        assert_eq!(shards.len(), 1);
        assert_eq!(
            join(Layout::Concat, perder(shards, &[]), d.len()).unwrap(),
            d
        );
    }

    #[test]
    fn mirror_sobrevive_a_todas_las_copias_menos_una() {
        let d = datos(1000);
        let shards = split(Layout::Mirror { n: 3 }, &d).unwrap();
        assert_eq!(shards.len(), 3);
        let recuperado = join(Layout::Mirror { n: 3 }, perder(shards, &[0, 1]), d.len()).unwrap();
        assert_eq!(recuperado, d);
    }

    #[test]
    fn rs_ida_y_vuelta_sin_perdidas() {
        let layout = Layout::ReedSolomon { k: 4, m: 2 };
        let d = datos(10_000);
        let shards = split(layout, &d).unwrap();
        assert_eq!(shards.len(), 6);
        assert_eq!(join(layout, perder(shards, &[]), d.len()).unwrap(), d);
    }

    /// El criterio de exito de la v0.1: perder `m` proveedores no pierde datos.
    #[test]
    fn rs_sobrevive_a_m_caidas_en_cualquier_combinacion() {
        let layout = Layout::ReedSolomon { k: 4, m: 2 };
        let d = datos(10_000);
        let shards = split(layout, &d).unwrap();

        for a in 0..6 {
            for b in (a + 1)..6 {
                let recuperado = join(layout, perder(shards.clone(), &[a, b]), d.len())
                    .unwrap_or_else(|e| panic!("fallo perdiendo {a} y {b}: {e}"));
                assert_eq!(recuperado, d, "datos corruptos al perder {a} y {b}");
            }
        }
    }

    #[test]
    fn rs_falla_limpiamente_si_se_pierden_mas_de_m() {
        let layout = Layout::ReedSolomon { k: 4, m: 2 };
        let d = datos(10_000);
        let shards = split(layout, &d).unwrap();
        let err = join(layout, perder(shards, &[0, 1, 2]), d.len()).unwrap_err();
        assert!(matches!(
            err,
            RaidError::NotEnoughShards {
                needed: 4,
                available: 3,
                total: 6
            }
        ));
    }

    #[test]
    fn rs_con_longitudes_que_no_son_multiplo_de_k() {
        let layout = Layout::ReedSolomon { k: 4, m: 2 };
        // El relleno es donde se esconden los errores de off-by-one.
        for n in [1, 2, 3, 5, 7, 4095, 4096, 4097] {
            let d = datos(n);
            let shards = split(layout, &d).unwrap();
            let recuperado = join(layout, perder(shards, &[1, 4]), d.len()).unwrap();
            assert_eq!(recuperado, d, "fallo con longitud {n}");
        }
    }

    #[test]
    fn rs_con_dato_vacio() {
        let layout = Layout::ReedSolomon { k: 4, m: 2 };
        let shards = split(layout, &[]).unwrap();
        assert_eq!(
            join(layout, perder(shards, &[0]), 0).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn numero_de_fragmentos_incorrecto_se_detecta() {
        let layout = Layout::ReedSolomon { k: 4, m: 2 };
        let err = join(layout, vec![None; 5], 100).unwrap_err();
        assert!(matches!(
            err,
            RaidError::ShardCount {
                expected: 6,
                got: 5
            }
        ));
    }

    /// Anadir paridad **no cambia la que ya habia**, ni los fragmentos de datos.
    ///
    /// De esto depende que se pueda anadir un proveedor sin reescribir el volumen entero: si
    /// se cumple, pasar de `rs(k,m)` a `rs(k,m+1)` es calcular un fragmento nuevo y subirlo, y
    /// mientras tanto el volumen se sigue leyendo con la topologia vieja. Si no se cumpliera,
    /// habria que reescribir cada trozo y no habria forma de hacerlo sin una ventana en la que
    /// el volumen no se pueda leer.
    ///
    /// Es una propiedad de como `reed-solomon-erasure` construye su matriz, no algo que
    /// prometa su API. Por eso se comprueba aqui: si un dia cambia, esto falla antes de que
    /// corrompa el volumen de nadie.
    #[test]
    fn ampliar_la_paridad_no_toca_los_fragmentos_que_ya_estaban() {
        let datos: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();

        let antes = split(Layout::ReedSolomon { k: 4, m: 2 }, &datos).unwrap();
        let despues = split(Layout::ReedSolomon { k: 4, m: 3 }, &datos).unwrap();

        assert_eq!(antes.len(), 6);
        assert_eq!(despues.len(), 7);
        for i in 0..6 {
            assert_eq!(
                antes[i], despues[i],
                "el fragmento {i} cambio al ampliar la paridad"
            );
        }
    }

    /// Y lo mismo saltando varios de golpe, que es lo que pasa al anadir dos proveedores.
    #[test]
    fn ampliar_la_paridad_de_dos_en_dos_tampoco() {
        let datos: Vec<u8> = (0..7_777u32).map(|i| (i % 97) as u8).collect();

        let antes = split(Layout::ReedSolomon { k: 3, m: 1 }, &datos).unwrap();
        let despues = split(Layout::ReedSolomon { k: 3, m: 3 }, &datos).unwrap();

        for i in 0..antes.len() {
            assert_eq!(antes[i], despues[i], "el fragmento {i} cambio");
        }
    }

    /// Cambiar `k`, en cambio, **si** mueve todo: las fronteras del reparto se desplazan.
    ///
    /// Es la diferencia entre anadir un proveedor para tener mas seguridad --barato-- y
    /// anadirlo para tener mas sitio --reescribir el volumen entero--. Conviene que este
    /// escrito en un test y no solo en la documentacion.
    #[test]
    fn cambiar_k_si_mueve_todos_los_fragmentos() {
        let datos: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();

        let antes = split(Layout::ReedSolomon { k: 4, m: 2 }, &datos).unwrap();
        let despues = split(Layout::ReedSolomon { k: 5, m: 2 }, &datos).unwrap();

        assert_ne!(
            antes[0], despues[0],
            "con otro k, hasta el primer fragmento es distinto"
        );
    }
}
