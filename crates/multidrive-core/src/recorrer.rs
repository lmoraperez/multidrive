//! Recorrer el arbol entero: buscar por nombre y sacar lo mas reciente.
//!
//! # Lo que cuesta, que no es poco
//!
//! El arbol no tiene indice. Recorrerlo es bajar un nodo por directorio, y cada nodo es una
//! peticion **por proveedor**. Un volumen con doscientas carpetas son doscientas bajadas antes
//! de poder contestar.
//!
//! Nextcloud busca en su servidor y le sale gratis; aqui no hay servidor y lo que hay esta
//! cifrado, asi que el precio es este. Se paga de dos maneras:
//!
//! - **Un tope de nodos visitados.** Mejor contestar con lo que se encontro y decir que puede
//!   faltar, que tardar dos minutos en dar una lista completa que ya no interesa.
//! - **La cache del almacen.** Un recorrido deja los nodos calientes, asi que el segundo sale
//!   mucho mas barato que el primero.
//!
//! El dia que esto se quede corto, lo que toca es un indice local, no subir el tope.

use crate::tree::{Entry, Kind, Volume};

/// Cuantos directorios se bajan como mucho en un recorrido.
///
/// Doscientos es lo que cabe en una espera que alguien aguanta mirando la pantalla. No es una
/// cifra medida contra nubes reales: es un tope para que lo peor que pase sea una respuesta
/// incompleta, en vez de una pantalla colgada.
const TOPE: usize = 200;

/// Un hallazgo, con su ruta entera: la interfaz necesita poder ir a el.
pub struct Hallazgo {
    pub ruta: String,
    pub nombre: String,
    pub dir: bool,
    pub bytes: u64,
    pub modificado: Option<u64>,
}

impl Hallazgo {
    fn de(ruta: String, nombre: String, e: &Entry) -> Self {
        Hallazgo {
            ruta,
            nombre,
            dir: matches!(e.kind, Kind::Dir),
            bytes: e.len,
            modificado: e.modificado,
        }
    }
}

/// Lo que se encontro, y si hubo que parar antes de terminar.
pub struct Recorrido {
    pub hallazgos: Vec<Hallazgo>,
    /// `true` si se llego al tope. Quien lo pinte **tiene que decirlo**: una lista incompleta
    /// que se presenta como completa hace creer que un fichero no esta cuando si esta.
    pub incompleto: bool,
}

/// Recorre el arbol en anchura y se queda con lo que diga `quedarse`.
///
/// En anchura y no en profundidad a proposito: lo que la gente busca suele estar arriba, y si
/// hay que parar por el tope, haberlo gastado en los primeros niveles da mejores respuestas que
/// haberlo gastado bajando por una sola rama.
async fn recorrer(
    v: &Volume,
    mut quedarse: impl FnMut(&Hallazgo) -> bool,
) -> Result<Recorrido, String> {
    let mut hallazgos = Vec::new();
    let mut pendientes = vec![String::from("/")];
    let mut visitados = 0usize;
    let mut incompleto = false;

    while let Some(dir) = pendientes.first().cloned() {
        pendientes.remove(0);
        if visitados >= TOPE {
            incompleto = true;
            break;
        }
        visitados += 1;

        let hijos = match v.read_dir(&dir).await {
            Ok(h) => h,
            // Un directorio que no se puede leer no para el recorrido: se anota como incompleto
            // y se sigue. Lo contrario seria que un fallo suelto vaciara la busqueda entera.
            Err(_) => {
                incompleto = true;
                continue;
            }
        };

        for (nombre, e) in hijos {
            let ruta = if dir == "/" {
                format!("/{nombre}")
            } else {
                format!("{dir}/{nombre}")
            };
            let h = Hallazgo::de(ruta.clone(), nombre, &e);
            if matches!(e.kind, Kind::Dir) {
                pendientes.push(ruta);
            }
            if quedarse(&h) {
                hallazgos.push(h);
            }
        }
    }

    Ok(Recorrido {
        hallazgos,
        incompleto,
    })
}

/// Busca por nombre, sin distinguir mayusculas ni acentos de mas: solo minusculas y `contains`.
///
/// No es una busqueda de verdad --no parte palabras ni ordena por relevancia-- y no pretende
/// serlo. Es la que se puede hacer sin indice, y es la que la gente espera al escribir cuatro
/// letras de un nombre de fichero.
pub async fn buscar(v: &Volume, texto: &str) -> Result<Recorrido, String> {
    let aguja = texto.trim().to_lowercase();
    if aguja.is_empty() {
        return Ok(Recorrido {
            hallazgos: Vec::new(),
            incompleto: false,
        });
    }
    recorrer(v, |h| h.nombre.to_lowercase().contains(&aguja)).await
}

