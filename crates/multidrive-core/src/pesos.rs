//! Cuanto aporta cada proveedor, para las topologias que colocan cada trozo entero.
//!
//! Solo `lineal` los usa: son lo que hace que un volumen sobre una cuenta de 15 GB y otra de
//! 50 GB guarde 65 y no 30. Para las demas topologias esto devuelve vacio y nadie los mira.
//!
//! Vivian copiados en la CLI y en la interfaz grafica, que es la peor forma de tener una
//! regla: los dos calculaban el peso y solo uno de los dos lo escribia en la cabecera.

use std::sync::Arc;

use futures::future::join_all;
use multidrive_backend_api::Backend;
use multidrive_raid::Layout;

/// La unidad en la que se miden los pesos.
///
/// En gigabytes y no en bytes porque lo que importa es la proporcion, y numeros pequenos
/// hacen la cabecera legible a ojo.
const GIB: u64 = 1 << 30;

/// Pesos para un volumen que se crea ahora.
///
/// Salen de lo que cada proveedor declara **libre** en este momento, y se quedan fijos en la
/// cabecera. Eso es deliberado: si se recalcularan en cada arranque, un trozo escrito ayer se
/// buscaria hoy en otro proveedor y el volumen se desmontaria solo. Cuando las cuentas cambien
/// de tamano habra que mudar el reparto a proposito, no que cambie por su cuenta.
///
/// Al que no declare cupo se le da el peso del mas pequeno que si lo declare: dejarlo en cero
/// lo sacaria del volumen, y darle mucho le mandaria trozos que quiza no caben.
pub async fn pesos_para(layout: Layout, backends: &[Arc<dyn Backend>]) -> Vec<u64> {
    if !layout.coloca_por_trozo() {
        return Vec::new();
    }
    let libres = libres(backends).await;
    let minimo = libres.iter().flatten().min().copied().unwrap_or(GIB);
    libres.iter().map(|l| en_gib(*l, minimo)).collect()
}

/// Pesos al anadir proveedores a un volumen que ya existe.
///
/// **Los pesos de los que ya estaban no se tocan**, aunque su hueco libre haya cambiado desde
/// que se creo el volumen. No es pereza: cada peso que cambia mueve trozos de sitio, y mover
/// un trozo es descargarlo y volverlo a subir. Recalcularlos todos convertiria "he anadido una
/// cuenta" en "reescribo el volumen entero" sin que nadie lo haya pedido.
///
/// Asi, lo unico que se mueve es la fraccion que gana el recien llegado. Ver
/// [`crate::rebalance::fraccion_que_se_mueve`].
pub async fn pesos_ampliados(actuales: &[u64], nuevos: &[Arc<dyn Backend>]) -> Vec<u64> {
    let libres = libres(nuevos).await;

    // El respaldo para el que no declara cupo sale de la misma escala que los demas pesos,
    // incluidos los que ya estaban: son gigabytes en los dos casos.
    let minimo = libres
        .iter()
        .flatten()
        .map(|l| l / GIB)
        .chain(actuales.iter().copied())
        .filter(|p| *p > 0)
        .min()
        .unwrap_or(1);

    actuales
        .iter()
        .copied()
        .chain(libres.iter().map(|l| en_gib(*l, minimo * GIB)))
        .collect()
}

/// Lo que cada proveedor declara libre, o nada si no lo dice o no responde.
///
/// Publica porque medir una retirada necesita exactamente esto, y tener dos formas de
/// preguntar el hueco libre seria tener una que se queda vieja.
pub async fn libres_de(backends: &[Arc<dyn Backend>]) -> Vec<Option<u64>> {
    libres(backends).await
}

/// Lo que cada proveedor declara libre, o nada si no lo dice o no responde.
async fn libres(backends: &[Arc<dyn Backend>]) -> Vec<Option<u64>> {
    join_all(backends.iter().map(|b| b.cuota()))
        .await
        .iter()
        .map(|c| c.as_ref().ok().and_then(|c| c.libre()))
        .collect()
}

/// Un peso nunca es cero: cero es estar fuera del volumen, y eso no es lo que significa
/// "tiene menos de un giga libre".
fn en_gib(libre: Option<u64>, respaldo: u64) -> u64 {
    (libre.unwrap_or(respaldo) / GIB).max(1)
}

#[cfg(test)]
mod tests {
    use multidrive_backend::OpendalBackend;

    use super::*;
    use crate::testutil::{Declara, backends};

    fn declara(nombre: &str, libres_gib: u64) -> Arc<dyn Backend> {
        Arc::new(Declara {
            interior: OpendalBackend::memory(nombre).unwrap(),
            total: libres_gib * GIB,
            usado: 0,
        }) as Arc<dyn Backend>
    }

    #[tokio::test]
    async fn el_peso_son_los_gigas_que_quedan_libres() {
        let bs = vec![declara("chica", 15), declara("grande", 50)];
        assert_eq!(pesos_para(Layout::Lineal { n: 2 }, &bs).await, vec![15, 50]);
    }

    /// Las topologias que parten cada trozo entre todos no tienen pesos que valgan: darles uno
    /// seria guardar en la cabecera un numero que nadie mira.
    #[tokio::test]
    async fn las_demas_topologias_no_llevan_pesos() {
        let bs = vec![declara("chica", 15), declara("grande", 50)];
        assert!(pesos_para(Layout::Mirror { n: 2 }, &bs).await.is_empty());
        assert!(
            pesos_para(Layout::ReedSolomon { k: 1, m: 1 }, &bs)
                .await
                .is_empty()
        );
    }

    /// **Lo que hace barato anadir una cuenta**: los pesos de los que ya estaban no se tocan.
    ///
    /// Si se recalcularan, cada giga gastado desde que se creo el volumen cambiaria las
    /// proporciones y moveria trozos entre proveedores que no han cambiado de sitio. Anadir
    /// costaria entonces lo mismo que reescribir el volumen entero.
    #[tokio::test]
    async fn anadir_no_toca_los_pesos_de_los_que_ya_estaban() {
        let nuevo = vec![declara("la-nueva", 35)];
        assert_eq!(pesos_ampliados(&[15, 50], &nuevo).await, vec![15, 50, 35]);
    }

    /// Al que no dice cuanto le queda se le da el peso del mas pequeno que si lo dice: cero lo
    /// dejaria fuera del volumen, y uno grande le mandaria trozos que quiza no caben.
    #[tokio::test]
    async fn al_que_no_dice_su_cupo_se_le_da_el_del_mas_pequeno() {
        let mudo = backends(1);
        assert_eq!(pesos_ampliados(&[15, 50], &mudo).await, vec![15, 50, 15]);

        let mudos = backends(2);
        assert_eq!(
            pesos_para(Layout::Lineal { n: 2 }, &mudos).await,
            vec![1, 1],
            "sin nadie que diga nada, todos aportan lo mismo"
        );
    }

    /// Un peso nunca es cero. Cero es estar fuera del volumen, y eso no es lo que significa
    /// que a una cuenta le quede menos de un giga.
    #[tokio::test]
    async fn un_peso_nunca_es_cero() {
        let bs = vec![declara("llena", 0), declara("grande", 50)];
        assert_eq!(pesos_para(Layout::Lineal { n: 2 }, &bs).await, vec![1, 50]);

        let llena = vec![declara("llena", 0)];
        assert_eq!(pesos_ampliados(&[15, 50], &llena).await, vec![15, 50, 1]);
    }
}
