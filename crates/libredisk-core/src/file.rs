//! Capa de fichero sobre el almacen de trozos.
//!
//! [`ChunkStore`] opera sobre un trozo suelto. Un volumen necesita ficheros, y un fichero es
//! una lista ordenada de trozos mas su longitud: eso es el [`Manifest`].
//!
//! El troceado es de **tamano fijo**. Es lo que permite que leer un rango pequeno baje solo
//! los trozos que ese rango toca, que es el requisito sin el cual un montaje no es viable:
//! nadie acepta que leer 4 KiB de un fichero de 1 GiB descargue 1 GiB. El precio de esa
//! decision esta dicho donde toca, en [`Manifest`].

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{ChunkRef, ChunkStore, CoreError};

/// Tamano de trozo por defecto.
///
/// 16 MiB es el compromiso que razona `docs/ARQUITECTURA.md`: bastante grande para no agotar
/// las cuotas de peticiones de los proveedores, bastante pequeno para que leer 1 MiB no
/// obligue a bajar 500.
pub const CHUNK_SIZE: usize = 16 * 1024 * 1024;

/// Todo lo necesario para volver a montar un fichero.
///
/// **El troceado es de tamano fijo, no definido por contenido.** La consecuencia hay que
/// asumirla: insertar un byte al principio desplaza todos los limites y reescribe el fichero
/// entero. El troceado por contenido (CDC) lo evitaria, a cambio de trozos de tamano
/// variable, y con esos se pierde poder localizar un offset con una division. Para un volumen
/// que se monta, poder localizar pesa mas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Los trozos, en orden. Un fichero vacio no tiene ninguno.
    pub chunks: Vec<ChunkRef>,
    /// Longitud total del contenido en claro.
    pub len: u64,
    /// Tamano de trozo con el que se escribio **este** fichero.
    ///
    /// Se guarda en vez de darse por supuesto, por dos motivos. La lectura por rango lo
    /// necesita para saber en que trozo cae un offset, y el valor por defecto puede cambiar
    /// entre versiones: un manifiesto escrito hoy tiene que seguir leyendose cuando cambie.
    pub chunk_size: usize,
}

impl Manifest {
    /// Manifiesto de un fichero vacio.
    pub fn empty(chunk_size: usize) -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
            chunk_size,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Comprueba que el manifiesto es coherente consigo mismo.
    ///
    /// La lectura por rango cuenta con que **todos los trozos menos el ultimo miden
    /// `chunk_size`**. Si eso no se cumple, el calculo del offset apunta al trozo equivocado
    /// y se devuelven datos mal en silencio, que es la peor forma posible de fallar. Mas vale
    /// negarse a leer.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.chunk_size == 0 {
            return Err(CoreError::ZeroChunkSize);
        }

        let suma: u64 = self.chunks.iter().map(|c| c.plain_len as u64).sum();
        if suma != self.len {
            return Err(CoreError::InvalidManifest(format!(
                "declara {} bytes pero sus {} trozos suman {suma}",
                self.len,
                self.chunks.len()
            )));
        }

        if let Some((ultimo, previos)) = self.chunks.split_last() {
            if let Some(i) = previos.iter().position(|c| c.plain_len != self.chunk_size) {
                return Err(CoreError::InvalidManifest(format!(
                    "el trozo {i} mide {} y solo el ultimo puede medir menos de {}",
                    previos[i].plain_len, self.chunk_size
                )));
            }
            if ultimo.plain_len == 0 || ultimo.plain_len > self.chunk_size {
                return Err(CoreError::InvalidManifest(format!(
                    "el ultimo trozo mide {}, fuera de 1..={}",
                    ultimo.plain_len, self.chunk_size
                )));
            }
        }

        Ok(())
    }

    /// Indice del trozo donde cae `offset`, y la posicion dentro de ese trozo.
    fn locate(&self, offset: u64) -> (usize, usize) {
        let tam = self.chunk_size as u64;
        ((offset / tam) as usize, (offset % tam) as usize)
    }
}

/// Resultado de escribir un fichero.
#[derive(Debug, Clone)]
pub struct FileReport {
    pub manifest: Manifest,
    /// Proveedores que fallaron en **algun** trozo, sin repetir.
    ///
    /// Igual que [`crate::PutReport::degraded`]: el fichero quedo escrito y se puede leer,
    /// pero al menos un trozo tiene menos redundancia de la pedida y hay que repararlo.
    pub degraded: Vec<String>,
}

impl FileReport {
    pub fn is_fully_redundant(&self) -> bool {
        self.degraded.is_empty()
    }
}

