# LibreDisk

**Una sola unidad. Muchas nubes.**

LibreDisk agrega varios servicios de almacenamiento en la nube (S3/B2/R2/Wasabi/Storj,
WebDAV/Nextcloud, Google Drive, OneDrive, Dropbox…) en **un único volumen virtual** que se
monta como una unidad más del sistema en Windows y Linux, con semántica tipo RAID
(espejo, striping y paridad por *erasure coding*) y **cifrado de conocimiento cero**.

> Estado: **pre-alfa.** El núcleo funciona y está cubierto por tests: cifrado, RAID,
> troceado, árbol de metadatos copy-on-write y rescate. Google Drive y OneDrive pasan la
> suite de conformidad contra cuentas reales. **Se monta como unidad en Windows en lectura
> y escritura**, con caché en memoria y en disco y subida en segundo plano. Sigue siendo
> pre-alfa: no es donde poner el único ejemplar de nada.

Sitio del proyecto: https://libredisk.chambaya.net

## Instalar en Windows

Hay dos descargas en la página de publicaciones, y la que quieres casi seguro es la primera:

| | |
|---|---|
| `libredisk-X.Y.Z-x64-completo.exe` | Instala **WinFsp y LibreDisk**. Es lo normal. |
| `libredisk-X.Y.Z-x64.msi` | Solo LibreDisk; se niega a instalarse si no hay WinFsp. Para quien ya administra WinFsp por su cuenta. |

WinFsp es quien pone el controlador que hace aparecer la unidad. El instalador completo
**encadena el instalador de WinFsp sin modificar, tal y como lo publica su proyecto**, y lo
detecta si ya está: no lo toca, ni lo reinstala, ni lo quita al desinstalar LibreDisk. Puede
haber otros programas usándolo.

> **Hace falta reiniciar una vez después de instalar WinFsp.** Su proveedor de red se registra
> en el arranque, así que hasta el primer reinicio `net use` responde «error de sistema 67»
> aunque todo lo demás esté bien. Montar con `libredisk mount Z:` funciona desde el primer
> momento.

> **Todavía no van firmados.** Windows los anunciará como de «editor desconocido» y SmartScreen
> avisará la primera vez. Qué cuesta eso y por qué sigue sin resolverse está en
> [docs/FIRMA.md](docs/FIRMA.md). Comprueba el SHA-256 contra el publicado en la versión.

Para construirlos desde el código hacen falta el SDK de .NET y WiX 5:

```powershell
winget install Microsoft.DotNet.SDK.8
dotnet tool install --global wix --version 5.0.2
wix extension add --global WixToolset.BootstrapperApplications.wixext/5.0.2
.\packaging\windows\construir-msi.ps1            # los dos
.\packaging\windows\construir-msi.ps1 -SoloMsi   # sin red, solo el MSI
```

Para que el volumen esté disponible sin tener una consola abierta, se registra en el lanzador
de WinFsp y se conecta como una unidad de red: **Windows pide la frase de paso al conectar y
no se guarda en ninguna parte**. Ver [docs/SERVICIO.md](docs/SERVICIO.md).

## Probarlo

Requiere Rust estable (edición 2024). La ventana de escritorio usa el webview del sistema:
en Windows ya viene (WebView2), y en Linux hacen falta sus cabeceras
—`libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev pkg-config` en Debian y derivados—.

```sh
cargo test --workspace          # 241 tests
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
cargo run -- providers list               # proveedores del volumen y los configurados de más
cargo run -- providers add --mas-seguridad   # una caída más de margen (barato)
cargo run -- providers add --mas-espacio     # más capacidad (reescribe el volumen)
cargo run -- providers remove fs2 --menos-seguridad   # sacar uno, aunque ya no responda
cargo run -- rebuild                      # regenerar lo que falte

cargo run -- header export ./cabecera.json   # respaldo: sin la sal no hay clave
cargo run -- rescue                          # si se pierde el puntero de raiz

cargo run -- put ./grande.mkv /medios/grande.mkv --diferido   # vuelve enseguida
cargo run -- sync                                             # y sube cuando toque
cargo run -- mount Z:                        # montarlo como unidad (Windows)
cargo run -- mount Z: --escritura            # ...y poder escribir (sube solo cada 30 s)

cargo run -p libredisk-gui                   # la ventana de escritorio
```

