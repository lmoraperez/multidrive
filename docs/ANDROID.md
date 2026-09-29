# La aplicación de Android

Qué hay que construir, qué está ya comprobado y qué decisiones quedan tomadas antes de escribir
código. El análisis de por qué Android no es un tercer objetivo de montaje está en
[ROADMAP.md](ROADMAP.md#móvil-android); aquí se da por sabido y se pasa al cómo.

## Lo que ya está medido, no supuesto

| Qué | Resultado | Cuándo |
|---|---|---|
| `crypto`, `raid`, `backend` y `core` cruzan a `aarch64-linux-android` | sí, con NDK r30 | 29-09-2026 |
| El binario **enlaza**, no solo compila | ELF aarch64 de 17 MiB | 29-09-2026 |
| Argon2id en un TECNO POVA 5 (Android 14) | **95 ms** de mediana, contra 22 ms en el escritorio | 29-09-2026 |
| HTTPS desde un binario sin JVM | **entra en pánico**, ver más abajo | 29-09-2026 |

Los 95 ms son la cifra que evita un problema que no tendría arreglo: los parámetros de
Argon2id van ligados al volumen, así que si el móvil hubiera necesitado otros, un volumen
creado en el escritorio se abriría lento en el móvil **para siempre**. No es el caso, y por eso
no hay que decidir nada al respecto.

La línea de órdenes no cruza, y está bien: arrastra `fuser`. FUSE no existe para Android.

## El proyecto de Android

Vive en [`android/`](../android), **aparte del workspace de Rust**: son dos sistemas de
construcción que no se entienden entre sí, y mezclarlos obligaría a quien solo quiere compilar
el escritorio a tener instalado un SDK que no necesita.

El puente entre los dos es [`packaging/android/compilar.ps1`](../packaging/android/compilar.ps1),
que construye el `.so` con cargo y lo deja en `android/app/src/main/jniLibs/arm64-v8a/`.

```powershell
$env:ANDROID_NDK_HOME = "C:\Users\Luis\Android\android-ndk-r30"
.\packaging\android\compilar.ps1          # el .so
cd android; .\gradlew.bat assembleDebug   # el APK
```

`jniLibs/` no va al repositorio: es la salida de cargo copiada. Versionarla significaría que
alguien puede clonar y compilar un APK con un núcleo que no corresponde a las fuentes que tiene
delante.

El *wrapper* de Gradle sí va, y **con la huella de la distribución** en
`gradle-wrapper.properties`: es lo que hace que el Gradle que se descargue solo sea el que
esperamos y no lo que haya en esa URL ese día.

## Las herramientas, y dónde están

Todo fuera del repositorio, en `C:\Users\Luis\Android`, y todo verificado contra la huella que
publica quien lo distribuye antes de descomprimirlo. Ninguna toca el `PATH` ni el registro: se
apuntan con variables de entorno, que es lo que permite tener varias versiones y tirar una sin
consecuencias.

| Qué | Versión | Espacio | Para qué |
|---|---|---|---|
| NDK | r30 (LTS) | 2,01 GiB | compilar Rust para `aarch64-linux-android` |
| JDK Temurin | 21.0.12.1+1 | 0,32 GiB | Gradle y el plugin de Android piden 17 o más |
| SDK: plataforma | android-36 | 0,10 GiB | contra qué se compila |
| SDK: build-tools | 36.1.0 | 0,13 GiB | empaquetar el APK |
| SDK: platform-tools | 37.0.1 | 0,02 GiB | `adb` |

**2,9 GiB en total**, de los cuales dos tercios son el NDK. Gradle no está todavía: se descarga
solo la primera vez que se compile, por el *wrapper*, y añadirá unos cientos de megas más sus
cachés.

```powershell
$env:JAVA_HOME    = "C:\Users\Luis\Android\jdk-21.0.12.1+1"
$env:ANDROID_HOME = "C:\Users\Luis\Android\sdk"
$env:ANDROID_NDK_HOME = "C:\Users\Luis\Android\android-ndk-r30"
$env:PATH = "$env:JAVA_HOME\bin;$env:ANDROID_HOME\platform-tools;$env:PATH"
```

En esta máquina hay además un Java 8 en el `PATH` del sistema, de otra cosa. No se ha tocado:
poniendo `JAVA_HOME` delante, Gradle usa el 21 y el otro sigue donde estaba para quien lo
necesite.

Las licencias del SDK de Google están aceptadas en `sdk\licenses`. Se revocan borrando esa
carpeta, o el SDK entero.

## La forma de la aplicación

No es un montaje. Es un **`DocumentsProvider`** del Storage Access Framework: el volumen
aparece como origen en la app de Archivos y en el selector de ficheros del sistema, y cualquier
otra aplicación lee a través de `ContentResolver`. Es lo que hacen Nextcloud y Cryptomator,
porque en Android no hay otra cosa que hacer.

Consecuencia que conviene tener presente desde el principio: **una app que use rutas crudas no
verá nada**. No hay `open()` por ruta ni `mmap`. Lo que se entrega es un descriptor.

## El contrato, y a qué corresponde de lo nuestro

`DocumentsProvider` tiene tres métodos que hay que implementar sí o sí, y el cuarto es el que
de verdad importa:

| Método de Android | Qué devuelve | Con qué se resuelve aquí |
|---|---|---|
| `queryRoots(String[])` | un `Cursor` con las raíces | una fila: el volumen. Solo aparece si hay uno configurado |
| `queryDocument(String, String[])` | metadatos de uno | `Volume::lookup` |
| `queryChildDocuments(String, String[], String)` | los hijos de un directorio | `Volume::read_dir` |
| `openDocument(String, String, CancellationSignal)` | un `ParcelFileDescriptor` | lo de la sección siguiente |

Los `documentId` son cadenas opacas para Android. Aquí serán **la ruta dentro del volumen**,
que ya es única y estable, en vez de inventar identificadores y tener que mantener una tabla.

Los métodos de escritura —`createDocument`, `deleteDocument`, `renameDocument`,
`moveDocument`— **no se implementan de momento**. Ver «Solo lectura».

### Todos se llaman desde cualquier hilo

Android no garantiza ninguno en particular y espera que sean rápidos: mientras `queryRoots` o
`queryChildDocuments` no vuelvan, la interfaz de Archivos está esperando. Nuestro `read_dir`
contra cuatro nubes puede tardar segundos.

Hay una salida prevista por el propio Android y hay que usarla: devolver un `Cursor` con
`EXTRA_LOADING` a `true`, que hace que el selector enseñe que está cargando, y notificar
después cuando estén los datos. Sin eso, la primera impresión de la aplicación es una pantalla
congelada.

## Leer por rangos, que es lo que la hace viable

Nextcloud descarga el fichero entero a un temporal para abrirlo. **Nosotros no podemos
permitírnoslo**: el troceado es de 16 MiB y `read_at` baja solo los trozos que el rango toca,
que es lo que hace posible abrir un vídeo de 2 GiB sin bajarse 2 GiB.

Para conservar eso en Android hace falta
[`StorageManager.openProxyFileDescriptor`](https://developer.android.com/reference/android/os/storage/StorageManager),
**API 26**, que devuelve un descriptor cuyas lecturas llegan como llamadas a un
`ProxyFileDescriptorCallback`:

```java
long onGetSize()                                  // Volume::lookup → len
int  onRead(long offset, int size, byte[] data)   // Volume::read_at
int  onWrite(long offset, int size, byte[] data)  // no, de momento
void onRelease()
```

Los errores se comunican lanzando `ErrnoException`, no devolviendo códigos.

Eso fija dos cosas:

- **`minSdk` = 26.** No es negociable si se quiere lectura por rangos, y sin lectura por rangos
  la aplicación es un descargador de ficheros con más pasos.
- **Las llamadas llegan por el `Handler` que se le pasa**, así que hay que crear un
  `HandlerThread` propio. No es un hilo del sistema del que se pueda abusar, y serializa las
  llamadas de un mismo descriptor, lo cual simplifica el lado de Rust.

## El componente Kotlin de los certificados: obligatorio

Esto es lo que más cerca estuvo de ser una sorpresa cara, y por eso se comprobó pronto.

`reqwest` arrastra `rustls-platform-verifier`, que en Android busca el almacén de certificados
del sistema **a través de la JVM**. Sin inicializarlo con el `Context` de una aplicación no
falla: **aborta**.

```
thread panicked at rustls-platform-verifier-0.7.0/src/android.rs:90
Expect rustls-platform-verifier to be initialized
```

Un error se maneja, se reintenta, se le enseña al usuario. Un pánico en mitad de una petición
de red es un cierre inesperado de la aplicación. Así que:

- Hay que incluir la clase Kotlin que el crate pide y llamar a
  `rustls_platform_verifier::android::init_with_env` **al arrancar el proceso**, antes de
  cualquier petición, no en el primer uso.
- Y no hay escapatoria por configuración: `reqwest` 0.13.5 solo ofrece `rustls` y
  `rustls-no-provider`, y los dos traen el verificador de plataforma. Los certificados
  empaquetados (`webpki-roots`) no son una opción desde ahí.

## La frontera con Rust

La regla es que sea **pequeña y estable**, porque cada función que cruza es una que hay que
mantener en dos lenguajes. Todo lo que pueda vivir en Rust, vive en Rust.

```
abrir(config, frase)      -> sesión, o error
raices(sesión)            -> el volumen, o nada
mirar(sesión, ruta)       -> metadatos
listar(sesión, ruta)      -> entradas
leer(sesión, ruta, off, n)-> bytes
cerrar(sesión)
```

Seis funciones. Lo que no cruza y se queda dentro: la topología, el reparto, el cifrado, la
caché, el reensamblado de fragmentos y qué proveedor tiene qué. Kotlin no necesita saber nada de
eso, y cuanto menos sepa, menos hay que tocar cuando cambie.

**El runtime asíncrono se crea una vez** y vive en la sesión. Los métodos de Android son
síncronos y llegan desde cualquier hilo, así que el lado de Rust bloquea contra ese runtime. Es
correcto por lo mismo que en los hilos del kernel de FUSE: **no son hilos del runtime**.

## La frase de paso

No se guarda, igual que en el escritorio. Vive en la sesión, en memoria, y muere con el
proceso. Android puede matar el proceso de un `DocumentsProvider` cuando le convenga, así que
hay que aceptar que **volver a Archivos puede pedirla otra vez**, y decirlo en vez de fingir
que la sesión es eterna.

Guardarla en el llavero de Android para no volver a pedirla es exactamente la clase de decisión
que se toma a sabiendas o no se toma. De momento, no.

## Solo lectura

v1 es de **un solo escritor**. Un móvil escribiendo en un volumen que el escritorio también
escribe *es* el problema de escritura concurrente que está fuera de alcance, y no se arregla
con cuidado: se arregla con el *lease* renovable de v0.5.

Así que `createDocument`, `deleteDocument` y compañía no se implementan, y las banderas del
`Cursor` lo dicen: sin `FLAG_SUPPORTS_WRITE` ni `FLAG_DIR_SUPPORTS_CREATE`, el selector del
sistema **no ofrece** guardar ahí. Es mejor no aparecer como destino que aparecer y fallar.

## Lo que el móvil impone

| Problema | Qué obliga a hacer |
|---|---|
| Doze y límites de segundo plano | nada que subir de momento, pero el día que lo haya: `WorkManager` y servicio en primer plano |
| Datos móviles | `rs(4,2)` sube 1,5× y leer exige *k* fragmentos de *k* cuentas distintas. Política de solo-WiFi, y que se pueda cambiar |
| Batería | Argon2id una vez por sesión ya se midió: 95 ms. Reed-Solomon y XChaCha20 son por trozo, y eso sí escala con lo que se lea |

Con los ~6,5 s por operación medidos contra Drive
([VIABILIDAD.md](VIABILIDAD.md#5-medidas-reales-contra-google-drive)), sobre red móvil la
latencia manda sobre todo lo demás. La caché en disco deja de ser una optimización y pasa a ser
lo que hace usable la aplicación.

## Lo que no se copia

La app de Nextcloud resuelve los mismos problemas de integración y merece estudiarse. Pero su
repositorio es **GPLv2 con aportaciones posteriores a junio de 2016 en AGPLv3-or-later**, o sea
una mezcla, y su `DocumentsStorageProvider.java` viene del linaje de ownCloud, anterior a ese
cambio. Copiar de ahí exigiría rastrear la procedencia línea a línea.

Se aprende la forma y se escribe la nuestra. Que además es distinta: ellos hablan WebDAV contra
un servidor que ve los ficheros, y aquí hay que descifrar y recomponer trozos de varias cuentas
antes de entregar un byte.

## Por dónde se empieza

1. ~~**Cargar el núcleo desde una app vacía** y que diga la versión.~~ **Hecho el 29 de
   septiembre de 2026**, corriendo en el TECNO POVA 5:

   ```
   Nucleo cargado
   version   0.0.1
   prueba    rs:4,2: tolera perder 2, sobrecoste 1.50x
   abi       arm64-v8a, armeabi-v7a, armeabi
   ```

   La segunda línea es la que importa: está **calculada** por el motor RAID dentro del `.so`,
   no escrita. Un «hola» no habría distinguido «se cargó el núcleo» de «se cargó una
   biblioteca vacía con el nombre correcto».

   Dos cosas que no estaban previstas y se resolvieron por el camino. **Desde AGP 9.0 el
   plugin de Kotlin va dentro del de Android**: declararlo aparte no es redundante, es un
   error de compilación que dice «ya no hace falta». Y **`cargo-ndk` no se usa**: el `.so` lo
   construye `packaging/android/compilar.ps1` con cargo y tres variables de entorno, que es lo
   que `cargo-ndk` haría por debajo, y lo copia a `jniLibs`. Una dependencia menos y una capa
   menos donde mirar cuando algo falle. El día que haya que compilar cuatro arquitecturas y
   esto se vuelva incómodo, se cambia entonces.

   Ese guion comprueba además que el `.so` **exporta los símbolos `Java_`**. Sin ellos la
   aplicación compila, instala, arranca y falla al llamar, que es el peor momento para
   enterarse.
2. **Inicializar el verificador de certificados** y hacer un HTTPS cualquiera. Hasta que esto
   pase, no hay nube que valga.
3. **Abrir un volumen y listar la raíz** por JNI, enseñándolo en una lista a pelo. Aquí se sabrá
   lo que todavía no sabemos: cuánto tarda de verdad la red desde el móvil.
4. **El `DocumentsProvider`** encima de lo anterior, con `EXTRA_LOADING`.
5. **`openProxyFileDescriptor`** para leer por rangos, que es lo que la separa de un juguete.

Los tres primeros no necesitan interfaz ninguna, y el tercero contesta la pregunta que quedó
abierta el 29 de septiembre.
