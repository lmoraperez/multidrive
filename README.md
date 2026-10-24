# MultiDrive

**Español** · [English](README.en.md)

**Una sola unidad. Muchas nubes.**

MultiDrive agrega varios servicios de almacenamiento en la nube (S3/B2/R2/Wasabi/Storj,
WebDAV/Nextcloud, Google Drive, OneDrive) en **un único volumen virtual** que se monta como
una unidad más del sistema en Windows y Linux, y que en Android aparece dentro de la app de
Archivos, con semántica tipo RAID (espejo, striping y paridad por *erasure coding*) y
**cifrado de conocimiento cero**.

> Estado: **alfa.** El núcleo funciona y está cubierto por tests: cifrado, RAID, troceado,
> árbol de metadatos copy-on-write y rescate. Google Drive y OneDrive pasan la suite de
> conformidad contra cuentas reales. **Se monta como unidad en Windows en lectura y
> escritura**, con caché en memoria y en disco y subida en segundo plano. **Hay aplicación
> de Android**, en pruebas internas de Google Play, que crea volúmenes, los amplía, los
> elimina, lee, escribe y sube solas las carpetas del teléfono que se le digan.
>
> Alfa quiere decir que los caminos principales funcionan y se usan a diario, y que todavía
> faltan piezas: miniaturas, versión web, y de proveedores sólo Drive y OneDrive están
> verificados contra cuentas reales. **No es donde poner el único ejemplar de nada.**

Sitio del proyecto: https://multidrive.net

## Probarla

### 👉 [Apúntate a las pruebas de Android](https://multidrive.net/#probarla)

La aplicación está en **pruebas internas de Google Play**: no se busca en la tienda, se entra
por invitación. En ese enlace hay un formulario; deja tu correo y entras en la lista.

Tiene que ser **la cuenta de Google con la que usas el teléfono**, no otro correo: Google manda
ahí la invitación y sólo esa cuenta puede aceptarla. La invitación no es inmediata, porque la
lista se añade a mano.

Hace falta Android 8 o posterior y un teléfono `arm64-v8a`. La interfaz está en español.

> El formulario no puede vivir en este README: GitHub elimina `<form>` y `<input>` al mostrar
> la página, así que lo que queda es el enlace.

### 💻 [Descargar para Windows](https://github.com/lmoraperez/multidrive/releases/latest)

Windows de 64 bits. Dos ficheros, y para casi todo el mundo el bueno es
`multidrive-0.0.1-x64-completo.exe`, que trae **WinFsp** dentro: es lo que permite que el
volumen aparezca como una unidad del sistema. El `.msi` a secas es para quien ya tenga WinFsp
o lo despliegue por su cuenta.