## Montaje

En Windows, `libredisk mount Z:` expone el volumen como una unidad del sistema: el Explorador
lista carpetas, cualquier programa abre ficheros y `Copy-Item` copia. Requiere
[WinFsp](https://winfsp.dev), que trae el instalador completo, y el toolchain **MSVC** de Rust
para compilarlo.

Con `--escritura` se monta en **lectura y escritura**. Lo que se escribe queda en esta máquina
y un hilo de fondo lo sube cada `--sync-cada` segundos (30 por defecto); al desmontar se sube
lo que quede antes de salir.

Leer va razonable porque hay caché en memoria (`--cache-mb`, 512 por defecto): la primera
lectura de cada trozo va a la nube y las siguientes no. Medido sobre un proveedor local,
releer 7,6 MiB pasa de 2 718 ms a 23 ms.

Hay además una **caché en disco** (`--disco-mb`, 4096 por defecto) que sobrevive al reinicio,
así que montar de nuevo no vuelve a pagar la red. Guarda los trozos **cifrados**: los datos
no quedan en claro en la máquina.

En Linux, `libredisk mount /ruta/vacia` hace lo mismo con FUSE, y está probado de punta a
punta: leer y escribir con el contenido intacto, renombrar carpetas con cosas dentro, y los
códigos de error que `rm -r` necesita para no rendirse. Lo comprueba CI en cada cambio,
montando de verdad.

> **Con `fusermount3` 3.18.2 hace falta root para montar**, y no es cosa del adaptador: ese
> ayudante no acepta el descriptor que se le pasa. Como root no hace falta, porque se monta
> con `mount(2)` directo. Con 3.14.0 monta sin privilegios, que es lo que comprueba CI. El
> detalle, y lo que queda sin cerrar, en [docs/ROADMAP.md](docs/ROADMAP.md).


## Estructura

| Crate | Qué hace |
|---|---|
| `libredisk-crypto` | Argon2id + XChaCha20-Poly1305 por trozo |
| `libredisk-raid` | Reed-Solomon `k+m`, espejo, reparto puro (RAID 0), concat |
| `libredisk-backend` | `trait Backend` + adaptador OpenDAL (~40 proveedores) |
| `libredisk-core` | Almacén de trozos: id por BLAKE3, `put`/`get`/`scrub`/`repair` |
| `libredisk-mount` | Montaje como unidad del sistema: WinFsp en Windows, FUSE en Linux |
| `libredisk-gui` | Ventana de escritorio (Tauri, sin Node): crear, montar, salud y cola de subida |
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
| Vendor lock-in | Añade o retira proveedores eligiendo qué priorizar, sin dejar de poder leer el volumen. **Retirar funciona aunque la cuenta ya no responda** |

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
- [docs/SERVICIO.md](docs/SERVICIO.md) — montar como unidad de red sin tener una consola abierta
- [docs/FIRMA.md](docs/FIRMA.md) — por qué el instalador todavía no va firmado, y qué haría falta

## Créditos

**WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos**
— <https://github.com/winfsp/winfsp>

WinFsp pone el controlador que hace que el volumen aparezca como una unidad de Windows.
LibreDisk lo usa al amparo de su excepción para software libre, y la cumple: se distribuye
bajo AGPL-3.0, lleva este aviso aquí, en [packaging/windows/AVISOS.txt](packaging/windows/AVISOS.txt)
—que se instala junto al programa— y en la propia orden de montaje, y no va con ningún
componente propietario.

## Licencia

[AGPL-3.0](LICENSE) — ver [docs/VIABILIDAD.md](docs/VIABILIDAD.md#licencia) para el razonamiento.
