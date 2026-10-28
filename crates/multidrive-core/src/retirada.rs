//! Medir una retirada de proveedor **antes** de empezarla.
//!
//! Retirar un proveedor de un volumen `lineal` no es la operacion simetrica de anadirlo. Al
//! anadir, lo que se mueve va a un sitio que acaba de llegar vacio, asi que siempre cabe. Al
//! retirar hay que meter todo lo que tenia el que se va en lo que les quede libre a los demas,
//! y eso **puede no caber**.
//!
//! Lo caro de descubrirlo a mitad no es el tiempo perdido: es que el volumen se queda con la
//! mitad de una mudanza hecha, y quien lo mire no sabe en que estado esta. Por eso esto se mide
//! entero primero y se dice, y solo despues se mueve algo.
//!
//! # Que se mide, exactamente
//!
//! El recorrido del arbol da los trozos que hay. Para cada uno, el sorteo ponderado dice quien
//! lo tiene hoy y quien lo tendra con los pesos nuevos; los que cambian de dueno son los que
//! hay que mover, y su tamano **sellado** es lo que van a ocupar. Sellado y no en claro porque
//! es lo que de verdad se sube.
//!
//! El hueco libre lo declara cada proveedor. El que no lo declare cuenta como desconocido y no
//! como infinito: decir «cabe» apoyandose en una cuenta que no ha contestado es justo la clase
//! de promesa que este programa no hace.

use std::collections::BTreeMap;
use std::sync::Arc;

use multidrive_backend_api::Backend;

use crate::{ChunkRef, ChunkStore, CoreError};

/// Lo que le toca recibir a un proveedor, y si le cabe.
#[derive(Debug, Clone)]
pub struct Reciben {
    pub nombre: String,
    /// Bytes sellados que se le van a mudar.
    pub bytes: u64,
    /// Lo que declara libre, o `None` si no lo dice o no contesta.
    pub libre: Option<u64>,
}

impl Reciben {
    /// `Some(false)` si no cabe, `Some(true)` si cabe, `None` si no se sabe.
    ///
    /// Los tres casos son distintos y se cuentan distinto. «No se sabe» no es «si».
    pub fn cabe(&self) -> Option<bool> {
        self.libre.map(|l| l >= self.bytes)
    }
}

/// Lo que costaria retirar un proveedor, medido.
#[derive(Debug, Clone, Default)]
pub struct Medida {
    /// Trozos distintos que cambian de dueno.
    pub trozos: usize,
    /// Bytes sellados que hay que mover.
    pub bytes: u64,
    /// Por cada proveedor que se queda, lo que recibe y lo que tiene libre.
    pub reciben: Vec<Reciben>,
}

impl Medida {
    /// Los que declaran que no les cabe.
    pub fn no_caben(&self) -> Vec<&Reciben> {
        self.reciben
            .iter()
            .filter(|r| r.cabe() == Some(false))
            .collect()
    }

    /// Los que no dicen cuanto tienen libre **y van a recibir algo**.
    ///
    /// Que uno no declare cupo solo importa si le toca recibir: si no recibe nada, que no se
    /// sepa su hueco no cambia nada de esta decision.
    pub fn sin_medir(&self) -> Vec<&Reciben> {
        self.reciben
            .iter()
            .filter(|r| r.libre.is_none() && r.bytes > 0)
            .collect()
    }

    /// Si se puede seguir sin reservas: todos los que reciben algo declaran que les cabe.
    pub fn cabe_seguro(&self) -> bool {
        self.no_caben().is_empty() && self.sin_medir().is_empty()
    }
}