Van **sin firmar**, así que SmartScreen avisará de que no reconoce al editor. Las sumas
`sha256` para comprobar lo que bajas están en las [notas de la
publicación](https://github.com/lmoraperez/multidrive/releases/tag/v0.0.1-windows).

También se compila desde el código: [Instalar en Windows](#instalar-en-windows).

## En treinta segundos

[![MultiDrive en treinta segundos](https://multidrive.net/video/cartel.png)](https://multidrive.net/#spot)

No lleva sonido: lo cuenta con texto en pantalla. **El reproductor está en
[multidrive.net](https://multidrive.net/#spot)**, no aquí: GitHub solo reproduce vídeo alojado
en sus propios dominios, así que esto es el cartel y el enlace.

## Instalar en Windows

Hay dos descargas en la página de publicaciones, y la que quieres casi seguro es la primera:

| | |
|---|---|
| `multidrive-X.Y.Z-x64-completo.exe` | Instala **WinFsp y MultiDrive**. Es lo normal. |
| `multidrive-X.Y.Z-x64.msi` | Solo MultiDrive; se niega a instalarse si no hay WinFsp. Para quien ya administra WinFsp por su cuenta. |

Los dos instalan **la ventana y la orden**: dejan un acceso directo «MultiDrive» en el
escritorio y otro en el menú de inicio, y `multidrive` queda en el PATH para cualquier consola.

Si no quieres el del escritorio, el MSI acepta que se lo digas:

```
msiexec /i multidrive-X.Y.Z-x64.msi ACCESO_ESCRITORIO=0
```

WinFsp es quien pone el controlador que hace aparecer la unidad. El instalador completo
**encadena el instalador de WinFsp sin modificar, tal y como lo publica su proyecto**, y lo
detecta si ya está: no lo toca, ni lo reinstala, ni lo quita al desinstalar MultiDrive. Puede
haber otros programas usándolo.

> **La ventana necesita WebView2**, que es lo que dibuja su interfaz. Windows 11 lo trae de
> fábrica y los Windows 10 al día también; si falta, la ventana lo dice con un mensaje y el
> enlace en vez de morirse sin más. La orden `multidrive` funciona igual sin él.

> **Hace falta reiniciar una vez después de instalar WinFsp.** Su proveedor de red se registra
> en el arranque, así que hasta el primer reinicio `net use` responde «error de sistema 67»
> aunque todo lo demás esté bien. Montar con `multidrive mount Z:` funciona desde el primer
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

**En Windows hace falta el toolchain MSVC**, no el GNU: `winfsp-sys` —lo que permite montar
la unidad— no compila con `x86_64-pc-windows-gnu` y lo dice con un `unsupported triple` en
mitad de la compilación, que no se parece en nada al problema que es. Si `rustup show
active-toolchain` dice `gnu`:

```sh
rustup toolchain install stable-x86_64-pc-windows-msvc
rustup override set stable-x86_64-pc-windows-msvc    # solo para este directorio
```

Es un `override` **por ruta**: si mueves o renombras la carpeta del repositorio, se queda
huérfano y la compilación vuelve a caer en GNU. `rustup override unset --nonexistent` limpia
los que sobran.

```sh
cargo test --workspace          # 356 tests
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
export MULTIDRIVE_PASSPHRASE='...'        # no hay recuperación: si la pierdes, lo pierdes todo
cargo run -- volume create --layout rs:2,1
cargo run -- put ./pelicula.mkv /medios/pelicula.mkv
cargo run -- ls /medios
cargo run -- cat /medios/pelicula.mkv --offset 1000000 --len 64   # solo baja el trozo que toca
cargo run -- get /medios/pelicula.mkv ./vuelta.mkv
cargo run -- scrub                        # qué le falta a cada trozo, y dónde
cargo run -- providers list               # proveedores del volumen y los configurados de más
cargo run -- providers add --mas-seguridad   # una caída más de margen (barato)
cargo run -- providers add --mas-espacio     # más capacidad (en lineal solo mueve lo que aporta el nuevo)
cargo run -- providers remove fs2 --menos-seguridad   # sacar uno, aunque ya no responda
cargo run -- rebuild                      # regenerar lo que falte
cargo run -- volume migrar                # volumen de antes del cambio de nombre: renombra su cabecera
cargo run -- config publicar               # guarda la configuracion cifrada en tus propias nubes
cargo run -- config recuperar             # la trae a este equipo: basta con tener una cuenta

cargo run -- header export ./cabecera.json   # respaldo: sin la sal no hay clave
cargo run -- rescue                          # si se pierde el puntero de raiz

cargo run -- put ./grande.mkv /medios/grande.mkv --diferido   # vuelve enseguida
cargo run -- sync                                             # y sube cuando toque
cargo run -- mount Z:                        # montarlo como unidad (Windows)
cargo run -- mount Z: --escritura            # ...y poder escribir (sube solo cada 30 s)

cargo run -p multidrive-gui                   # la ventana de escritorio
```

## Montaje

En Windows, `multidrive mount Z:` expone el volumen como una unidad del sistema: el Explorador
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

En Linux, `multidrive mount /ruta/vacia` hace lo mismo con FUSE, y está probado de punta a
punta: leer y escribir con el contenido intacto, renombrar carpetas con cosas dentro, y los
códigos de error que `rm -r` necesita para no rendirse. Lo comprueba CI en cada cambio,
montando de verdad.

> **Con `fusermount3` 3.18.2 hace falta root para montar**, y no es cosa del adaptador: ese
> ayudante no acepta el descriptor que se le pasa. Como root no hace falta, porque se monta
> con `mount(2)` directo. Con 3.14.0 monta sin privilegios, que es lo que comprueba CI. El
> detalle, y lo que queda sin cerrar, en [docs/ROADMAP.md](docs/ROADMAP.md).


## Android

| | | | |
|:-:|:-:|:-:|:-:|
| <img src="https://multidrive.net/capturas/principal.png" width="200" alt="Pantalla principal con el volumen abierto y el espacio de cada cuenta"> | <img src="https://multidrive.net/capturas/topologia.png" width="200" alt="Elección de topología, con lo que cuesta cada una"> | <img src="https://multidrive.net/capturas/archivos.png" width="200" alt="Explorador de archivos"> | <img src="https://multidrive.net/capturas/ampliar.png" width="200" alt="Ampliar el volumen con más cuentas"> |
| El volumen abierto | Elegir el reparto | Tus archivos | Ampliar después |

Capturas hechas en un teléfono con la versión publicada. Sin maquetar ni retocar.

Hay **aplicación de Android**, publicada en Google Play como `com.jlsoltech.multidrive`. Está
en **pruebas internas**: se instala desde el enlace de prueba, no buscándola en la tienda.
Pide Android 8 o posterior y es solo para `arm64-v8a`, que es lo que llevan los teléfonos
desde hace años.

No es un visor de lo que hizo el escritorio: desde el móvil se **crea** un volumen —eligiendo
proveedores y topología igual que en la ventana—, se **amplía** con cuentas nuevas, y se
**elimina**. Y para abrirlo no
hay que volver a enlazar las cuentas una por una: con **una** cuenta enlazada y la frase de
paso, la maleta trae las demás. En un teléfono eso se nota, porque cada cuenta son una vuelta
al navegador y una pantalla de permisos escribiendo con el teclado del móvil.

Lo que hace, una vez abierto:

- **Aparece en la app de Archivos** del sistema como un almacenamiento más, y en el selector de
  cualquier aplicación que pida un fichero. Eso lo pone un `DocumentsProvider`, que es el
  equivalente de Android a la unidad `Z:` de Windows.
- **Trae su propio explorador**, con buscador, tamaños y fechas, carpetas favoritas y menú
  lateral. Está para no depender de la app de Archivos del fabricante, que cambia de un
  teléfono a otro.
- **Escribe**: crear carpetas, renombrar, borrar y subir, tanto desde el explorador como
  compartiendo a MultiDrive desde cualquier aplicación.
- **Lee por rangos**: abrir un vídeo de 2 GiB baja los trozos que el reproductor pide, no el
  fichero entero. Es lo que separa esto de un descargador.
- **Sube carpetas del teléfono solo**, como la carga automática de Nextcloud: se eligen las
  carpetas, y un trabajo del sistema sube lo nuevo cuando hay red —con opción de **solo
  WiFi**— sin tener la aplicación delante.
- **Dice cuánto queda**: el tamaño del volumen con su espacio libre, y una barra por cuenta.
  Lo ocupado que enseña la barra es el de la cuenta entera, no solo lo de MultiDrive, y la
  pantalla lo dice: ahí cuentan también tus fotos y tu correo.
- **Crece metiéndole cuentas nuevas**, con una advertencia que da antes de empezar: solo un
  volumen `lineal` gana espacio al añadir una. En espejo, paridad o unión cada trozo se parte
  entre todas, así que una más da **margen de caídas** y ni un byte de sitio; ganar sitio ahí
  es reescribir el volumen entero, que se hace desde el escritorio.
- **Vuelve sola del navegador** al enlazar una cuenta, sin tener que cerrar la pestaña y
  buscar la aplicación.

Dos cosas que conviene saber antes de activarlas:

> **La subida automática necesita la frase de paso guardada.** Un trabajo que arranca solo no
> puede pedirla, así que para activarla hay que dejarla sellada en el **llavero de Android**,
> cifrada por el hardware y exigiendo que el teléfono esté desbloqueado. No es gratis: quien
> tenga el teléfono desbloqueado tiene el volumen. La aplicación lo dice con esas palabras
> antes de activarlo, y se puede apagar y borrar la frase cuando se quiera.

> **No hay copia de seguridad de la aplicación**, a propósito. La copia automática de Android
> habría subido el fichero de configuración —con los tokens de las nubes— a la cuenta de Drive
> del usuario, y habría devuelto cuentas ya eliminadas al reinstalar. Reinstalar empieza
> limpio: se vuelven a enlazar las cuentas, o se trae la maleta con una sola.

Todavía no hay miniaturas: las carpetas de fotos se ven como una lista de nombres. Cómo está
construido, qué se midió en un teléfono de verdad y qué queda abierto está en
[docs/ANDROID.md](docs/ANDROID.md).

## Estructura

| Crate | Qué hace |
|---|---|
| `multidrive-crypto` | Argon2id + XChaCha20-Poly1305 por trozo |
| `multidrive-raid` | Reed-Solomon `k+m`, espejo, reparto puro (RAID 0), concat |
| `multidrive-backend` | `trait Backend` + adaptador OpenDAL (~40 proveedores) |
| `multidrive-core` | Almacén de trozos: id por BLAKE3, `put`/`get`/`scrub`/`repair` |
| `multidrive-mount` | Montaje como unidad del sistema: WinFsp en Windows, FUSE en Linux |
| `multidrive-gui` | Ventana de escritorio (Tauri, sin Node): crear, montar, salud y cola de subida |
| `multidrive-android` | Biblioteca JNI (`cdylib`) para la aplicación de Android: el mismo núcleo, sin montaje |
| `multidrive-cli` | Binario `multidrive` |

---

## La idea en una frase

Si RAID convirtió varios discos poco fiables en un volumen fiable, MultiDrive hace lo mismo
con varias cuentas de nube poco fiables (y con cuotas): **ningún proveedor tiene tus datos
completos, ninguno puede leerlos, y la caída de uno no te deja sin volumen.**

## Qué resuelve

| Problema | Respuesta de MultiDrive |
|---|---|
| Tengo cuentas sueltas en varios sitios | Un solo volumen sobre todas, cifrado y con RAID |
| Si el proveedor me suspende la cuenta, lo pierdo todo | Paridad *k de n*: sobrevive a la pérdida de *m* proveedores |
| El proveedor puede leer mis archivos | Cifrado en cliente (XChaCha20-Poly1305), nombres incluidos |
| Vendor lock-in | Añade o retira proveedores eligiendo qué priorizar, sin dejar de poder leer el volumen. **Retirar funciona aunque la cuenta ya no responda** |

**Sobre el espacio, con todas las letras: manda el proveedor con menos sitio.** Los trozos se
reparten a partes iguales entre todos, así que cuando el más pequeño se llena no cabe nada más
aunque a los otros les sobre. Con 15 GB, 5 GB y 1 TB el volumen sale de **5 GB** en espejo y
**15 GB** en unión, no de 1,02 TB. Esto decía «un solo volumen agregado», que se leía como que
las capacidades se suman; no se suman. Para eso está `lineal`, que coloca cada trozo entero en un proveedor en vez de repartirlo y
sí suma: con esos mismos números da 1,02 TB.

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
- [docs/ANDROID.md](docs/ANDROID.md) — la aplicación de Android: cómo se construye y qué se midió en un teléfono
- [docs/SERVICIO.md](docs/SERVICIO.md) — montar como unidad de red sin tener una consola abierta
- [docs/FIRMA.md](docs/FIRMA.md) — por qué el instalador todavía no va firmado, y qué haría falta

## Créditos

**WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos**
— <https://github.com/winfsp/winfsp>

WinFsp pone el controlador que hace que el volumen aparezca como una unidad de Windows.
MultiDrive lo usa al amparo de su excepción para software libre, y la cumple: se distribuye
bajo AGPL-3.0, lleva este aviso aquí, en [packaging/windows/AVISOS.txt](packaging/windows/AVISOS.txt)
—que se instala junto al programa— y en la propia orden de montaje, y no va con ningún
componente propietario.

## ¿Te gusta el proyecto? Apóyame a seguir creando

MultiDrive es software libre bajo AGPL-3.0: no hay versión de pago, ni funciones guardadas
detrás de una suscripción, ni anuncios. Si te resulta útil y quieres que siga creciendo,
puedes echar una mano.

| | |
|---|:--|
| <a href="https://ko-fi.com/luismora4all"><img src="https://ko-fi.com/img/githubbutton_sm.svg" alt="Invítame un café en Ko-fi" height="36"></a> | Se abre en Ko-fi, que es quien gestiona el pago. |
| <img src="https://multidrive.net/apoyo/binance-pay.png" width="150" alt="Código QR de Binance Pay para el alias LuisMora78"> | **Binance Pay** — escanéalo desde la aplicación de Binance, o busca el alias `LuisMora78`. |
| <img src="https://multidrive.net/apoyo/monero.png" width="150" alt="Código QR con la dirección de Monero del proyecto"> | **Monero (XMR)** — escanea el código, o copia la dirección de aquí abajo. |

```
426WwUxyLAbEAvzxcaLgofXf9xu4E7qEagGGdqPHkbDddHk36XdpGFf9U1Kc8N5DuAAzAYN5PStwtLj2zhKcuNm1KLH2bxc
```

Apoyar es del todo opcional y no desbloquea nada: lo que hay, lo hay para todo el mundo igual.
Lo que sí hace es pagar el servidor y dar tiempo para seguir.

## Licencia

[AGPL-3.0](LICENSE) — ver [docs/VIABILIDAD.md](docs/VIABILIDAD.md#licencia) para el razonamiento.