impl ChunkStore {
    /// Trocea lo que produzca `reader`, lo sube y devuelve el manifiesto.
    ///
    /// Toma un lector y no un `&[u8]` a proposito: un fichero no cabe necesariamente en
    /// memoria, y el objetivo de todo esto es montar un volumen. Solo se retiene un trozo
    /// cada vez.
    pub async fn put_file<R>(&self, reader: R) -> Result<FileReport, CoreError>
    where
        R: AsyncRead + Unpin,
    {
        self.put_file_with(reader, CHUNK_SIZE).await
    }

    /// Igual que [`ChunkStore::put_file`] con un tamano de trozo explicito.
    ///
    /// Existe porque el tamano tendra que ser **por proveedor**: contra Drive el coste es por
    /// operacion y conviene agrandarlo, contra OneDrive es por byte y agrandarlo empeora la
    /// latencia de la primera lectura. Ver `docs/VIABILIDAD.md`.
    pub async fn put_file_with<R>(
        &self,
        mut reader: R,
        chunk_size: usize,
    ) -> Result<FileReport, CoreError>
    where
        R: AsyncRead + Unpin,
    {
        if chunk_size == 0 {
            return Err(CoreError::ZeroChunkSize);
        }

        let mut chunks = Vec::new();
        let mut degraded: Vec<String> = Vec::new();
        let mut len: u64 = 0;
        let mut buf = vec![0u8; chunk_size];

        loop {
            let n = leer_hasta_llenar(&mut reader, &mut buf).await?;
            if n == 0 {
                break;
            }

            let report = self.put(&buf[..n]).await?;
            len += n as u64;
            for nombre in report.degraded {
                if !degraded.contains(&nombre) {
                    degraded.push(nombre);
                }
            }
            chunks.push(report.chunk);

            if n < chunk_size {
                break;
            }
        }

        Ok(FileReport {
            manifest: Manifest {
                chunks,
                len,
                chunk_size,
            },
            degraded,
        })
    }

    /// Reconstruye el fichero entero y lo escribe en `writer`.
    pub async fn get_file<W>(&self, manifest: &Manifest, mut writer: W) -> Result<(), CoreError>
    where
        W: AsyncWrite + Unpin,
    {
        manifest.validate()?;
        for chunk in &manifest.chunks {
            let datos = self.get(chunk).await?;
            writer.write_all(&datos).await?;
        }
        writer.flush().await?;
        Ok(())
    }

    /// Lee hasta `len` bytes desde `offset`, bajando **solo los trozos que el rango toca**.
    ///
    /// Esta es la operacion que hace viable un montaje. Sin ella, leer 4 KiB de un fichero de
    /// 1 GiB obligaria a reconstruirlo entero desde los proveedores.
    ///
    /// Se comporta como `pread`: si el rango se pasa del final devuelve lo que haya, y si
    /// empieza en el final o despues devuelve vacio. Un sistema de ficheros espera eso, no un
    /// error.
    pub async fn read_at(
        &self,
        manifest: &Manifest,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, CoreError> {
        manifest.validate()?;
        if len == 0 || offset >= manifest.len {
            return Ok(Vec::new());
        }

        let fin = offset.saturating_add(len as u64).min(manifest.len);
        let (mut i, mut dentro) = manifest.locate(offset);
        let mut salida = Vec::with_capacity((fin - offset) as usize);
        let mut pos = offset;

        while pos < fin {
            // `validate` garantiza que los trozos cubren `len`, asi que este indice existe.
            let datos = self.get_shared(&manifest.chunks[i]).await?;
            let disponible = datos.len().saturating_sub(dentro);
            let toma = ((fin - pos) as usize).min(disponible);
            salida.extend_from_slice(&datos[dentro..dentro + toma]);

            pos += toma as u64;
            dentro = 0;
            i += 1;
        }

        Ok(salida)
    }
}

/// Llena `buf` salvo que el lector se agote antes. Devuelve cuantos bytes hay.
///
/// No es lo mismo que un solo `read`: `read` puede devolver menos de lo pedido sin que sea el
/// final del fichero, y una tuberia, un socket o un fichero por red lo hacen constantemente.
/// Cortar el trozo ahi produciria trozos de tamano arbitrario, y con ellos la lectura por
/// rango deja de funcionar, porque cuenta con que todos los trozos menos el ultimo miden
/// `chunk_size`.
async fn leer_hasta_llenar<R>(reader: &mut R, buf: &mut [u8]) -> Result<usize, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut leido = 0;
    while leido < buf.len() {
        let n = reader.read(&mut buf[leido..]).await?;
        if n == 0 {
            break;
        }
        leido += n;
    }
    Ok(leido)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChunkId;
    use crate::testutil::{clave, como_backends, contadores, reiniciar, store, total_gets};
    use libredisk_raid::Layout;

    /// Trozos de 256 bytes en las pruebas: los limites son lo que importa, y con 16 MiB cada
    /// caso costaria segundos sin probar nada distinto.
    const TAM: usize = 256;

