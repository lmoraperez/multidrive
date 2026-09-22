//! Cuanto cabe en el volumen, deducido de lo que declara cada proveedor.
//!
//! Un volumen no tiene tamano propio: lo tiene prestado de las cuentas sobre las que vive, y
//! la topologia decide cuanto de ese prestamo se aprovecha. Aqui se hace esa cuenta, que es
//! aritmetica pura, para poder probarla sin tocar la red.
//!
//! La regla es la misma para las cuatro topologias y cabe en una linea: **manda el proveedor
//! mas apretado**, y lo que se saca de el se multiplica por cuantos fragmentos hacen falta
//! para reconstruir un trozo.
//!
//! - `mirror:n` guarda el trozo entero en cada uno, asi que el factor es 1: el volumen es tan
//!   grande como el proveedor mas pequeno, por muchos que haya.
//! - `rs(k,m)` guarda en cada uno un fragmento de `1/k` del trozo, asi que el factor es `k`.
//! - `stripe(n)`, `1/n` en cada uno: factor `n`.
//! - `concat`, un solo proveedor: factor 1.
//!
//! Y en los cuatro casos ese factor es exactamente [`Layout::shards_needed`], que es la misma
//! funcion que usa el motor para leer. No es casualidad ni una coincidencia aprovechada: son
//! la misma cantidad mirada desde los dos lados.

use libredisk_backend::Cuota;
use libredisk_raid::Layout;

/// Lo que cabe en el volumen y lo que queda libre, si se puede saber.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Espacio {
    /// Capacidad del volumen, o `None` si ningun proveedor declara un limite.
    pub total: Option<u64>,
    /// Lo que todavia se puede escribir.
    pub libre: Option<u64>,
    /// Proveedores que no dijeron nada: los que no declaran cupo y los que no contestaron.
    ///
    /// Importa que se sepa quienes son, porque **la cifra de arriba se ha calculado sin
    /// ellos** y cualquiera de ellos podria ser el mas apretado de todos. Es decir: con esta
    /// lista no vacia, `libre` es un techo, no una medida.
    pub sin_respuesta: Vec<String>,
}

impl Espacio {
    /// `true` si todos los proveedores contestaron y declararon su cupo.
    pub fn es_medida(&self) -> bool {
        self.sin_respuesta.is_empty() && self.total.is_some()
    }
}

