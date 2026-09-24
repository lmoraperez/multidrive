//! Motor RAID de MultiDrive: reparte un trozo entre varios proveedores y lo reconstruye
//! cuando faltan algunos.
//!
//! Un *fragmento* (shard) es la parte que va a parar a un proveedor concreto. La posicion
//! del fragmento en el vector importa: el codigo Reed-Solomon necesita saber cual falta,
//! por eso las funciones de reensamblado reciben `Vec<Option<Vec<u8>>>` y no una lista
//! compacta de los que sobrevivieron.

pub mod reparto;

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
    /// Reparto puro entre `n` proveedores, **sin ninguna redundancia**: lo que en un disco se
    /// llama RAID 0.
    ///
    /// Da toda la capacidad sumada y reparte el ancho de banda, y a cambio **perder uno solo
    /// pierde el volumen entero**.
    ///
    /// No es Reed-Solomon con `m = 0` porque eso no existe: Reed-Solomon necesita al menos un
    /// fragmento de paridad. Es su propia topologia, y tenerla aparte tiene la ventaja de que
    /// «sin redundancia» se lee en el nombre en vez de esconderse en un cero.
    Stripe { n: usize },
    /// Cada trozo **entero** en un solo proveedor, elegido por [`crate::reparto`] a partir
    /// del propio identificador del trozo.
    ///
    /// Es la unica topologia en la que **las capacidades desiguales se suman**: una cuenta de
    /// 1 TB junto a otra de 5 GB aporta lo suyo, en vez de quedarse esperando a que la
    /// pequena se llene. Las demas cortan cada trozo en partes iguales, asi que manda el
    /// proveedor con menos sitio.
    ///
    /// No es `Stripe` con otro reparto: `Stripe` parte cada trozo entre todos --leer uno pide
    /// `n` peticiones y perder un proveedor rompe **todos** los trozos-- y esta pone cada
    /// trozo en uno solo, asi que leer es una peticion y perder un proveedor se lleva solo lo
    /// que vivia ahi, que ademas se puede decir cual es.
    ///
    /// **No anade ni una caida de margen.** Perder un proveedor pierde el volumen, igual que
    /// `Stripe`. Lo que mejora es el espacio, el coste de leer y el de ampliar.
    Lineal { n: usize },
}

impl Layout {
    pub fn validate(&self) -> Result<(), RaidError> {
        match *self {
            Layout::Concat => Ok(()),
            Layout::Mirror { n } if n >= 1 => Ok(()),
            // Con uno solo no es un reparto, es `concat` con otro nombre.
            Layout::Lineal { n } if (2..=256).contains(&n) => Ok(()),
            Layout::Lineal { n } => Err(RaidError::InvalidLayout(format!(
                "lineal necesita 2 <= n <= 256, se dio {n}"
            ))),
            Layout::Stripe { n } if (2..=256).contains(&n) => Ok(()),
            Layout::Stripe { n } => Err(RaidError::InvalidLayout(format!(
                "stripe necesita 2 <= n <= 256, se dio {n}"
            ))),
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
            Layout::Stripe { n } => n,
            Layout::Lineal { n } => n,
        }
    }

    /// Cuantos fragmentos hacen falta para poder reconstruir el trozo.
    pub fn shards_needed(&self) -> usize {
        match *self {
            Layout::Concat => 1,
            Layout::Mirror { .. } => 1,
            Layout::ReedSolomon { k, .. } => k,
            // Todos: sin paridad no hay nada que reconstruir.
            Layout::Stripe { n } => n,
            // Uno: el trozo esta entero en un solo proveedor.
            Layout::Lineal { .. } => 1,
        }
    }

    /// Cuantos proveedores pueden caerse sin perder datos.
    ///
    /// **No se deriva de los otros dos.** En `lineal`, «hace falta un fragmento de n» no
    /// significa que sobren `n - 1`: significa que cada trozo esta en uno concreto, y si ese
    /// se cae, ese trozo no esta en ninguna otra parte. La resta daria `n - 1` y seria la
    /// mentira mas cara que puede decir esta funcion.
    pub fn tolerated_failures(&self) -> usize {
        match *self {
            Layout::Concat => 0,
            Layout::Mirror { n } => n - 1,
            Layout::ReedSolomon { m, .. } => m,
            Layout::Stripe { .. } => 0,
            Layout::Lineal { .. } => 0,
        }
    }

    /// Multiplicador de espacio ocupado respecto al tamano real de los datos.
    pub fn storage_overhead(&self) -> f64 {
        match *self {
            // Cada trozo se guarda una sola vez: no hay sobrecoste.
            Layout::Concat | Layout::Stripe { .. } | Layout::Lineal { .. } => 1.0,
            Layout::Mirror { n } => n as f64,
            Layout::ReedSolomon { k, m } => (k + m) as f64 / k as f64,
        }
    }

