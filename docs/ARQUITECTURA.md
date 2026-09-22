# Arquitectura

## Vista general

```
        Aplicaciones del usuario
                  │
     ┌────────────┴────────────┐
     │  Capa de montaje        │   Linux: FUSE (fuser)
     │                         │   Windows: WinFsp
     └────────────┬────────────┘
                  │
     ┌────────────┴────────────┐
     │  VFS  (árbol de inodos) │  ← SQLite: inodos, mapa de trozos, versiones
     └────────────┬────────────┘
                  │
     ┌────────────┴────────────┐
     │  Caché local en disco   │  ← lectura + escritura diferida (cola de subida)
     └────────────┬────────────┘
                  │
     ┌────────────┴────────────┐
     │  Motor RAID             │  ← Reed-Solomon k+m / espejo / concat
     └────────────┬────────────┘
                  │
     ┌────────────┴────────────┐
     │  Cifrado                │  ← XChaCha20-Poly1305 por trozo, Argon2id
     └────────────┬────────────┘
                  │
     ┌────────────┴────────────┐
     │  Abstracción de backend │  ← OpenDAL / rclone
     └──┬────┬────┬────┬───────┘
        │    │    │    │
       B2   R2  Nextcloud  Drive …
```

La capa de montaje es el único punto realmente atado al sistema operativo: el resto de la
pila no tiene `cfg` de plataforma. Android queda fuera de este esquema porque FUSE exige root
allí, y la alternativa (Storage Access Framework) no es un montaje POSIX sino una fachada con
otra semántica. El análisis está en [ROADMAP.md](ROADMAP.md#móvil-android).

## Decisiones de diseño

### Trozos (chunks), no archivos
Cada archivo se corta en trozos de tamaño fijo (por defecto **16 MiB**). Cada trozo se cifra,
se codifica en *k+m* fragmentos y cada fragmento va a un proveedor distinto.

Consecuencia que hay que asumir: **los datos dejan de ser visibles desde la web del
proveedor.** Se ven *blobs* opacos. Es el precio inevitable del RAID y del cifrado real.

- 16 MiB es el compromiso: suficientemente grande para no agotar las cuotas de API,
  suficientemente pequeño para que leer 1 MB no baje 500 MB.

### Copy-on-write, direccionado por contenido
Los trozos son **inmutables** y su nombre es su hash (BLAKE3). Modificar un archivo escribe
trozos nuevos y cambia atómicamente el puntero del manifiesto. Esto da gratis:
deduplicación, instantáneas, y compatibilidad con la consistencia eventual de los object
stores (que no tienen escritura en sitio ni `rename` atómico).

### Topologías

| Modo | Fórmula | Sobrevive a | Sobrecoste |
|---|---|---|---|
| `concat` | 1 de 1 | nada | 1,0× |
| `stripe(n)` | n de n | **nada** | 1,0× |
| `mirror` | 1 de n | n−1 caídas | n× |
| `rs(k,m)` | k de k+m | m caídas | (k+m)/k × |

Ejemplo práctico: `rs(4,2)` con 6 proveedores → sobrevive a que 2 te suspendan la cuenta,
con 1,5× de sobrecoste de espacio.

`stripe` es lo que en un disco se llama RAID 0: toda la capacidad sumada y el ancho de banda
repartido entre los proveedores, a cambio de que **perder uno solo pierda el volumen entero**.
No es `rs(k,0)` porque eso no existe —Reed-Solomon necesita al menos un fragmento de
paridad—, y tenerlo como topología propia hace que «sin redundancia» se lea en el nombre en
vez de esconderse en un cero.

### Configurar proveedores

Un proveedor se describe con variables `LIBREDISK_<RANURA>_<CAMPO>`, que se leen del entorno
o de un fichero de configuración; el entorno gana, porque quien exporta una variable está
diciendo algo sobre *esta* ejecución.

**De cada tipo caben varios.** La ranura lleva un número detrás: `GDRIVE_*` es una cuenta de
Drive y `GDRIVE2_*` es otra distinta. No es un adorno: sin ello caben cuatro nubes como
mucho, una por servicio, y un `rs(4,2)` pide seis. La promesa de repartir entre cuentas —e
idealmente entre empresas— distintas no se puede cumplir con una sola cuenta por servicio.

Numeradas y no separadas por comas porque una ruta de Windows lleva `:` y puede llevar `,`,
así que cualquier separador sería ambiguo; y porque el nombre va pegado a la variable, que es
lo que evita el fallo silencioso de reordenar una lista y que cada fragmento se lea del sitio
equivocado.

**El orden de los proveedores es el índice de cada fragmento.** Lo fija el catálogo, y dentro
de cada tipo va la ranura sin número antes que las numeradas. Añadir un proveedor mueve a los
que vienen detrás, y eso no corrompe nada: `Header::check_backends` se niega a abrir un
volumen cuyos proveedores no casen con los de su cabecera. Lo que arregla el desajuste es
`libredisk providers add`, que reescribe la cabecera moviendo los fragmentos que haga falta.

#### Un solo catálogo

Qué tipos hay y qué campos pide cada uno vive en un sitio: `libredisk-backend::catalogo`. De
ahí salen los formularios de la ventana, la ayuda de `libredisk config ver` y la validación
al guardar. Antes eran tres copias de la misma lista, y una copia de una lista es una lista
que acaba discrepando: al numerar todos los tipos, el texto de ayuda habría seguido diciendo
que solo se numeran las carpetas locales.

Lo que el catálogo deliberadamente **no** ofrece es el token de acceso suelto. `live` lo
sigue aceptando para no romper a quien ya lo tenga escrito, pero caduca en una hora: un
volumen configurado con él deja de abrirse esa misma tarde. Ofrecerlo en un formulario sería
ofrecer una trampa.

#### Conectar una cuenta

OneDrive y Google Drive no se configuran tecleando credenciales: se pulsa conectar, se abre
el navegador en la página del proveedor, el usuario entra en su cuenta y da permiso. Es el
flujo de código de autorización con redirección a `localhost`, el que usan las aplicaciones
de escritorio:

1. LibreDisk abre un servidor en `127.0.0.1`, con el puerto que le dé el sistema. Los dos
   proveedores admiten cualquier puerto de loopback sin declararlo por adelantado, justamente
   para esto; fijar uno sería fallar el día que estuviera ocupado.
2. Manda el navegador a la página del proveedor diciéndole que al terminar redirija a ese
   puerto.
3. **Las credenciales del usuario no pasan por LibreDisk.** Van del navegador al proveedor.
   Aquí solo llega un código de un solo uso.
4. Ese código se canjea por un token de refresco, que es lo único que se guarda.

Se manda un reto **PKCE** aunque la aplicación lleve secreto y por tanto no sea obligatorio.
El motivo es concreto: el redirector es `localhost`, así que cualquier otro programa de la
máquina puede quedarse escuchando en un puerto y recoger el código. Sin PKCE le bastaría con
eso; con PKCE, el código no sirve sin un valor que solo conoce este proceso y que nunca viaja
por el navegador. Al volver se comprueba también el `state`, que es lo que impide que un
programa ajeno empuje un código suyo al puerto y consiga que se conecte *su* cuenta.

El token de refresco **no pasa por la página**: se queda en el estado del proceso hasta que
se guarda, y se borra de memoria en cuanto está en el fichero. Que la página sea nuestra y
corra en el mismo proceso no es motivo para pasear por ella acceso permanente a una cuenta.

A Google se le pide `drive.file`, o sea **solo los ficheros que LibreDisk cree**, que es todo
lo que necesita: crea su propia carpeta y todo lo que hay dentro. Pedir el Drive entero sería
pedir permiso sobre las fotos del usuario para guardar en una carpeta, y además Google lo
clasifica como ámbito restringido, lo que obliga a una evaluación de seguridad anual de
terceros. El ámbito de OneDrive no se elige: es `offline_access Files.ReadWrite`, **el mismo
que OpenDAL usa al renovar**, y pedir aquí otro daría un token que deja de valer en la
primera renovación.

Lo que falta para que esto sea de verdad un botón y nada más: una aplicación de LibreDisk
registrada en Google y en Azure. Mientras no la haya, el ID y el secreto de *alguna*
aplicación hay que pegarlos una vez, y eso sigue siendo un paso de desarrollador.

#### Se prueba antes de guardar

Añadir un proveedor escribe, lee, compara y borra un objeto pequeño, y solo si eso sale bien
escribe la configuración. Comprueba lo que descarta los fallos de configurar —credenciales
malas, carpeta inexistente, permisos de solo lectura— y sobre todo **que lo leído sea lo
escrito**: un proveedor que acepta escrituras y devuelve otra cosa es peor que uno que falla,
porque el fallo aparece mucho más tarde y lejos. No sustituye a `libredisk check`, que son
once comprobaciones y minutos contra una nube.

#### Dónde van las credenciales

En el fichero del **usuario** (`%APPDATA%\LibreDisk`, `~/.config/libredisk`), nunca en el de
la máquina. `ProgramData` y `/etc` los lee cualquier usuario del equipo, y lo que se guarda
aquí son tokens de refresco y claves secretas, o sea acceso permanente a las cuentas. En Unix
el fichero nace con permisos `0600`, desde que nace y no después: entre crearlo y un `chmod`
posterior hay una ventana en la que es legible por todo el mundo.

Un servicio sí necesita el fichero de máquina, porque arranca como LocalSystem y no ve el
perfil de nadie. Esa es una decisión que hay que tomar a sabiendas y a mano; nada de lo que
LibreDisk escribe solo va allí.


### Cuánto espacio queda

Son **tres cifras distintas** y el sistema no las mezcla, porque mezclarlas sería inventarse
una medida:

| Cifra | De dónde sale | Qué cuesta |
|---|---|---|
| Total de la cuenta | la API de cupo del proveedor | una petición |
| Ocupado en la cuenta | la misma | (la misma) |
| Lo que ocupa **este volumen** | enumerar el prefijo de fragmentos | una enumeración |

El cupo no pasa por OpenDAL: OpenDAL abstrae objetos, y el cupo de una cuenta no es un
objeto. Cada servicio lo cuenta en su sitio —`drive/v3/about` en Google, `/me/drive` en
Graph, un `PROPFIND` con las propiedades del RFC 4331 en WebDAV, `statvfs` o
`GetDiskFreeSpaceEx` en un directorio local— así que hay una llamada propia por servicio.

**Un proveedor puede no tener total, y eso no es cero.** Un bucket S3 se factura por lo que
ocupa y no tiene cupo: ahí la interfaz dice «sin límite declarado» en vez de pintar una barra
vacía. Y un proveedor que no contesta sale con el motivo, no con una barra a cero, que se
leería como «aquí no hay nada guardado».

Lo que ocupa el volumen **sale de la enumeración que ya hacía `scrub`**: los seis proveedores
soportados mandan el tamaño en la misma respuesta que los nombres, así que la cifra no cuesta
ni una petición más. Por eso viaja en `ScrubReport` y no en un método aparte: medirla por su
cuenta obligaría a enumerar dos veces para saber dos cosas que llegan juntas. Cuenta todo lo
que hay bajo el prefijo, no solo lo alcanzable desde la raíz actual, porque lo que cobra el
proveedor es lo que ocupa y las versiones viejas ocupan.

**Las dos medidas no se restan.** Medido contra OneDrive el 2026-09-22: después de escribir
20 MiB, la cuenta de uso del proveedor seguía dando la misma cifra un cuarto de hora
después. La cuenta de la nube va por detrás de lo que uno acaba de escribir, así que el tramo
de la barra que corresponde a LibreDisk se recorta a lo que el proveedor dice que hay
ocupado. Sin ese recorte, un volumen recién escrito pintaría más del 100 % de una cuenta que
está al 6 %. Detalle en [VIABILIDAD.md §9](VIABILIDAD.md#9-primer-volumen-real-sobre-dos-nubes-y-qué-dice-cada-una-de-su-espacio).


#### El tamaño que ve el sistema operativo

El Explorador y `df` solo tienen **dos** números donde aquí hay tres, así que declaran el de
las cuentas: total y libre. Lo ocupado que enseña el Explorador es, por tanto, *lo ocupado en
las cuentas* —fotos del usuario incluidas—, no lo que ocupa este volumen. El desglose de las
tres cifras está en la ventana y en `libredisk espacio`.

Un volumen no tiene tamaño propio: lo tiene prestado, y la topología decide cuánto de ese
préstamo se aprovecha. La regla cabe en una línea: **manda el proveedor más apretado**, y lo
que se saque de él se multiplica por `shards_needed`, que es 1 en `mirror` y en `concat`, `k`
en `rs(k,m)` y `n` en `stripe(n)`. Es la misma función que usa el motor para leer: son la
misma cantidad mirada desde los dos lados.

Medido el 2026-09-22 sobre el volumen real `mirror:2` de OneDrive (15,0 GiB) y Google Drive
(17,0 GiB): Windows enseña **15,00 GiB de tamaño y 3,46 GiB libres**, porque Drive está al
79 %. Antes enseñaba 7,9 EB.

Ese 7,9 EB era `u64::MAX / 2`, y estaba puesto con un comentario que decía que no había forma
honesta de dar un tamaño. Lo era mientras no se supiera el cupo. Lo que impedía preguntarlo
sigue siendo cierto —el Explorador pide esto en cada repintado y `df` en cada ejecución, y una
petición por nube en cada uno dejaría la ventana pensando—, así que **la medida no se hace en
el camino que atiende al sistema**: una tarea de fondo la refresca cada cinco minutos y deja
el resultado en dos enteros atómicos que el sistema lee sin bloquear y sin red.

Mientras no hay medida —los primeros segundos tras montar, o si ningún proveedor declara
cupo— se vuelve al número enorme. Es deliberado: un tamaño claramente falso es mejor que uno
de cero, porque cero significa «disco lleno» y ningún programa volvería a intentar escribir.

### Metadatos

**La verdad vive en los proveedores, no en el disco local.** El árbol es una estructura
*copy-on-write* de nodos inmutables, y cada nodo es un trozo más: pasa por el mismo
`ChunkStore` que los datos, así que hereda cifrado, reparto RAID, direccionamiento por
contenido y deduplicación sin una línea de código propia. Los nombres de fichero quedan
cifrados porque el nodo entero lo está.

Modificar algo reescribe los nodos desde la hoja hasta la raíz (*path copying*) y termina
intercambiando un único puntero. Eso es lo que da atomicidad sobre almacenes que no tienen ni
renombrado atómico ni escritura en sitio.

**El puntero a la raíz es un objeto suelto en una clave conocida**, no un registro dentro de
una base de datos. Es deliberado: cualquier lector que tenga la frase de paso arranca leyendo
esa única clave, sin necesidad de entender ningún esquema. Es el único objeto mutable del
sistema y, por tanto, el único sitio donde vive la atomicidad.

Un índice local (SQLite o `redb`) es una **caché** de ese árbol, no su origen. Sirve para no
pagar una petición por nivel en cada `lookup` y para agrupar escrituras, que es obligatorio:
con los costes medidos en [VIABILIDAD.md §7](VIABILIDAD.md#7-enumerar-qué-cuesta-list), un
cambio de metadatos que reescriba cuatro niveles sobre seis proveedores son 24 subidas, o sea
segundos. **El escritor de metadatos no puede ser síncrono.**

Tres cosas distintas se apoyan en que la verdad esté fuera:

- Perder el disco local no puede significar perder el volumen.
- `libredisk rescue` reconstruye el árbol escaneando los proveedores aunque no quede ninguna
  copia local del puntero de raíz. **Implementado**, y sin necesitar la cabecera
  autodescriptiva por trozo que este documento preveía: la longitud sellada se deduce, porque
  `split` rellena hasta un múltiplo de *k* y solo hay *k* candidatos, y el tag de
  XChaCha20-Poly1305 dice cuál es el bueno sin ambigüedad. Dos verificaciones independientes,
  cero bytes de formato añadidos.
- Un lector de solo lectura —el caso de [Android](ROADMAP.md#móvil-android)— necesita
  exactamente una cosa: el id de la raíz. No replica una base ni reproduce un diario, y baja
  solo la rama del árbol que el usuario abre, que es justo el patrón de acceso de SAF. Si los
  metadatos vivieran solo en una base local, un lector externo tendría que replicarla entera.

Android **no es el motivo** de esta decisión —lo son `rescue` y la falta de renombrado
atómico— pero pide exactamente la misma propiedad. Conviene saber que no cuesta nada extra.

### Recuperación ante desastre

Si se rompe la máquina donde vive el volumen, **no se pierde nada**, y no por suerte: es la
consecuencia directa de que la verdad esté en los proveedores. Hoy el programa no guarda
ningún estado local —ni índice, ni caché, ni base de datos—, así que la máquina es
desechable.

Para recuperar hacen falta tres cosas, y solo tres:

| | Se puede volver a conseguir |
|---|---|
| La **frase de paso** | **No.** No se guarda en ninguna parte. Es el único punto sin recuperación |
| Acceso a las **cuentas** de los proveedores | Sí: los tokens se regeneran rehaciendo el OAuth |
| El software | Sí, está en el repositorio |

De la máquina muerta no hace falta nada. La cabecera (sal, topología y orden de proveedores)
y el puntero de raíz viven replicados en los propios proveedores.

Los riesgos reales, por gravedad:

1. **La frase de paso.** Trátala como la semilla de una cartera: gestor de contraseñas y
   copia en papel en otro sitio físico.
2. **La cabecera.** Lleva la sal, y sin sal no hay clave con la que descifrar nada. Va
   replicada en todos los proveedores, pero `libredisk header export` la vuelca a un fichero
   para guardarla junto a la frase. Sin ella los trozos son ilegibles para siempre.
3. **Perder una cuenta de nube.** Con `rs(k,m)` sobrevives a perder *m*. Por eso los
   proveedores deben estar en cuentas, e idealmente en empresas, distintas.
4. **Usar un directorio local como proveedor.** Muere con la máquina y consume presupuesto de
   redundancia.

Si se pierde el puntero de raíz pero queda la cabecera, `libredisk rescue` enumera los
trozos, los descifra y encuentra los directorios a los que no apunta nadie: son las raíces
candidatas. Como aquí nada se sobrescribe, aparecen también las de versiones anteriores, y el
rescate las describe por su contenido —cuántos ficheros, qué rutas— para poder elegir sin
mirar hashes. Adoptar una no borra las demás.

> **Esto se degrada en cuanto haya caché de escritura diferida.** Ese día existirá una ventana
> real en la que hay datos en la máquina que todavía no están en las nubes, y ahí sí habría
> pérdida. El índice local tiene que seguir siendo una caché, nunca el origen, y la interfaz
> tiene que distinguir "guardado localmente" de "a salvo en la nube".

> **La escritura diferida rompe esto a propósito, y hay que saberlo.** Entre `put --diferido`
> y `sync` hay datos que **solo existen en esta máquina**: si muere en esa ventana, se
> pierden. No es evitable —es la definición de diferir— pero sí decible, y por eso
> `libredisk status` distingue "pendiente" de "a salvo en los proveedores" y `put --diferido`
> lo avisa en mayúsculas.

### Concurrencia
**v1 = un solo escritor, varios lectores.** El bloqueo distribuido entre máquinas es un
problema difícil y no pertenece a un MVP. Se materializa como un *lease* renovable
depositado en uno de los proveedores.

### Cifrado
- Clave maestra derivada con **Argon2id** de una frase de paso.
- Cada trozo: **XChaCha20-Poly1305**, nonce aleatorio de 24 bytes.
- Los **nombres de archivo también se cifran**; el proveedor no ve estructura ni nombres.
- Sin puerta trasera y sin recuperación: si pierdes la frase de paso, pierdes el volumen.
  Debe decirse con todas las letras en la UI al crear el volumen.

## Estructura del repositorio (prevista)

```
crates/
  libredisk-core/      VFS, inodos, manifiestos, COW
  libredisk-raid/      Reed-Solomon, espejo, concat, reconstrucción
  libredisk-crypto/    Argon2id, XChaCha20-Poly1305, gestión de claves
  libredisk-backend/   trait Backend + adaptadores (OpenDAL)
  libredisk-cache/     caché en disco, cola de subida
  libredisk-mount/     fuser (Linux) / winfsp (Windows)
  libredisk-cli/       binario `libredisk`
docs/
```

## CLI prevista

```
libredisk volume create mivolumen --layout rs:4,2
libredisk backend add b2     --type s3 --endpoint ...
libredisk backend add nube   --type webdav --url https://nextcloud...
libredisk mount mivolumen /mnt/libredisk
libredisk status                 # salud, cola de subida, coste estimado
libredisk scrub mivolumen        # verificar integridad
libredisk rebuild mivolumen      # reconstruir tras perder un proveedor
libredisk rescue --from-backends # reconstruir metadatos desde cero
```
