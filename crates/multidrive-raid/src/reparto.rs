//! A que proveedor le toca cada trozo, cuando el trozo no se parte.
//!
//! La topologia lineal pone **cada trozo entero** en un solo proveedor, en vez de cortarlo en
//! fragmentos iguales y repartirlos. Eso es lo que hace que capacidades desiguales se sumen:
//! una cuenta de 1 TB junto a otra de 5 GB aporta lo suyo en vez de quedarse esperando a que
//! la pequena se llene.
//!
//! # Por que se calcula y no se apunta
//!
//! La colocacion **se deriva del identificador del trozo**. No hay ninguna tabla que
//! consultar ni nada que guardar, y eso no es un ahorro: es lo que mantiene en pie las dos
//! propiedades de las que cuelga el resto del sistema.
//!
//! - `scrub` enumera una vez por proveedor y compara contra claves calculadas. Si la
//!   colocacion se decidiera sobre la marcha, habria que ir a buscar donde esta cada trozo.
//! - `rescue` reconstruye el arbol leyendo los proveedores, **sin ninguna base de datos
//!   local**. Una tabla de colocacion seria justo esa base de datos, y perderla seria perder
//!   el volumen.
//!
//! # Por que reparto por maximo y no un resto de division
//!
//! Lo facil seria `proveedor = hash(trozo) % n`. Funciona hasta el dia que se anade un
//! proveedor: con `n + 1` casi **todos** los trozos cambian de sitio, y ampliar el volumen
//! pasa a costar moverlo entero. Que anadir un proveedor sea barato es justamente el motivo
//! de que esta topologia interese.
//!
//! El reparto por maximo --a cada proveedor se le calcula una puntuacion a partir del trozo y
//! se gana la mas alta-- solo mueve los trozos que el recien llegado gana, que son los suyos
//! y ninguno mas. Con pesos, ademas, reparte en proporcion a lo que cada uno aporta.

/// Cuanto aporta cada proveedor. Cero significa que no recibe nada.
pub type Pesos = [u64];

/// A que proveedor le toca este trozo.
///
/// `id` es el identificador del trozo; lo unico que se le pide es que sea el mismo siempre
/// para el mismo contenido, que es lo que ya garantiza el direccionamiento por contenido.
///
/// Devuelve `None` si no hay ningun proveedor con peso: sin sitio en ninguna parte no hay
/// eleccion que hacer, y devolver el cero de turno seria escribir donde no cabe.
pub fn elegir(id: &str, pesos: &Pesos) -> Option<usize> {
    let mut mejor: Option<(usize, f64)> = None;
    for (i, &peso) in pesos.iter().enumerate() {
        if peso == 0 {
            continue;
        }
        let p = puntuacion(id, i, peso);
        if mejor.is_none_or(|(_, m)| p > m) {
            mejor = Some((i, p));
        }
    }
    mejor.map(|(i, _)| i)
}

/// La puntuacion de un proveedor para un trozo.
///
/// Es el reparto por maximo ponderado de la literatura: se convierte el hash en un numero
/// entre 0 y 1 y se calcula `peso / -ln(u)`. Esa forma concreta es la que hace que la
/// probabilidad de ganar sea **proporcional al peso**; con `peso * u` o con `u / peso` no lo
/// es, y un proveedor del doble de tamano no recibiria el doble.
fn puntuacion(id: &str, indice: usize, peso: u64) -> f64 {
    let h = mezclar(id, indice);
    // Se desplaza media unidad para que nunca salga exactamente 0: `ln(0)` es infinito, y ahi
    // la comparacion dejaria de tener sentido.
    let u = (h as f64 + 0.5) / (u64::MAX as f64 + 1.0);
    peso as f64 / -u.ln()
}

