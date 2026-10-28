//! Que topologias tienen sentido con los proveedores que hay, y cuanto daria cada una.
//!
//! # Por que esto vive en el nucleo
//!
//! Porque la lista **es** una decision del producto, no de una interfaz. Nacio dentro de la
//! ventana de escritorio, y cuando el telefono tuvo que ofrecer lo mismo habia dos caminos:
//! copiarla, o subirla aqui. Copiarla habria funcionado el primer dia y habria empezado a
//! mentir en el segundo, porque el dia que se afine un aviso --que `k = 1` es un espejo con
//! otro nombre, que `lineal` va antes que `union` con proveedores desiguales-- se afinaria en
//! un sitio y no en el otro, y las dos caras del mismo programa dirian cosas distintas sobre
//! lo mismo.

use std::sync::Arc;

use multidrive_backend_api::{Backend, Cuota};
use multidrive_raid::Layout;
use serde::Serialize;

/// Una topologia posible, con lo que hay que saber para elegirla.
#[derive(Debug, Clone, Serialize)]
pub struct Opcion {
    pub spec: String,
    pub tolera: usize,
    pub sobrecoste: f64,
    /// Fraccion del espacio contratado que queda utilizable.
    pub aprovechado: f64,
    /// No aguanta ninguna caida. Quien la pinte tiene que decirlo con todas las letras:
    /// elegir esto sin enterarse es perder el volumen entero el dia que caiga un proveedor.
    pub sin_red: bool,
    /// Coloca cada trozo entero en un proveedor, en vez de partirlo entre todos.
    ///
    /// Cambia lo que significa la cifra de espacio: en las demas es el mas apretado por un
    /// factor --un **techo**-- y aqui es la suma de lo que declara cada uno, que con alguno
    /// mudo se queda **corta**. Lo contrario.
    pub suma: bool,
    /// Lo que quedaria libre en el volumen, si se pudo preguntar.
    ///
    /// Es la cifra con la que de verdad se elige: «aprovechas el 50 %» no dice si te quedan
    /// tres gigas o trescientos.
    pub libre: Option<u64>,
    /// Proveedores que no declararon cupo. Con alguno, `libre` es un techo y no una medida.
    pub sin_medir: Vec<String>,
}

/// Las topologias que tienen sentido con `n` proveedores, de mas segura a mas aprovechada.
///
/// Sin medir: no toca la red. Para poner las cifras esta [`medir`].
pub fn opciones(n: usize) -> Vec<Opcion> {
    let describir = |l: Layout| Opcion {
        spec: l.to_string(),
        tolera: l.tolerated_failures(),
        sobrecoste: l.storage_overhead(),
        aprovechado: l.shards_needed() as f64 / l.shard_count() as f64,
        sin_red: l.tolerated_failures() == 0,
        suma: l.coloca_por_trozo(),
        libre: None,
        sin_medir: Vec::new(),
    };

    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    // Espejo: una copia entera en cada proveedor. Aguanta todo menos uno, y desperdicia
    // tanto como aguanta.
    if n > 1 {
        out.push(describir(Layout::Mirror { n }));
    }
    // Reed-Solomon, de mas segura a mas aprovechada.
    //
    // `k = 1` se queda fuera porque **es un espejo**, no algo parecido: con un solo fragmento
    // de datos, cada fragmento de paridad acaba siendo una copia entera. Mismo margen, mismo
    // sobrecoste y misma capacidad que `mirror`, pero calculado con aritmetica de cuerpo
    // finito y sin la propiedad util del espejo —que cualquier copia suelta es el dato—.
    // Ofrecer las dos es ofrecer la misma opcion dos veces con nombres distintos.
    for m in (1..n).rev() {
        let k = n - m;
        if k < 2 {
            continue;
        }
        out.push(describir(Layout::ReedSolomon { k, m }));
    }

    // Y al final del todo, las que no tienen redundancia ninguna. Van las ultimas porque la
    // lista se lee de mas segura a mas aprovechada, y porque nadie deberia toparselas antes
    // que las demas.
    if n > 1 {
        // `lineal` antes que `union` porque de las dos es la que mas da: con proveedores
        // desiguales suma lo que cada uno aporta, mientras que `union` se queda en n veces
        // el mas pequeno. Con proveedores iguales dan lo mismo y el orden da igual.
        out.push(describir(Layout::Lineal { n }));
        out.push(describir(Layout::Stripe { n }));
    } else {
        out.push(describir(Layout::Concat));
    }
    out
}

