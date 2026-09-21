# LibreDisk

**Una sola unidad. Muchas nubes.**

LibreDisk agrega varios servicios de almacenamiento en la nube (S3/B2/R2/Wasabi/Storj,
WebDAV/Nextcloud, Google Drive, OneDrive, Dropbox…) en **un único volumen virtual** que se
monta como una unidad más del sistema en Windows y Linux, con semántica tipo RAID
(espejo, striping y paridad por *erasure coding*) y **cifrado de conocimiento cero**.

> Estado: **pre-alfa.** El núcleo (cifrado, RAID, proveedores, almacén de trozos) funciona
> y está cubierto por tests. Google Drive y OneDrive pasan la suite de conformidad contra
> cuentas reales. Todavía **no se monta como unidad**: eso es el siguiente hito.

Sitio del proyecto: https://libredisk.chambaya.net

## Probarlo

Requiere Rust estable (edición 2024).

```sh
cargo test --workspace          # 93 tests
cargo run -- layout rs:4,2      # tolerancia y sobrecoste de una topología
cargo run -- demo --layout rs:4,2 --kib 512
cargo run -- check              # conformidad de los proveedores configurados
```

`demo` hace el ciclo completo sobre directorios locales, uno por "proveedor": escribe un
trozo cifrado y repartido, **borra dos proveedores enteros**, lo vuelve a leer íntegro y
regenera los fragmentos que faltaban.

## Un volumen de verdad

Con proveedores configurados (ver [docs/PRUEBAS-PROVEEDORES.md](docs/PRUEBAS-PROVEEDORES.md))
y una frase de paso:

```sh
export LIBREDISK_PASSPHRASE='...'        # no hay recuperación: si la pierdes, lo pierdes todo
cargo run -- volume create --layout rs:2,1
cargo run -- put ./pelicula.mkv /medios/pelicula.mkv
cargo run -- ls /medios
cargo run -- cat /medios/pelicula.mkv --offset 1000000 --len 64   # solo baja el trozo que toca
cargo run -- get /medios/pelicula.mkv ./vuelta.mkv
cargo run -- scrub                        # qué le falta a cada trozo, y dónde
cargo run -- rebuild                      # regenerar lo que falte
```

## Estructura

| Crate | Qué hace |
|---|---|
| `libredisk-crypto` | Argon2id + XChaCha20-Poly1305 por trozo |
| `libredisk-raid` | Reed-Solomon `k+m`, espejo, concat |
| `libredisk-backend` | `trait Backend` + adaptador OpenDAL (~40 proveedores) |
| `libredisk-core` | Almacén de trozos: id por BLAKE3, `put`/`get`/`scrub`/`repair` |
| `libredisk-cli` | Binario `libredisk` |

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
- [docs/PRUEBAS-PROVEEDORES.md](docs/PRUEBAS-PROVEEDORES.md) — probar contra cuentas reales (OneDrive, Google Drive, S3, WebDAV)

## Licencia

[AGPL-3.0](LICENSE) — ver [docs/VIABILIDAD.md](docs/VIABILIDAD.md#licencia) para el razonamiento.