/// Un hash de 64 bits del par (trozo, proveedor).
///
/// No hace falta que sea criptografico: aqui no hay nada que ocultar ni que autenticar, solo
/// repartir. Lo que si hace falta es que mezcle bien, porque si dos indices contiguos dieran
/// numeros parecidos el reparto se torceria.
///
/// Es una mezcla de las de tipo FNV con un paso final de dispersion. Se escribe aqui en vez
/// de traer una caja: son diez lineas y no tiene ninguna decision que revisar.
fn mezclar(id: &str, indice: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    for b in (indice as u64).to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    // Dispersion final: sin esto, entradas parecidas dejan bits altos parecidos.
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Identificadores de trozo con la misma pinta que los de verdad.
    fn ids(n: usize) -> Vec<String> {
        (0..n as u64)
            .map(|i| format!("{:064x}", i.wrapping_mul(0x9e37_79b9_7f4a_7c15)))
            .collect()
    }

    #[test]
    fn el_mismo_trozo_siempre_cae_en_el_mismo_sitio() {
        // Es la propiedad de la que cuelga todo: sin ella habria que apuntar donde esta cada
        // trozo, y esa tabla seria una base de datos que perder.
        let pesos = [10, 20, 30];
        for id in ids(200) {
            assert_eq!(elegir(&id, &pesos), elegir(&id, &pesos));
        }
    }

    #[test]
    fn sin_ningun_peso_no_hay_eleccion() {
        // Devolver el cero de turno seria escribir donde no cabe.
        assert_eq!(elegir("x", &[0, 0]), None);
        assert_eq!(elegir("x", &[]), None);
    }

    #[test]
    fn un_proveedor_sin_peso_no_recibe_nada() {
        // Es como se saca de circulacion uno que esta lleno sin tener que retirarlo.
        let pesos = [0, 1, 0];
        for id in ids(300) {
            assert_eq!(elegir(&id, &pesos), Some(1));
        }
    }

    #[test]
    fn reparte_en_proporcion_al_peso() {
        // Lo que hace que una cuenta de 1 TB junto a una de 5 GB aporte lo suyo.
        let pesos = [1, 3, 6];
        let total = 60_000;
        let mut cuenta = [0usize; 3];
        for id in ids(total) {
            cuenta[elegir(&id, &pesos).unwrap()] += 1;
        }
        for (i, &peso) in pesos.iter().enumerate() {
            let esperado = total as f64 * peso as f64 / 10.0;
            let visto = cuenta[i] as f64;
            let desvio = (visto - esperado).abs() / esperado;
            assert!(
                desvio < 0.05,
                "proveedor {i}: esperado ~{esperado:.0}, visto {visto:.0} ({:.1}% de desvio)",
                desvio * 100.0
            );
        }
    }

    /// **La razon de ser de este reparto.** Anadir un proveedor tiene que mover solo lo suyo.
    ///
    /// Con un resto de division cambiarian de sitio casi todos los trozos, y ampliar el
    /// volumen costaria reescribirlo entero, que es justo lo que se intenta evitar.
    #[test]
    fn anadir_un_proveedor_solo_mueve_su_parte() {
        let antes = [10, 10, 10];
        let despues = [10, 10, 10, 10];
        let total = 40_000;

        let mut movidos = 0;
        let mut al_nuevo = 0;
        for id in ids(total) {
            let a = elegir(&id, &antes).unwrap();
            let d = elegir(&id, &despues).unwrap();
            if a != d {
                movidos += 1;
                assert_eq!(
                    d, 3,
                    "solo se puede mover al recien llegado, no entre viejos"
                );
                al_nuevo += 1;
            }
        }

        // Al nuevo le toca una cuarta parte, asi que se mueve ~el 25 % y nada mas.
        let fraccion = movidos as f64 / total as f64;
        assert!(
            (fraccion - 0.25).abs() < 0.02,
            "se movio el {:.1} %, se esperaba ~25 %",
            fraccion * 100.0
        );
        assert_eq!(movidos, al_nuevo);
    }

    /// Y quitar uno mueve solo lo que habia en el.
    #[test]
    fn quitar_un_proveedor_no_revuelve_a_los_demas() {
        let antes = [10, 10, 10, 10];
        // Peso cero es como se retira: deja de recibir sin cambiar los indices de los otros.
        let despues = [10, 10, 10, 0];

        for id in ids(20_000) {
            let a = elegir(&id, &antes).unwrap();
            let d = elegir(&id, &despues).unwrap();
            if a != 3 {
                assert_eq!(a, d, "lo que no estaba en el retirado no se mueve");
            }
        }
    }

    /// Subir el peso de uno no revuelve el volumen entero.
    ///
    /// Es el caso de quien amplia una cuenta: tiene que recibir mas, pero lo que ya estaba
    /// colocado en los demas deberia quedarse donde esta salvo lo que el gane.
    #[test]
    fn cambiar_un_peso_mueve_poco() {
        let antes = [10, 10, 10];
        let despues = [10, 10, 12];
        let total = 20_000;
        let movidos = ids(total)
            .iter()
            .filter(|id| elegir(id, &antes) != elegir(id, &despues))
            .count();
        let fraccion = movidos as f64 / total as f64;
        assert!(fraccion < 0.10, "se movio el {:.1} %", fraccion * 100.0);
    }
}