/// Las mismas opciones, con la cifra de espacio puesta.
///
/// El porcentaje de aprovechamiento no basta para decidir: «aprovechas el 50 %» no dice si te
/// quedan tres gigas o trescientos. Y con proveedores desiguales la cifra sorprende, que es
/// justo cuando hay que verla antes y no despues: manda el mas apretado, asi que dos cuentas
/// de 15 GiB y 3,4 GiB dan 3,4 en espejo y 6,8 en union, no 18,4.
///
/// Cuesta **una peticion por proveedor**, una sola vez.
///
/// Lo que no se pudo preguntar se queda sin cifra en vez de tumbar la lista: elegir topologia
/// sigue siendo posible sin saber cuanto queda, y negarse a ensenarla por no poder medir seria
/// peor que ensenarla sin la medida.
pub async fn medir(backends: &[Arc<dyn Backend>]) -> Vec<Opcion> {
    let mut out = opciones(backends.len());

    let cuotas: Vec<Result<Cuota, String>> = futures::future::join_all(
        backends
            .iter()
            .map(|b| async move { b.cuota().await.map_err(|e| e.to_string()) }),
    )
    .await;
    let nombres: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();

    for o in &mut out {
        let Ok(layout) = o.spec.parse::<Layout>() else {
            continue;
        };
        let e = crate::espacio::combinar(layout, &nombres, &cuotas);
        o.libre = e.libre;
        o.sin_medir = e.sin_respuesta;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sin proveedores no hay nada que ofrecer, y eso no es un fallo: es la pantalla antes de
    /// enlazar la primera cuenta.
    #[test]
    fn sin_proveedores_no_hay_opciones() {
        assert!(opciones(0).is_empty());
    }

    /// Dos opciones que aguantan lo mismo y cuestan lo mismo **son la misma opcion**, aunque
    /// se llamen distinto. Con tres proveedores pasaba: salian `mirror:3` y `rs:1,2`.
    ///
    /// Se comprueba por la propiedad y no por los nombres, para que cace tambien el duplicado
    /// que aparezca al anadir una topologia nueva.
    ///
    /// La comparacion es por **lo que el usuario lee al elegir**, no por la forma interna:
    /// cuantas caidas aguanta, cuanto ocupa de mas, y si coloca cada trozo entero en un
    /// proveedor. Esa tercera parte hizo falta al llegar `lineal`: mirando solo fragmentos
    /// necesarios y totales, `lineal:2` y `espejo:2` parecen la misma cosa --uno de dos-- y
    /// no lo son en absoluto, porque el espejo aguanta una caida y lineal ninguna.
    #[test]
    fn ninguna_opcion_se_repite_con_otro_nombre() {
        for n in 1..=8 {
            let mut vistas: Vec<(usize, u64, bool)> = Vec::new();
            for o in opciones(n) {
                let l: Layout = o.spec.parse().unwrap();
                let clave = (
                    l.tolerated_failures(),
                    l.storage_overhead().to_bits(),
                    l.coloca_por_trozo(),
                );
                assert!(
                    !vistas.contains(&clave),
                    "con {n} proveedores, '{}' promete lo mismo que otra de la lista",
                    o.spec
                );
                vistas.push(clave);
            }
        }
    }

    /// Las que no aguantan nada van **al final** y van marcadas, todas ellas.
    ///
    /// Son dos desde que existe `lineal`, y las dos tienen que llevar el aviso: lo que hace
    /// falta no es que haya una sino que ninguna se cuele entre las que si aguantan.
    #[test]
    fn las_que_no_aguantan_nada_van_al_final_y_van_marcadas() {
        for n in 2..=8 {
            let ops = opciones(n);
            let sin_red: Vec<usize> = ops
                .iter()
                .enumerate()
                .filter(|(_, o)| o.sin_red)
                .map(|(i, _)| i)
                .collect();

            assert!(!sin_red.is_empty(), "con {n} proveedores no hay ninguna");
            // Las ultimas posiciones de la lista, sin huecos: ninguna colada en medio.
            let primera = ops.len() - sin_red.len();
            assert_eq!(
                sin_red,
                (primera..ops.len()).collect::<Vec<_>>(),
                "con {n} proveedores hay una sin redundancia en medio de la lista"
            );
            for i in &sin_red {
                assert_eq!(ops[*i].tolera, 0);
                assert_eq!(
                    ops[*i].sobrecoste, 1.0,
                    "no ocupa de mas: es su razon de ser"
                );
            }
        }
    }

    /// Con dos proveedores: espejo, y las dos formas de no tener redundancia.
    ///
    /// No cabe la paridad: `rs:1,1` seria el mismo espejo con otro nombre.
    #[test]
    fn con_dos_proveedores_hay_tres_opciones() {
        let specs: Vec<String> = opciones(2).into_iter().map(|o| o.spec).collect();
        assert_eq!(specs, vec!["espejo:2", "lineal:2", "union:2"]);
    }

    /// Con tres entra ademas la paridad.
    #[test]
    fn con_tres_proveedores_entra_la_paridad() {
        let specs: Vec<String> = opciones(3).into_iter().map(|o| o.spec).collect();
        assert_eq!(specs, vec!["espejo:3", "rs:2,1", "lineal:3", "union:3"]);
    }

    /// Con uno solo no hay nada que repartir.
    #[test]
    fn con_un_solo_proveedor_solo_cabe_concat() {
        let specs: Vec<String> = opciones(1).into_iter().map(|o| o.spec).collect();
        assert_eq!(specs, vec!["concat"]);
    }

    /// La lista va de mas segura a mas aprovechada.
    ///
    /// **No estrictamente**: `lineal` y `union` aguantan las dos cero, asi que van seguidas.
    /// Lo que no puede pasar es que una que aguanta menos vaya antes que una que aguanta
    /// mas, porque el orden es lo que hace que el valor por defecto sea el prudente.
    #[test]
    fn la_lista_va_de_mas_segura_a_mas_aprovechada() {
        for n in 2..=8 {
            let ops = opciones(n);
            for par in ops.windows(2) {
                assert!(
                    par[0].tolera >= par[1].tolera,
                    "con {n}: '{}' va antes que '{}' y aguanta menos",
                    par[0].spec,
                    par[1].spec
                );
            }
        }
    }

    /// Y entre las dos sin redundancia, **`lineal` va primera**: de las dos es la que mas da.
    ///
    /// Con proveedores desiguales suma lo que cada uno aporta, mientras que `union` se queda
    /// en n veces el mas pequeno. Quien llegue hasta el final de la lista deberia encontrarse
    /// antes la mejor de las dos.
    #[test]
    fn entre_las_dos_sin_red_lineal_va_antes_que_union() {
        for n in 2..=8 {
            let specs: Vec<String> = opciones(n).into_iter().map(|o| o.spec).collect();
            let lineal = specs.iter().position(|s| s.starts_with("lineal")).unwrap();
            let union = specs.iter().position(|s| s.starts_with("union")).unwrap();
            assert!(lineal < union, "con {n} proveedores: {specs:?}");
        }
    }
}