/// Lo ultimo que se escribio, de lo mas nuevo a lo mas viejo.
///
/// Solo ficheros: un directorio cambia de fecha cada vez que cambia algo dentro, asi que en una
/// lista de «recientes» saldrian todos los directorios del camino por encima de cada fichero, y
/// eso no es lo que nadie quiere ver.
///
/// Lo que no tiene fecha se queda fuera en vez de ir al final: es de antes de que el arbol las
/// guardara, y no se sabe si es viejo o nuevo. Colocarlo seria afirmar algo que no se sabe.
pub async fn recientes(v: &Volume, cuantos: usize) -> Result<Recorrido, String> {
    let mut r = recorrer(v, |h| !h.dir && h.modificado.is_some()).await?;
    r.hallazgos.sort_by_key(|h| std::cmp::Reverse(h.modificado));
    r.hallazgos.truncate(cuantos);
    Ok(r)
}

#[cfg(test)]
mod tests {
    use multidrive_raid::Layout;

    use super::*;
    use crate::testutil::{backends, store_sobre};

    async fn volumen() -> Volume {
        let bs = backends(6);
        Volume::create(store_sobre(&bs, Layout::ReedSolomon { k: 4, m: 2 }))
            .await
            .unwrap()
            .0
    }

    /// Busca por un trozo del nombre, en cualquier profundidad, sin distinguir mayusculas.
    #[tokio::test]
    async fn encuentra_por_un_trozo_del_nombre() {
        let mut v = volumen().await;
        v.mkdir_p("/fotos/2026").await.unwrap();
        v.write_file("/fotos/2026/Playa.jpg", &b"x"[..])
            .await
            .unwrap();
        v.write_file("/notas.txt", &b"y"[..]).await.unwrap();

        let r = buscar(&v, "play").await.unwrap();
        let rutas: Vec<&str> = r.hallazgos.iter().map(|h| h.ruta.as_str()).collect();
        assert_eq!(rutas, vec!["/fotos/2026/Playa.jpg"]);
        assert!(!r.incompleto);
    }

    /// Una busqueda vacia no recorre nada. Sin esto, borrar el texto del buscador lanzaria un
    /// recorrido entero del volumen para no enseñar nada.
    #[tokio::test]
    async fn buscar_nada_no_recorre_nada() {
        let v = volumen().await;
        let r = buscar(&v, "   ").await.unwrap();
        assert!(r.hallazgos.is_empty());
        assert!(!r.incompleto);
    }

    /// Recientes son ficheros, no directorios, y van de lo mas nuevo a lo mas viejo.
    #[tokio::test]
    async fn los_recientes_son_ficheros_y_van_del_mas_nuevo_al_mas_viejo() {
        let mut v = volumen().await;
        v.mkdir_p("/carpeta").await.unwrap();
        v.write_file("/primero.txt", &b"1"[..]).await.unwrap();
        v.write_file("/carpeta/segundo.txt", &b"2"[..])
            .await
            .unwrap();

        let r = recientes(&v, 10).await.unwrap();
        assert!(
            r.hallazgos.iter().all(|h| !h.dir),
            "se colo un directorio: cambian de fecha cada vez que cambia algo dentro"
        );
        let fechas: Vec<u64> = r.hallazgos.iter().filter_map(|h| h.modificado).collect();
        assert!(
            fechas.windows(2).all(|p| p[0] >= p[1]),
            "no van del mas nuevo al mas viejo: {fechas:?}"
        );
    }

    /// El tope se dice. Una lista incompleta presentada como completa hace creer que un fichero
    /// no esta cuando si esta, que es peor que decir «puede faltar algo».
    #[tokio::test]
    async fn pasado_el_tope_se_dice_que_esta_incompleto() {
        let mut v = volumen().await;
        for i in 0..(TOPE + 5) {
            v.mkdir_p(&format!("/d{i}")).await.unwrap();
        }
        let r = buscar(&v, "no-esta-esto").await.unwrap();
        assert!(r.incompleto, "se recorrio de mas sin avisar");
    }
}