/// Combina lo que declara cada proveedor en el tamano del volumen.
///
/// `cuotas` viene en el orden de los proveedores; cada elemento es lo que dijo ese, o un
/// `Err` con el motivo si no se le pudo preguntar.
pub fn combinar(layout: Layout, nombres: &[String], cuotas: &[Result<Cuota, String>]) -> Espacio {
    let factor = layout.shards_needed() as u64;
    let (mut min_total, mut min_libre) = (None::<u64>, None::<u64>);
    let mut sin_respuesta = Vec::new();

    for (i, cuota) in cuotas.iter().enumerate() {
        let nombre = || nombres.get(i).cloned().unwrap_or_else(|| i.to_string());
        match cuota {
            // Un proveedor sin cupo que declarar no aprieta: un bucket S3 se factura por lo
            // que ocupa y no se llena. Se anota igualmente, porque sin el la cifra de arriba
            // no describe el volumen entero.
            Ok(c) => match (c.total, c.libre()) {
                (Some(t), Some(l)) => {
                    min_total = Some(min_total.map_or(t, |m: u64| m.min(t)));
                    min_libre = Some(min_libre.map_or(l, |m: u64| m.min(l)));
                }
                _ => sin_respuesta.push(nombre()),
            },
            Err(_) => sin_respuesta.push(nombre()),
        }
    }

    Espacio {
        // Saturar y no desbordar: `rs(8,2)` sobre ocho cuentas de varios terabytes se acerca
        // al limite del entero, y darle la vuelta al contador convertiria un volumen enorme
        // en uno diminuto.
        total: min_total.map(|t| t.saturating_mul(factor)),
        libre: min_libre.map(|l| l.saturating_mul(factor)),
        sin_respuesta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuota(total: u64, usado: u64) -> Result<Cuota, String> {
        Ok(Cuota {
            total: Some(total),
            usado: Some(usado),
        })
    }

    fn nombres(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("p{i}")).collect()
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn con_espejo_manda_el_proveedor_mas_pequeno() {
        // Cada uno guarda una copia entera, asi que por muchos que haya no cabe mas que en
        // el que menos tenga.
        let e = combinar(
            Layout::Mirror { n: 3 },
            &nombres(3),
            &[
                cuota(100 * GIB, 10 * GIB),
                cuota(20 * GIB, 5 * GIB),
                cuota(100 * GIB, 0),
            ],
        );
        assert_eq!(e.total, Some(20 * GIB));
        assert_eq!(e.libre, Some(15 * GIB));
        assert!(e.es_medida());
    }

    #[test]
    fn con_reed_solomon_el_volumen_es_k_veces_el_mas_apretado() {
        // `rs(4,2)`: cada proveedor guarda un cuarto de cada trozo, asi que 10 GiB libres en
        // el mas apretado son 40 GiB de volumen.
        let e = combinar(
            Layout::ReedSolomon { k: 4, m: 2 },
            &nombres(6),
            &[
                cuota(50 * GIB, 40 * GIB),
                cuota(50 * GIB, 0),
                cuota(50 * GIB, 0),
                cuota(50 * GIB, 0),
                cuota(50 * GIB, 0),
                cuota(50 * GIB, 0),
            ],
        );
        assert_eq!(e.libre, Some(40 * GIB));
    }

    #[test]
    fn sin_redundancia_se_suma_todo_lo_del_mas_apretado() {
        // RAID 0 con tres iguales: tres veces uno. Es la topologia que mas capacidad da, y
        // la cuenta lo refleja.
        let e = combinar(
            Layout::Stripe { n: 3 },
            &nombres(3),
            &[cuota(10 * GIB, 0), cuota(10 * GIB, 0), cuota(10 * GIB, 0)],
        );
        assert_eq!(e.total, Some(30 * GIB));
    }

    #[test]
    fn el_que_no_declara_cupo_no_aprieta_pero_queda_anotado() {
        // Un bucket S3 no se llena, asi que no puede ser el mas apretado. Pero mientras este
        // ahi sin declarar nada, la cifra no describe el volumen entero y hay que decirlo.
        let e = combinar(
            Layout::Mirror { n: 2 },
            &nombres(2),
            &[cuota(20 * GIB, 5 * GIB), Ok(Cuota::MUDA)],
        );
        assert_eq!(e.libre, Some(15 * GIB));
        assert_eq!(e.sin_respuesta, vec!["p1"]);
        assert!(
            !e.es_medida(),
            "con un proveedor mudo la cifra es un techo, no una medida"
        );
    }

    #[test]
    fn el_que_no_contesta_tambien_queda_anotado() {
        let e = combinar(
            Layout::Mirror { n: 2 },
            &nombres(2),
            &[cuota(20 * GIB, 5 * GIB), Err("se cayo la red".into())],
        );
        assert_eq!(e.sin_respuesta, vec!["p1"]);
        assert!(!e.es_medida());
    }

    #[test]
    fn si_no_contesta_nadie_no_se_sabe_y_no_es_cero() {
        // Cero se leeria como "el disco esta lleno", que es una afirmacion. Aqui no se sabe
        // nada, y quien reciba esto tiene que decir eso.
        let e = combinar(
            Layout::Mirror { n: 2 },
            &nombres(2),
            &[Err("a".into()), Err("b".into())],
        );
        assert_eq!(e.total, None);
        assert_eq!(e.libre, None);
        assert_eq!(e.sin_respuesta.len(), 2);
    }

    #[test]
    fn multiplicar_por_el_factor_no_le_da_la_vuelta_al_contador() {
        // Ocho cuentas al limite del entero con `rs(8,2)`. Sin saturar, el volumen mas grande
        // imaginable saldria como uno diminuto, que es el peor error posible en esta cifra:
        // un sistema de ficheros que dice tener sitio y no lo tiene.
        let enorme = cuota(u64::MAX, 0);
        let cuotas: Vec<_> = (0..10).map(|_| enorme.clone()).collect();
        let e = combinar(Layout::ReedSolomon { k: 8, m: 2 }, &nombres(10), &cuotas);
        assert_eq!(e.total, Some(u64::MAX));
        assert_eq!(e.libre, Some(u64::MAX));
    }
}
