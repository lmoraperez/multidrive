# LibreDisk

**Una sola unidad. Muchas nubes.**

LibreDisk agrega varios servicios de almacenamiento en la nube (S3/B2/R2/Wasabi/Storj,
WebDAV/Nextcloud, Google Drive, OneDrive, Dropbox…) en **un único volumen virtual** que se
monta como una unidad más del sistema en Windows y Linux, con semántica tipo RAID
(espejo, striping y paridad por *erasure coding*) y **cifrado de conocimiento cero**.

> Estado: **diseño / pre-alfa.** Todavía no hay código funcional. Este repositorio
> arranca con la arquitectura, el análisis de viabilidad y la hoja de ruta.

Sitio del proyecto: https://libredisk.chambaya.net

---

## La idea en una frase

Si RAID convirtió varios discos poco fiables en un volumen fiable, LibreDisk hace lo mismo
con varias cuentas de nube poco fiables (y con cuotas): **ningún proveedor tiene tus datos
completos, ninguno puede leerlos, y la caída de uno no te deja sin volumen.**

## Qué resuelve

| Problema | Respuesta de LibreDisk |
|---|---|
| Tengo 15 GB aquí, 5 GB allá, 1 TB en otro lado | Un solo volumen agregado |
| Si el proveedor me suspende la cuenta, lo pierdo todo | Paridad *k de n*: sobrevive a la pérdida de *m* proveedores |
| El proveedor puede leer mis archivos | Cifrado en cliente (XChaCha20-Poly1305), nombres incluidos |
| Vendor lock-in | Añade o retira proveedores y el volumen se rebalancea |

## Qué **no** es

- No es un sistema de respaldo (para eso están restic/kopia/borg).
- No es un disco de sistema ni un volumen para bases de datos: la escritura aleatoria de
  4 KB sobre Internet no tiene arreglo físico. Es para medios, archivo y datos "tibios".
- No es magia contra los Términos de Servicio de los proveedores. Ver
  [docs/VIABILIDAD.md](docs/VIABILIDAD.md).

## Documentos

- [docs/VIABILIDAD.md](docs/VIABILIDAD.md) — análisis honesto de viabilidad, riesgos y competencia
- [docs/ARQUITECTURA.md](docs/ARQUITECTURA.md) — diseño técnico
- [docs/ROADMAP.md](docs/ROADMAP.md) — hoja de ruta por versiones

## Licencia

[AGPL-3.0](LICENSE) — ver [docs/VIABILIDAD.md](docs/VIABILIDAD.md#licencia) para el razonamiento.