    /// Que fraccion del espacio contratado queda utilizable.
    ///
    /// Vive aqui y no en la interfaz porque sale de las mismas reglas que aplica el motor, y
    /// dos sitios que lo calculen por su cuenta acaban discrepando.
    pub fn aprovechado(&self) -> f64 {
        1.0 / self.storage_overhead()
    }

    /// Si cada trozo va entero a un solo proveedor, elegido a partir de su identificador.
    ///
    /// Es lo que distingue a `lineal` de todo lo demas, y lo que obliga al almacen a decidir
    /// **a donde** va cada fragmento en vez de mandar el fragmento `i` al proveedor `i`.
    pub fn coloca_por_trozo(&self) -> bool {
        matches!(self, Layout::Lineal { .. })
    }
}

/// Como se escribe una topologia. **Es lo que se guarda en la cabecera del volumen**, asi
/// que cambiarlo no es cosmetica: un volumen escrito con un nombre tiene que seguir
/// abriendose cuando el nombre cambie.
///
/// Por eso los nombres viejos --`mirror` y `stripe`-- se siguen aceptando al leer aunque ya
/// no se escriban. Ver [`Layout::from_str`].
impl std::fmt::Display for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Layout::Concat => write!(f, "concat"),
            Layout::Mirror { n } => write!(f, "espejo:{n}"),
            Layout::ReedSolomon { k, m } => write!(f, "rs:{k},{m}"),
            Layout::Stripe { n } => write!(f, "union:{n}"),
            Layout::Lineal { n } => write!(f, "lineal:{n}"),
        }
    }
}

impl std::str::FromStr for Layout {
    type Err = RaidError;