impl ChunkStore {
    /// Mide que habria que mudar para retirar el proveedor de la posicion `sale`.
    ///
    /// `pesos_nuevos` son los de los que se quedan, en su orden. No toca nada: solo lee el cupo
    /// de cada proveedor y hace cuentas con los trozos que se le den.
    pub async fn medir_retirada(
        &self,
        sale: usize,
        pesos_nuevos: &[u64],
        quedan: &[Arc<dyn Backend>],
        trozos: &[ChunkRef],
    ) -> Result<Medida, CoreError> {
        let pesos_viejos = self.pesos_efectivos();
        if sale >= pesos_viejos.len() {
            return Err(CoreError::Rebalanceo(format!(
                "la posicion {sale} no existe: el volumen tiene {} proveedor(es)",
                pesos_viejos.len()
            )));
        }
        if pesos_nuevos.len() != quedan.len() {
            return Err(CoreError::Rebalanceo(format!(
                "hay {} proveedor(es) que se quedan y {} peso(s)",
                quedan.len(),
                pesos_nuevos.len()
            )));
        }

        // Un mismo trozo aparece muchas veces en el recorrido: la deduplicacion por contenido
        // hace que dos ficheros iguales lo compartan. Contarlo dos veces diria que hay que
        // mover el doble de lo que hay.
        let mut unicos: BTreeMap<&str, &ChunkRef> = BTreeMap::new();
        for c in trozos {
            unicos.entry(c.id.as_str()).or_insert(c);
        }

        let mut recibe = vec![0u64; quedan.len()];
        let mut cuantos = 0usize;
        let mut bytes = 0u64;

        for (id, c) in &unicos {
            // Quien lo tiene hoy. Si no le tocaba al que se va, no se mueve.
            if multidrive_raid::reparto::elegir(id, &pesos_viejos) != Some(sale) {
                continue;
            }
            let Some(destino) = multidrive_raid::reparto::elegir(id, pesos_nuevos) else {
                // Sin ningun peso util no hay a donde mudarlo. Es el caso de retirar el ultimo
                // que quedaba con sitio, y decirlo aqui es mejor que fallar al mover.
                return Err(CoreError::SinSitio);
            };
            cuantos += 1;
            bytes += c.sealed_len as u64;
            recibe[destino] += c.sealed_len as u64;
        }

        let libres = crate::pesos::libres_de(quedan).await;
        Ok(Medida {
            trozos: cuantos,
            bytes,
            reciben: quedan
                .iter()
                .enumerate()
                .map(|(i, b)| Reciben {
                    nombre: b.name().to_string(),
                    bytes: recibe[i],
                    libre: libres[i],
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use multidrive_raid::Layout;

    use super::*;
    use crate::testutil::{backends, clave};
    use crate::tree::Volume;

    /// Un volumen lineal con algo escrito, y sus trozos.
    async fn con_contenido(n: usize) -> (Volume, Vec<ChunkRef>, Vec<Arc<dyn Backend>>) {
        let bs = backends(n);
        let store = ChunkStore::new(Layout::Lineal { n }, bs.clone(), clave())
            .unwrap()
            .with_pesos(vec![1; n]);
        let (mut v, _) = Volume::create(store).await.unwrap();
        for i in 0..24 {
            v.write_file(&format!("/f{i}.bin"), &vec![i as u8; 4096][..])
                .await
                .unwrap();
        }
        let (trozos, _) = v.walk().await;
        (v, trozos, bs)
    }

    /// Solo se mueve lo que tenia el que se va.
    ///
    /// Es la propiedad entera del sorteo ponderado, y lo que separa «retirar una cuenta» de
    /// «reescribir el volumen». Si esto dejara de cumplirse, retirar seguiria funcionando pero
    /// costaria el volumen entero, y nadie se enteraria hasta tener uno grande.
    #[tokio::test]
    async fn solo_se_mueve_lo_del_que_se_va() {
        let (v, trozos, bs) = con_contenido(4).await;
        let quedan: Vec<Arc<dyn Backend>> = bs[..3].to_vec();

        let m = v
            .store()
            .medir_retirada(3, &[1, 1, 1], &quedan, &trozos)
            .await
            .unwrap();

        // Lo que se mueve es lo que tenia el cuarto, no todo.
        let total: u64 = trozos.iter().map(|c| c.sealed_len as u64).sum();
        assert!(m.trozos > 0, "algo tenia el que se va");
        assert!(
            m.bytes < total,
            "se mueve {} de {total}: eso es el volumen entero",
            m.bytes
        );
        // Y lo que reciben los que se quedan suma exactamente lo que se mueve.
        let repartido: u64 = m.reciben.iter().map(|r| r.bytes).sum();
        assert_eq!(repartido, m.bytes);
    }

    /// El recorrido entero: se retira el ultimo y **todo se sigue leyendo**.
    ///
    /// Es la unica prueba que de verdad dice que esto funciona. Las demas comprueban cuentas;
    /// esta comprueba que despues de mudar y publicar la cabecera nueva, cada fichero que habia
    /// sigue estando y con el mismo contenido. Si algo se buscara donde no esta, aqui saldria.
    #[tokio::test]
    async fn tras_retirar_el_ultimo_se_lee_todo_igual() {
        let bs = backends(3);
        let store = ChunkStore::new(Layout::Lineal { n: 3 }, bs.clone(), clave())
            .unwrap()
            .with_pesos(vec![1, 1, 1]);
        let (mut v, _) = Volume::create(store).await.unwrap();

        let mut escrito = Vec::new();
        for i in 0..24u8 {
            let ruta = format!("/f{i}.bin");
            let datos = vec![i; 3000 + i as usize];
            v.write_file(&ruta, &datos[..]).await.unwrap();
            escrito.push((ruta, datos));
        }
        let (trozos, _) = v.walk().await;
        let raiz = v.root().clone();

        let quedan: Vec<Arc<dyn Backend>> = bs[..2].to_vec();
        let pesos_nuevos = vec![1, 1];

        let medida = v
            .store()
            .medir_retirada(2, &pesos_nuevos, &quedan, &trozos)
            .await
            .unwrap();
        assert!(medida.trozos > 0, "el tercero tenia algo");

        let informe = v
            .store()
            .mudar_reparto(Layout::Lineal { n: 2 }, &pesos_nuevos, &quedan, &trozos)
            .await
            .unwrap();
        assert!(informe.completo(), "{:?}", informe.fallidos);
        assert_eq!(
            informe.mudados, medida.trozos,
            "se mudo justo lo que se habia medido"
        );

        // El volumen con los dos que quedan, como lo abriria la cabecera nueva.
        let despues = ChunkStore::new(Layout::Lineal { n: 2 }, quedan, clave())
            .unwrap()
            .with_pesos(pesos_nuevos);
        let v2 = Volume::at(despues, raiz);

        for (ruta, datos) in &escrito {
            let leido = v2.read_at(ruta, 0, datos.len() + 10).await.unwrap();
            assert_eq!(&leido, datos, "cambio el contenido de {ruta}");
        }
    }

    /// Los proveedores de memoria no declaran cupo, y eso **no** cuenta como que cabe.
    ///
    /// Decir «cabe» apoyandose en una cuenta que no ha contestado es la clase de promesa que
    /// este programa no hace: quien recibe algo y no dice cuanto tiene libre sale en la lista
    /// de los que no se pueden medir.
    #[tokio::test]
    async fn sin_cupo_declarado_no_es_que_quepa() {
        let (v, trozos, bs) = con_contenido(3).await;
        let quedan: Vec<Arc<dyn Backend>> = bs[..2].to_vec();

        let m = v
            .store()
            .medir_retirada(2, &[1, 1], &quedan, &trozos)
            .await
            .unwrap();

        assert!(m.no_caben().is_empty(), "nadie dice que no le quepa");
        assert!(
            !m.sin_medir().is_empty(),
            "pero hay quien recibe sin declarar hueco"
        );
        assert!(
            !m.cabe_seguro(),
            "asi que no se puede decir que quepa seguro"
        );
    }

    /// Quedarse sin nadie a quien mudar se dice, en vez de fallar a mitad.
    #[tokio::test]
    async fn sin_sitio_a_donde_mudar_se_dice_antes() {
        let (v, trozos, bs) = con_contenido(2).await;
        let quedan: Vec<Arc<dyn Backend>> = bs[..1].to_vec();

        // Un peso de cero saca a ese proveedor del sorteo: no queda nadie.
        let r = v.store().medir_retirada(1, &[0], &quedan, &trozos).await;
        assert!(matches!(r, Err(CoreError::SinSitio)), "{r:?}");
    }

    /// Una posicion que no existe es un error de uso, y se dice cual es.
    #[tokio::test]
    async fn una_posicion_que_no_existe_se_dice() {
        let (v, trozos, bs) = con_contenido(2).await;
        let quedan: Vec<Arc<dyn Backend>> = bs[..1].to_vec();
        let e = v
            .store()
            .medir_retirada(9, &[1], &quedan, &trozos)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("no existe"), "{e}");
    }
}