    fn rs() -> Layout {
        Layout::ReedSolomon { k: 4, m: 2 }
    }

    /// Datos sin repeticiones: si un offset se calcula mal, el contenido no coincide.
    fn datos(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| (i.wrapping_mul(31).wrapping_add(7) % 251) as u8)
            .collect()
    }

    #[tokio::test]
    async fn ida_y_vuelta_de_varios_trozos() {
        let s = store(rs());
        let d = datos(1000);
        let r = s.put_file_with(&d[..], TAM).await.unwrap();

        assert!(r.is_fully_redundant());
        assert_eq!(r.manifest.len, 1000);
        assert_eq!(
            r.manifest.chunks.len(),
            4,
            "1000 bytes en trozos de 256 son tres llenos y uno de 232"
        );
        assert_eq!(r.manifest.chunks[3].plain_len, 232);
        r.manifest.validate().unwrap();

        let mut salida = Vec::new();
        s.get_file(&r.manifest, &mut salida).await.unwrap();
        assert_eq!(salida, d);
    }

    #[tokio::test]
    async fn fichero_vacio() {
        let s = store(rs());
        let r = s.put_file_with(&[][..], TAM).await.unwrap();

        assert!(r.manifest.is_empty());
        assert!(
            r.manifest.chunks.is_empty(),
            "un fichero vacio no gasta trozos"
        );
        r.manifest.validate().unwrap();

        let mut salida = Vec::new();
        s.get_file(&r.manifest, &mut salida).await.unwrap();
        assert!(salida.is_empty());
        assert!(s.read_at(&r.manifest, 0, 10).await.unwrap().is_empty());
    }

    /// Los limites del troceado son donde se rompen los troceadores.
    #[tokio::test]
    async fn limites_exactos_del_tamano_de_trozo() {
        let s = store(rs());
        for n in [1, 255, 256, 257, 511, 512, 513] {
            let d = datos(n);
            let r = s.put_file_with(&d[..], TAM).await.unwrap();

            r.manifest.validate().unwrap();
            assert_eq!(r.manifest.len, n as u64, "n={n}");
            assert_eq!(r.manifest.chunks.len(), n.div_ceil(TAM), "n={n}");

            let mut salida = Vec::new();
            s.get_file(&r.manifest, &mut salida).await.unwrap();
            assert_eq!(salida, d, "n={n}");
        }
    }

    #[tokio::test]
    async fn read_at_en_posiciones_que_cruzan_fronteras() {
        let s = store(rs());
        let d = datos(1000);
        let m = s.put_file_with(&d[..], TAM).await.unwrap().manifest;

        // Dentro de un trozo, pegado a una frontera, cruzando una, cruzando varias, y entero.
        for (off, len) in [
            (0usize, 10usize),
            (250, 12),
            (255, 2),
            (256, 1),
            (100, 400),
            (768, 300),
            (999, 1),
            (0, 1000),
        ] {
            assert_eq!(
                s.read_at(&m, off as u64, len).await.unwrap(),
                &d[off..(off + len).min(1000)],
                "offset={off} len={len}"
            );
        }
    }

    #[tokio::test]
    async fn read_at_se_comporta_como_pread_en_el_final() {
        let s = store(rs());
        let d = datos(1000);
        let m = s.put_file_with(&d[..], TAM).await.unwrap().manifest;

        // Pasarse del final devuelve lo que hay, no un error.
        assert_eq!(s.read_at(&m, 990, 100).await.unwrap(), &d[990..]);
        // Empezar en el final, o despues, devuelve vacio.
        assert!(s.read_at(&m, 1000, 10).await.unwrap().is_empty());
        assert!(s.read_at(&m, 5000, 10).await.unwrap().is_empty());
        // Longitud cero tambien.
        assert!(s.read_at(&m, 0, 0).await.unwrap().is_empty());
    }

    /// Cuenta descargas. Es la razon de ser de `read_at`: sin ella, leer unos pocos bytes
    /// obligaria a reconstruir el fichero entero desde los proveedores.
    #[tokio::test]
    async fn read_at_solo_baja_los_trozos_que_el_rango_toca() {
        let layout = rs();
        let cs = contadores(layout.shard_count());
        let s = ChunkStore::new(layout, como_backends(&cs), clave()).unwrap();

        let d = datos(20 * TAM);
        let m = s.put_file_with(&d[..], TAM).await.unwrap().manifest;
        assert_eq!(m.chunks.len(), 20);

        // Veinte bytes dentro de un solo trozo: un fragmento por proveedor y nada mas.
        reiniciar(&cs);
        s.read_at(&m, (5 * TAM + 10) as u64, 20).await.unwrap();
        assert_eq!(
            total_gets(&cs),
            layout.shard_count(),
            "leer dentro de un trozo no puede tocar los otros diecinueve"
        );

        // Un rango que cruza una frontera: exactamente dos trozos.
        reiniciar(&cs);
        s.read_at(&m, (TAM - 5) as u64, 10).await.unwrap();
        assert_eq!(total_gets(&cs), 2 * layout.shard_count());

        // Y el fichero entero sigue costando los veinte, claro.
        reiniciar(&cs);
        s.get_file(&m, &mut Vec::new()).await.unwrap();
        assert_eq!(total_gets(&cs), 20 * layout.shard_count());
    }

    /// El direccionamiento por contenido da deduplicacion sin pedirla.
    #[tokio::test]
    async fn los_trozos_identicos_comparten_id() {
        let s = store(rs());
        let bloque = datos(TAM);
        let d: Vec<u8> = bloque
            .iter()
            .chain(&bloque)
            .chain(&bloque)
            .copied()
            .collect();
        let m = s.put_file_with(&d[..], TAM).await.unwrap().manifest;

        assert_eq!(m.chunks.len(), 3);
        assert_eq!(m.chunks[0].id, m.chunks[1].id);
        assert_eq!(m.chunks[1].id, m.chunks[2].id);

        let mut salida = Vec::new();
        s.get_file(&m, &mut salida).await.unwrap();
        assert_eq!(salida, d, "compartir id no puede perder contenido");
    }

    /// Entrega un byte por llamada, que es el peor caso legal de `AsyncRead`.
    struct LectorTacano<'a> {
        datos: &'a [u8],
        pos: usize,
    }

    impl tokio::io::AsyncRead for LectorTacano<'_> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.pos < self.datos.len() && buf.remaining() > 0 {
                let pos = self.pos;
                buf.put_slice(&self.datos[pos..pos + 1]);
                self.pos += 1;
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Una tuberia, un socket o un fichero por red entregan menos de lo pedido sin que sea el
    /// final. Si el troceado se fiara de un solo `read`, los trozos saldrian del tamano que
    /// quisiera el lector y la lectura por rango dejaria de funcionar.
    #[tokio::test]
    async fn un_lector_que_entrega_de_uno_en_uno_no_parte_los_trozos() {
        let s = store(rs());
        let d = datos(1000);
        let r = s
            .put_file_with(LectorTacano { datos: &d, pos: 0 }, TAM)
            .await
            .unwrap();

        r.manifest.validate().unwrap();
        assert_eq!(r.manifest.chunks.len(), 4);
        assert_eq!(r.manifest.chunks[0].plain_len, TAM);

        let mut salida = Vec::new();
        s.get_file(&r.manifest, &mut salida).await.unwrap();
        assert_eq!(salida, d);
    }

    #[tokio::test]
    async fn el_manifiesto_viaja_como_json() {
        let s = store(rs());
        let m = s
            .put_file_with(&datos(600)[..], TAM)
            .await
            .unwrap()
            .manifest;

        let json = serde_json::to_string(&m).unwrap();
        let vuelta: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(vuelta, m);
    }

    #[tokio::test]
    async fn un_tamano_de_trozo_cero_se_rechaza() {
        let s = store(rs());
        assert!(matches!(
            s.put_file_with(&datos(10)[..], 0).await,
            Err(CoreError::ZeroChunkSize)
        ));
    }

    #[test]
    fn validate_caza_los_manifiestos_incoherentes() {
        let trozo = |n: usize| ChunkRef {
            id: ChunkId::of(&vec![0u8; n]),
            plain_len: n,
            sealed_len: n + 32,
        };

        // Tamano cero: la division de `locate` no puede llegar a ejecutarse nunca.
        assert!(matches!(
            Manifest {
                chunks: vec![],
                len: 0,
                chunk_size: 0
            }
            .validate(),
            Err(CoreError::ZeroChunkSize)
        ));

        // La longitud declarada no cuadra con la suma de los trozos.
        assert!(matches!(
            Manifest {
                chunks: vec![trozo(TAM)],
                len: 300,
                chunk_size: TAM
            }
            .validate(),
            Err(CoreError::InvalidManifest(_))
        ));

        // Un trozo que no es el ultimo mide menos de la cuenta. Aqui es donde `read_at`
        // devolveria datos mal en silencio si nadie lo comprobara.
        assert!(matches!(
            Manifest {
                chunks: vec![trozo(100), trozo(TAM)],
                len: 356,
                chunk_size: TAM
            }
            .validate(),
            Err(CoreError::InvalidManifest(_))
        ));

        // Y uno sano pasa.
        Manifest {
            chunks: vec![trozo(TAM), trozo(44)],
            len: 300,
            chunk_size: TAM,
        }
        .validate()
        .unwrap();
    }
}