    /// Acepta `concat`, `espejo:3`, `union:3` y `rs:4,2`.
    ///
    /// **Y tambien los nombres viejos**, `mirror` y `stripe`, que es lo que hace que un
    /// volumen creado antes del cambio de nombre siga abriendose: la topologia va escrita en
    /// su cabecera, y esa cabecera no se reescribe por cambiarle el nombre a una palabra.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let layout = match s.split_once(':') {
            None if s == "concat" => Layout::Concat,
            Some(("lineal", n)) => Layout::Lineal {
                n: n.parse()
                    .map_err(|_| RaidError::InvalidLayout(format!("n invalido: {n}")))?,
            },
            Some(("union" | "stripe", n)) => Layout::Stripe {
                n: n.parse()
                    .map_err(|_| RaidError::InvalidLayout(format!("n invalido: {n}")))?,
            },
            Some(("espejo" | "mirror", n)) => Layout::Mirror {
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

/// Parte `data` en `k` trozos iguales, rellenando el ultimo con ceros.
///
/// Todos del mismo tamano porque el codigo de borrado lo exige, y porque un `join` que tuviera
/// que adivinar donde acaba cada uno no podria distinguir un fragmento corto de uno truncado.
/// El relleno se descarta en [`join`], que para eso recibe la longitud original.
fn trocear(data: &[u8], k: usize) -> Vec<Vec<u8>> {
    let shard_len = data.len().div_ceil(k).max(1);
    (0..k)
        .map(|i| {
            let start = (i * shard_len).min(data.len());
            let end = ((i + 1) * shard_len).min(data.len());
            let mut shard = vec![0u8; shard_len];
            shard[..end - start].copy_from_slice(&data[start..end]);
            shard
        })
        .collect()
}

/// Reparte `data` en los fragmentos que exige la topologia.
pub fn split(layout: Layout, data: &[u8]) -> Result<Vec<Vec<u8>>, RaidError> {
    layout.validate()?;
    match layout {
        // Los dos ponen el trozo entero en un proveedor; la diferencia esta en **cual**, y
        // eso lo decide el almacen con `reparto::elegir`, no esta funcion.
        Layout::Concat | Layout::Lineal { .. } => Ok(vec![data.to_vec()]),
        Layout::Mirror { n } => Ok(vec![data.to_vec(); n]),
        // Los mismos fragmentos de datos que haria Reed-Solomon con `k = n`, y ninguno mas.
        Layout::Stripe { n } => Ok(trocear(data, n)),
        Layout::ReedSolomon { k, m } => {
            let mut shards = trocear(data, k);
            let shard_len = shards[0].len();
            shards.reserve(m);
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
        // Basta con el primero que haya llegado. En espejo porque todas las copias son
        // identicas; en concat y lineal porque solo hay una, y quien la pidio ya sabia a
        // que proveedor.
        Layout::Concat | Layout::Mirror { .. } | Layout::Lineal { .. } => {
            let mut data = shards
                .into_iter()
                .flatten()
                .next()
                .expect("ya comprobado que hay uno");
            data.truncate(original_len);
            Ok(data)
        }
        // Sin paridad no hay nada que reconstruir: o estan todos, o no hay trozo. Que hayan
        // llegado todos ya lo comprobo la cuenta de arriba, porque aqui `shards_needed` es
        // `n`.
        Layout::Stripe { .. } => {
            let mut data = Vec::with_capacity(original_len);
            for shard in shards {
                data.extend_from_slice(&shard.expect("con stripe hacen falta todos"));
            }
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

    /// Los nombres viejos se siguen leyendo, y eso **no es una comodidad**.
    ///
    /// La topologia va escrita en la cabecera del volumen, en los proveedores. Un volumen
    /// creado cuando se llamaba `mirror:2` tiene eso escrito, y nadie va a ir a reescribirlo
    /// por haberle cambiado el nombre a una palabra. Si esto dejara de parsear, ese volumen
    /// dejaria de abrirse y sus datos serian ilegibles.
    #[test]
    fn un_volumen_escrito_con_el_nombre_viejo_se_sigue_abriendo() {
        for (viejo, nuevo) in [("mirror:3", "espejo:3"), ("stripe:4", "union:4")] {
            let de_antes: Layout = viejo.parse().expect(viejo);
            let de_ahora: Layout = nuevo.parse().expect(nuevo);
            assert_eq!(de_antes, de_ahora, "{viejo} y {nuevo} son lo mismo");
        }
    }

    /// Lo que se escribe a partir de ahora son los nombres nuevos.
    #[test]
    fn se_escriben_los_nombres_nuevos() {
        assert_eq!(Layout::Mirror { n: 3 }.to_string(), "espejo:3");
        assert_eq!(Layout::Stripe { n: 4 }.to_string(), "union:4");
        assert_eq!(Layout::Concat.to_string(), "concat");
        assert_eq!(Layout::ReedSolomon { k: 4, m: 2 }.to_string(), "rs:4,2");
    }

    /// Y lo escrito se vuelve a leer igual, que es lo que sostiene la cabecera.
    #[test]
    fn lo_escrito_se_lee_igual() {
        for l in [
            Layout::Concat,
            Layout::Mirror { n: 2 },
            Layout::Stripe { n: 3 },
            Layout::ReedSolomon { k: 4, m: 2 },
        ] {
            assert_eq!(l.to_string().parse::<Layout>().unwrap(), l, "{l}");
        }
    }

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

    /// Reparto puro: va y vuelve entero mientras esten todos los fragmentos.
    #[test]
    fn stripe_reparte_y_reensambla() {
        let datos: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let l = Layout::Stripe { n: 3 };

        let fragmentos = split(l, &datos).unwrap();
        assert_eq!(fragmentos.len(), 3);
        // Sin paridad, lo repartido ocupa lo mismo que el original (salvo el relleno).
        assert!(fragmentos.iter().map(|f| f.len()).sum::<usize>() < datos.len() + 3);

        let vuelta = join(l, fragmentos.into_iter().map(Some).collect(), datos.len()).unwrap();
        assert_eq!(vuelta, datos);
    }

    /// Y la propiedad que lo define: **perder uno lo pierde todo**.
    ///
    /// Es lo que separa esta topologia de todas las demas, y por eso se comprueba en vez de
    /// darlo por supuesto: si algun dia dejara de cumplirse, seria porque se ha colado
    /// redundancia donde se prometio que no habia.
    #[test]
    fn stripe_no_sobrevive_a_ninguna_caida() {
        let datos: Vec<u8> = (0..5_000u32).map(|i| (i % 97) as u8).collect();
        let l = Layout::Stripe { n: 4 };
        assert_eq!(l.tolerated_failures(), 0);

        let fragmentos = split(l, &datos).unwrap();
        for perdido in 0..4 {
            let mutilados: Vec<Option<Vec<u8>>> = fragmentos
                .iter()
                .enumerate()
                .map(|(i, f)| if i == perdido { None } else { Some(f.clone()) })
                .collect();
            assert!(
                join(l, mutilados, datos.len()).is_err(),
                "sin el fragmento {perdido} no deberia poder reensamblarse"
            );
        }
    }

    /// No ocupa mas que los datos: es la razon de existir de esta topologia.
    #[test]
    fn stripe_no_tiene_sobrecoste() {
        assert_eq!(Layout::Stripe { n: 5 }.storage_overhead(), 1.0);
        assert_eq!(Layout::Stripe { n: 2 }.storage_overhead(), 1.0);
    }

    /// Con un solo proveedor no es un reparto: es `concat` con otro nombre.
    #[test]
    fn stripe_de_uno_no_es_una_topologia() {
        assert!(Layout::Stripe { n: 1 }.validate().is_err());
        assert!(Layout::Stripe { n: 0 }.validate().is_err());
        assert!("stripe:1".parse::<Layout>().is_err());
    }

    #[test]
    fn union_va_y_vuelve_por_su_nombre() {
        let l: Layout = "union:6".parse().unwrap();
        assert_eq!(l, Layout::Stripe { n: 6 });
        assert_eq!(l.to_string(), "union:6");
    }

    /// **`lineal` no aguanta ninguna caida**, y eso no se puede derivar de los otros numeros.
    ///
    /// «Hace falta 1 fragmento de n» invita a restar y decir que sobran `n - 1`. Es falso:
    /// cada trozo esta en un proveedor concreto, y si ese se cae, ese trozo no esta en
    /// ninguna otra parte. Seria la mentira mas cara que puede decir esta funcion, porque el
    /// usuario elige la topologia leyendo justo ese numero.
    #[test]
    fn lineal_no_aguanta_ninguna_caida_aunque_le_baste_un_fragmento() {
        let l = Layout::Lineal { n: 6 };
        assert_eq!(l.shards_needed(), 1);
        assert_eq!(l.shard_count(), 6);
        assert_eq!(
            l.tolerated_failures(),
            0,
            "restar aqui daria 5 y seria mentira"
        );
    }

    /// Y no desperdicia nada: cada trozo se guarda una sola vez.
    #[test]
    fn lineal_aprovecha_todo_el_espacio() {
        let l = Layout::Lineal { n: 4 };
        assert_eq!(l.storage_overhead(), 1.0);
        assert_eq!(l.aprovechado(), 1.0);
    }

    /// El espejo sigue costando lo que cuesta, que es lo que compra el margen.
    #[test]
    fn el_espejo_sigue_costando_n_veces() {
        let l = Layout::Mirror { n: 3 };
        assert_eq!(l.storage_overhead(), 3.0);
        assert_eq!(l.tolerated_failures(), 2);
    }

    #[test]
    fn lineal_va_y_vuelve_por_su_nombre() {
        let l: Layout = "lineal:5".parse().unwrap();
        assert_eq!(l, Layout::Lineal { n: 5 });
        assert_eq!(l.to_string(), "lineal:5");
    }

    /// Con uno solo no es un reparto: es `concat`.
    #[test]
    fn lineal_necesita_al_menos_dos() {
        assert!("lineal:1".parse::<Layout>().is_err());
        assert!("lineal:2".parse::<Layout>().is_ok());
    }

    /// El trozo va entero, sin cortar: un solo fragmento y con todos los bytes.
    #[test]
    fn lineal_no_corta_el_trozo() {
        let datos: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let fragmentos = split(Layout::Lineal { n: 4 }, &datos).unwrap();
        assert_eq!(fragmentos.len(), 1, "uno solo, no uno por proveedor");
        assert_eq!(fragmentos[0], datos);
    }

    /// Y se reconstruye con ese unico fragmento, aunque los demas huecos vengan vacios.
    ///
    /// El vector que recibe `join` es **posicional por proveedor**, igual que en el resto de
    /// topologias: en lineal solo uno trae algo y los demas vienen a `None`, porque ahi
    /// nunca hubo nada que traer. Es lo que permite que el resto del motor no tenga que
    /// saber que esta topologia es distinta.
    #[test]
    fn lineal_se_reconstruye_con_el_unico_que_hay() {
        let datos: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let fragmentos = split(Layout::Lineal { n: 4 }, &datos).unwrap();

        let mut recibidos = vec![None, None, None, None];
        recibidos[2] = Some(fragmentos[0].clone());
        assert_eq!(
            join(Layout::Lineal { n: 4 }, recibidos, datos.len()).unwrap(),
            datos
        );
    }

    /// Y si el que lo tenia no contesta, no hay trozo. No hay de donde sacarlo.
    #[test]
    fn lineal_sin_su_proveedor_no_hay_nada_que_reconstruir() {
        let vacios = vec![None, None, None, None];
        assert!(join(Layout::Lineal { n: 4 }, vacios, 1000).is_err());
    }

    #[test]
    fn stripe_trocea_igual_que_reed_solomon() {
        let datos: Vec<u8> = (0..7_777u32).map(|i| (i % 211) as u8).collect();
        let a = split(Layout::Stripe { n: 3 }, &datos).unwrap();
        let b = split(Layout::ReedSolomon { k: 3, m: 2 }, &datos).unwrap();
        assert_eq!(a, b[..3]);
    }
}
