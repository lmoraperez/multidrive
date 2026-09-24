# Análisis de viabilidad

Veredicto corto: **técnicamente viable y ya demostrado por terceros; el riesgo real no es
la ingeniería, son los Términos de Servicio de los proveedores y el coste de egreso.**

---

## 1. Prior art — esto ya se ha hecho (parcialmente)

Nada de esto invalida el proyecto, pero hay que conocerlo antes de escribir una línea:

| Proyecto | Qué hace | Qué le falta |
|---|---|---|
| **rclone** (`union` + `crypt` + `chunker` + `mount`) | ~70 proveedores, cifrado, montaje FUSE/WinFsp | Sin paridad: `union` es JBOD. Pierdes un remote, pierdes esos archivos |
| **StableBit CloudDrive** | Exactamente esta idea, a nivel de bloque | Comercial, cerrado, solo Windows |
| **JuiceFS** (Apache-2.0) | POSIX sobre object store + motor de metadatos | Un solo backend de datos; no reparte entre proveedores |
| **Tahoe-LAFS** | *Erasure coding* k-de-n + cifrado, desde 2007 | No se monta como unidad usable; proyecto casi dormido |
| **mergerfs + SnapRAID** | Unión + paridad | Solo discos locales |
| **Storj** | Erasure coding sobre nodos | Es un proveedor, no un agregador de tus cuentas |
| **Cryptomator** | Cifrado de conocimiento cero sobre una nube | Solo cifra, no agrega ni da paridad |

**El hueco real que ocupa LibreDisk:** nadie ofrece *libre + multiproveedor + paridad real
+ cifrado en cliente + multiplataforma con GUI*. Eso sí es un producto que no existe.

## 2. Los siete problemas duros

### 2.1 Términos de Servicio — el riesgo número uno
Más grave que cualquier problema técnico. Google Drive, Dropbox y OneDrive de consumo
desaconsejan o prohíben este patrón de uso, y subir cientos de miles de *blobs* cifrados
opacos es justo lo que dispara suspensiones de cuenta. Precedentes: la expulsión masiva de
clientes tipo rclone de Amazon Cloud Drive (2017) y el fin del almacenamiento ilimitado de
Google Workspace (2023).

**Mitigación (decisión de diseño, no un parche):** los proveedores de primera clase son los
que *explícitamente permiten* este uso — compatibles con S3 (Backblaze B2, Cloudflare R2,
Wasabi, Storj, IDrive e2, Scaleway) y autoalojados (Nextcloud/WebDAV, MinIO, SFTP). Drive,
Dropbox y OneDrive se soportan, pero marcados como "bajo tu propio riesgo" en la UI.

### 2.2 Límites de API y cuotas
Google Drive limita consultas por 100 s y estrangula subidas. Un sistema de archivos hace
miles de operaciones por minuto. Obliga a: trozos grandes (8–32 MiB), *token bucket* por
proveedor, reintentos con *backoff* exponencial y coalescencia de operaciones.

> Medido: seis escrituras simultáneas contra Drive no provocaron ni un 429, pero cada
> operación cuesta ~2,3 s por sí sola, y ~6,5 s si la ruta no existe. El cuello no fue la
> cuota, fue la latencia. Ver [§5](#5-medidas-reales-contra-google-drive).

### 2.3 Coste de egreso — hay que modelarlo, no ignorarlo
Una lectura normal solo baja *k* fragmentos, bien. Pero **una reconstrucción baja el array
entero**. Reconstruir 2 TB en B2 (0,01 USD/GB, con 3× gratis) son ~20 USD reales. Wasabi no
cobra egreso pero impone 90 días mínimos de retención. **El estimador de coste mensual debe
ser parte del producto**, visible antes de elegir la topología.

### 2.4 Los object stores no son discos
No hay `rename` atómico, ni escritura en sitio, ni `fsync`. Intentar emular bloques POSIX
es el camino al desastre. La única arquitectura sensata es **log-structured, copy-on-write,
direccionado por contenido**: trozos inmutables + cambio atómico de un puntero de metadatos.
Decisión tomada desde el día 1, no refactorizable después.

### 2.5 Perder los metadatos = perderlo todo
El árbol de inodos y el mapa de trozos es el punto único de fallo. Requiere: réplica del
diario en *n* proveedores, y trozos **autodescriptivos** (cabecera con el ID del manifiesto)
para que exista una herramienta de rescate capaz de reconstruir el árbol sin la base local.

### 2.6 Caché y durabilidad percibida
La caché local en disco es **obligatoria**, no opcional. Y la escritura diferida implica que
"archivo guardado" ≠ "archivo a salvo en la nube". Eso hay que exponerlo en la UI (cola de
subida, indicador de sincronización) o generará pérdida de datos y desconfianza.

### 2.7 Windows: firma de código
WinFsp evita tener que escribir y firmar un driver de kernel propio (el suyo ya va firmado),
pero el instalador necesita un certificado EV (~300–400 USD/año) para no chocar con
SmartScreen. Presupuestarlo.

## 3. Pila tecnológica recomendada

Dos caminos razonables:

| | **Rust** (recomendado) | **Go** (MVP más rápido) |
|---|---|---|
| Proveedores | Apache OpenDAL (~40 backends) | Importar los backends de rclone |
| Montaje | `fuser` (Linux) + `winfsp-rs` (Windows) | `cgofuse` (FUSE + WinFsp + macFUSE) |
| Paridad | `reed-solomon-erasure` / `reed-solomon-simd` | `klauspost/reedsolomon` (SIMD, excelente) |
| Metadatos | SQLite o `redb` | SQLite |
| GUI | Tauri | Wails |
| Ventaja | Base limpia a 5 años, sin GC, binario pequeño | Semanas en lugar de meses hasta el MVP |

**Recomendación:** Rust + OpenDAL + fuser/winfsp-rs. Si el objetivo es enseñar algo que
funcione en 6 semanas, Go reutilizando rclone es la apuesta pragmática.

## 4. Alcance realista del MVP (v0.1)

Recortar sin piedad:

- Proveedores: **solo compatibles con S3 + WebDAV + directorio local** (para pruebas)
- Topologías: JBOD, espejo (RAID1), paridad Reed-Solomon *k+m* (RAID5/6)
- Trozos inmutables, direccionados por contenido, cifrados
- Metadatos en SQLite + diario replicado a la nube
- **Solo montaje FUSE en Linux, solo CLI**

Windows, GUI y Google/Dropbox/OneDrive vienen después. Ver [ROADMAP.md](ROADMAP.md).

## 5. Medidas reales contra Google Drive

Cinco pasadas de la suite de conformidad (`libredisk check --pasadas 5`) contra una cuenta
real, el 2026-09-18. **Las nueve comprobaciones pasan en las cinco**: Drive cumple el
contrato de `Backend`.

| Comprobación | Mediana | min | max | Disp. | Ops | Por op |
|---|---:|---:|---:|---:|---:|---:|
| `ida_y_vuelta` (64 KiB) | 7 010 ms | 6 828 | 9 789 | 42 % | 2 | ~3 505 ms |
| `sobrescritura` | 7 668 ms | 6 736 | 8 714 | 26 % | 3 | ~2 556 ms |
| `not_found_distinguible` | 6 671 ms | 6 142 | 7 222 | 16 % | 1 | **6 671 ms** |
| `borrar_inexistente` | 2 443 ms | 2 381 | 2 994 | 25 % | 1 | **2 443 ms** |
| `exists_refleja_el_borrado` | 9 113 ms | 8 548 | 12 043 | 38 % | 5 | ~1 823 ms |
| `integridad_binaria` (4 KiB) | 5 302 ms | 5 215 | 5 483 | 5 % | 2 | ~2 651 ms |
| `clave_con_forma_de_fragmento` | 8 197 ms | 8 075 | 8 568 | 6 % | 2 | ~4 099 ms |
| `fragmento_de_4_mib` | 7 808 ms | 7 554 | 8 328 | 10 % | 2 | ~3 904 ms |
| `seis_escrituras_en_paralelo` | 5 175 ms | 4 828 | 6 047 | 24 % | 6 en paralelo | ~863 ms |

### El coste es por operación, no por byte

`borrar_inexistente` es el instrumento más limpio: una sola operación, cero bytes. Mediana
**2 443 ms** con un mínimo de 2 381 en cinco pasadas. Ese suelo es sólido.

- 4 KiB de ida y vuelta: **5 302 ms**
- 4 MiB de ida y vuelta: **7 808 ms**

Mil veces más datos por 1,5 veces el tiempo, proporción que se ha repetido en las tres
tandas de medidas. Drive no cobra en ancho de banda, cobra en *round trips*: OpenDAL localiza
cada ruta buscándola en Drive antes de tocarla, y esa búsqueda domina. El coste marginal de
mover datos, una vez pagada la búsqueda, sale a ~1,25 s por cada 4 MiB y sentido: unos
3,2 MB/s de transferencia real.

### La consulta que falla es la cara, y es estable

**Un `get` de algo que no existe cuesta 6 671 ms: 2,7 veces el suelo, y 3,7 veces un `exists`
que sí resuelve (1 823 ms).** Con solo un 16 % de dispersión en cinco pasadas, no es un
artefacto: buscar y no encontrar es el peor caso de Drive.

Es exactamente el perfil de trabajo de `scrub` y `repair`, que existen para preguntar por
fragmentos que quizá falten. La operación que más va a usar el mantenimiento del volumen es
la más lenta del proveedor.

### La dispersión es cola, no ruido

El dato que ordena todo lo demás. Donde la dispersión es alta, **el mínimo está pegado a la
mediana y el máximo muy lejos**:

| Comprobación | Mediana | Δ al mínimo | Δ al máximo |
|---|---:|---:|---:|
| `ida_y_vuelta` | 7 010 ms | −182 ms | **+2 779 ms** |
| `exists_refleja_el_borrado` | 9 113 ms | −565 ms | **+2 930 ms** |

Eso no es varianza: es **una petición lenta de cada cinco**. Y el exceso, ~2,8 s, coincide
con el suelo por operación (2 443 ms), lo que sugiere que la cola es *una operación de más*
—un reintento, o una resolución de ruta repetida— y no una red irregular. Es una hipótesis
que habría que confirmar instrumentando las peticiones HTTP.

Las otras siete comprobaciones se mueven entre el 5 % y el 26 %, con mínimos y máximos
repartidos. Dicho de otro modo: **las medianas son fiables**, y el problema no es medir, es
que una de cada cinco operaciones tarda el doble.

Consecuencia para el montaje: el volumen no se percibirá lento de forma uniforme, sino con
**tirones**. Cualquier tiempo de espera debe fijarse muy por encima de la mediana —un límite
ingenuo de 5 s cortaría peticiones legítimas— y la caché tiene que absorber la cola, no solo
la latencia media.

### Consecuencias para el diseño

1. **`scrub` no puede ser un `exists` por fragmento.** Un volumen de 1 TB en trozos de 16 MiB
   con `rs(4,2)` son ~393 000 objetos. A 1,8 s el caso bueno son **8 días** en serie; a 6,7 s
   el caso malo —el que importa, porque `scrub` existe para encontrar lo que falta— son
   **30 días**. Ni con 20 operaciones en paralelo baja de un día. Hay que **enumerar** el
   proveedor y comparar contra el manifiesto. El `trait Backend` no tiene `list` a propósito;
   esta medida es el argumento para añadirlo.

2. **Los trozos de 16 MiB se quedan cortos para Drive.** [ARQUITECTURA.md](ARQUITECTURA.md)
   los justifica como compromiso. Con un suelo de ~2,4 s por operación, leer 1 MiB cuesta
   prácticamente lo mismo que leer 16. El tamaño de trozo debería ser **por proveedor**.

3. **El paralelismo funciona y hay que usarlo a fondo.** Seis escrituras simultáneas: 5 175 ms
   frente a ~2,5 s una sola. Seis veces el trabajo por poco más del doble de tiempo, sin un
   solo 429 en cinco pasadas. El reparto RAID entre proveedores sale casi gratis; falta
   encadenar también *trozos* en vuelo, no solo fragmentos.

### Qué significa para el volumen montado

Un trozo de 16 MiB en `rs(4,2)` son 4 fragmentos de 4 MiB que se bajan en paralelo:
aproximadamente **5 s por trozo**, o unos 3 MB/s por flujo secuencial. Leer un archivo de
1 GB de forma ingenua serían ~6 minutos.

Esto convierte la caché local con lectura anticipada en **un requisito previo al montaje
FUSE**, no en una mejora posterior: sin varios trozos en vuelo, el montaje sería inusable
para el caso de uso que el propio README promete.

### Cuánto fiarse de estos números

Cinco pasadas, una cuenta, una red, en una tarde. Sirven para ordenar el trabajo y para
descartar diseños, no como cifras publicables: falta medir desde otra red, en otro momento
del día, y sobre un `rs(4,2)` completo en vez de operaciones sueltas.

Un detalle de método, por si alguien repite las medidas: hasta el commit que introdujo
`Cargas`, los datos de prueba se generaban dentro de la región cronometrada. Son ~48 ms por
4 MiB en una build de debug: un 0,6 % frente a Drive —irrelevante, las cifras de arriba
siguen siendo comparables— pero el 88 % de la medida contra un directorio local.

## 6. Medidas reales contra OneDrive

Cinco pasadas, misma suite, misma tarde del 2026-09-18, mismo servidor y misma red que las
de Drive. **Las nueve comprobaciones pasan en las cinco**: OneDrive cumple el contrato de
`Backend` y es el segundo proveedor de nube que sirve como pata de un RAID.

| Comprobación | Mediana | min | max | Disp. | Ops | Por op |
|---|---:|---:|---:|---:|---:|---:|
| `ida_y_vuelta` (64 KiB) | 2 868 ms | 2 526 | 6 942 | 154 % | 2 | ~1 434 ms |
| `sobrescritura` | 2 569 ms | 2 526 | 2 852 | 13 % | 3 | ~856 ms |
| `not_found_distinguible` | 518 ms | 478 | 584 | 20 % | 1 | **518 ms** |
| `borrar_inexistente` | 515 ms | 509 | 538 | 6 % | 1 | **515 ms** |
| `exists_refleja_el_borrado` | 2 851 ms | 2 798 | 3 052 | 9 % | 5 | ~570 ms |
| `integridad_binaria` (4 KiB) | 1 772 ms | 1 730 | 1 839 | 6 % | 2 | ~886 ms |
| `clave_con_forma_de_fragmento` | 1 891 ms | 1 865 | 2 478 | 32 % | 2 | ~946 ms |
| `fragmento_de_4_mib` | 6 065 ms | 5 660 | 8 121 | 41 % | 2 | ~3 033 ms |
| `seis_escrituras_en_paralelo` | 2 843 ms | 2 553 | 4 547 | 70 % | 6 en paralelo | ~474 ms |

### El suelo es 4,7 veces más barato que el de Drive

> **Esta cifra no se reproduce.** En tres tandas ha salido 4,7×, 1,6× y 2,5× (§8). Lo único
> constante es el signo: una operación contra OneDrive sale más barata que contra Drive. El
> resto de esta sección se deja tal y como se midió el 18.

Las dos comprobaciones de una sola operación coinciden dentro del ruido —**515 ms** el
`delete` de lo inexistente, **518 ms** el `get` que falla— y las dos son estables (6 % y
20 % de dispersión). Ese suelo es sólido, y está muy por debajo de los 2 443 ms de Drive.

`exists_refleja_el_borrado`, que son cinco operaciones, sale a ~570 ms cada una. Tres
instrumentos independientes dan el mismo número: **una operación contra OneDrive cuesta
~0,5 s**, casi cinco veces menos que contra Drive.

### El peor caso de Drive aquí no existe

Es la diferencia que más cambia el diseño. En Drive, un `get` de algo que no existe cuesta
6 671 ms: 2,7 veces su propio suelo, porque OpenDAL resuelve cada ruta buscándola y buscar
sin encontrar es el caso malo. **En OneDrive cuesta 518 ms, exactamente lo mismo que
cualquier otra operación**: Graph direcciona por ruta, no busca.

Eso rebaja la estimación de `scrub` de 30 días a **~2,4 días** para un volumen de 1 TB
(~393 000 objetos a ~0,52 s). Sigue siendo inviable en serie, así que **la conclusión de
§5 no cambia: el `trait Backend` necesita `list`**. Pero el argumento ya no depende de una
patología de Drive; se sostiene sobre el mejor de los dos proveedores medidos.

### La inversión: barato por operación, caro por byte

Drive no cobra en ancho de banda, cobra en *round trips*. Con OneDrive es al revés.

- 4 KiB de ida y vuelta: **1 772 ms**
- 4 MiB de ida y vuelta: **6 065 ms**

El salto son 4 293 ms por 8 MiB movidos (4 de subida y 4 de bajada): **~1,9 MB/s**, frente
a los ~3,2 MB/s de Drive. OneDrive es **4,7 veces más barato por operación y ~1,7 veces más
lento por byte**.

Consecuencia directa sobre [ARQUITECTURA.md](ARQUITECTURA.md): la §5 concluyó que los trozos
de 16 MiB se quedan cortos porque en Drive leer 1 MiB cuesta lo mismo que leer 16. Contra
OneDrive ese razonamiento no se sostiene —ahí los bytes sí se pagan— y subir el tamaño de
trozo empeoraría la latencia de la primera lectura. **Refuerza que el tamaño de trozo tiene
que ser por proveedor, y en direcciones opuestas.**

### El paralelismo rinde mucho menos que en Drive

Seis escrituras simultáneas de 256 KiB: 2 843 ms, o ~474 ms amortizados por escritura. Sin
un solo 429 en cinco pasadas (30 escrituras concurrentes).

Parece bueno hasta que se compara con el suelo. En Drive, seis en paralelo cuestan 863 ms
amortizados frente a un suelo de 2 443: un factor de **2,8**. En OneDrive, 474 ms frente a
un suelo de 515: un factor de **1,1**. Es coherente con lo anterior —si el límite es el
ancho de banda y no el *round trip*, abrir seis conexiones no lo mueve— pero conviene no
sobrevenderlo: **el "sequencial" de la comparación está inferido del suelo por operación, no
medido**. La suite no tiene un control de seis escrituras en serie, y ese control es lo que
convertiría esto en una medida. La dispersión del 70 % en esta comprobación pide lo mismo.

Queda además una pregunta abierta que decide el rendimiento del montaje: los 1,9 MB/s, ¿son
por conexión o por cuenta? La comprobación en paralelo mueve 1,5 MiB en total, poco para
distinguirlo. Una variante con seis cargas de 4 MiB lo respondería directamente. Si el
límite es por cuenta, bajar un trozo de 16 MiB en `rs(4,2)` son ~8,4 s —peor que los ~5 s de
Drive, pese a tener un suelo cinco veces mejor— y la caché con lectura anticipada se vuelve
todavía más obligatoria de lo que decía §5.

### Un artefacto del arnés, no del proveedor

`ida_y_vuelta` desentona: 154 % de dispersión, y una mediana de 2 868 ms cuando
`integridad_binaria` hace las mismas dos operaciones (`put` + `get`) en 1 772 ms. La
diferencia no es el tamaño —1 KiB y 4 KiB dan prácticamente lo mismo, 1 891 y 1 772 ms— sino
la posición: **`ida_y_vuelta` es la primera comprobación de cada pasada, y cada pasada
estrena prefijo**, así que su primer `put` paga la creación de la carpeta.

Es coste de montaje contabilizado como latencia de E/S, y afecta por igual a la tabla de
Drive (donde `ida_y_vuelta` también tiene la segunda dispersión más alta, 42 %). El arnés
debería crear el prefijo antes de arrancar el cronómetro. Hasta entonces, **la primera fila
de ambas tablas no es comparable con el resto**.

### Cuánto fiarse de estos números

Lo mismo que en §5, y con dos salvedades propias: la comparación con Drive es entre dos
tandas del mismo día y la misma red, lo que la hace más sólida de lo habitual, pero el token
de OneDrive venía de Graph Explorer y las medidas de ancho de banda están tomadas sobre una
única cuenta personal, que es justo donde Microsoft aplica la limitación más agresiva.

## 7. Enumerar: qué cuesta `list`

Cinco pasadas del **2026-09-21**, con la suite ya de once comprobaciones. **Las once pasan
en las cinco, en las dos nubes.**

Antes de leer una sola cifra: **estas medidas no son continuación de las de §5 y §6.** Otra
máquina, otra red, otro día y una cuenta de Drive recién creada. Son una segunda muestra
independiente, no la misma serie con más puntos. Eso las hace menos comparables y, a la vez,
más útiles: §5 y §6 decían que faltaba medir desde otra red, y esto es esa otra red.

| Comprobación | OneDrive | Drive |
|---|---:|---:|
| `listado_de_prefijo_vacio` | 1 133 ms (disp. 84 %) | 1 137 ms (disp. 31 %) |
| `listado_enumera_lo_escrito` | 10 397 ms (disp. 44 %) | 15 148 ms (disp. 30 %) |

### Una llamada a `list` no es una petición

Es la corrección más importante de esta sección, porque se dio por supuesto lo contrario.
En un almacén plano tipo S3 enumerar un prefijo se aproxima a una petición. OneDrive y Drive
son **jerárquicos**, y OpenDAL recorre el árbol nivel a nivel: paga una petición por
directorio.

`listado_enumera_lo_escrito` no mide solo eso. Escribe tres objetos y después enumera, así
que su mediana incluye tres `put` secuenciales. Descontándolos con el suelo por operación de
la tabla de abajo, el `list` en sí queda en unos **8 s en OneDrive y 11 s en Drive** para un
prefijo con tres directorios debajo: del orden de **3 a 4 s por directorio**.

### El suelo de una enumeración es idéntico en las dos nubes

`listado_de_prefijo_vacio` sí es una medida limpia: un único `list` sobre un prefijo sin nada
debajo. **1 133 ms en OneDrive y 1 137 ms en Drive.**

Que dos proveedores que difieren en todo lo demás coincidan en cuatro milisegundos es lo
bastante llamativo como para no darlo por bueno sin repetirlo.

> **Repetido en §8, y no se sostiene:** 593 ms en OneDrive y 1 522 ms en Drive. Era una
> coincidencia. El coste de enumerar sí lo pone el proveedor.

### Enumerar un proveedor entero

`ChunkId::shard_key` produce `chunks/<2 hex>/<hash>.<n>`, o sea **257 directorios** por
proveedor: `chunks/` más los 256 del segundo nivel.

| | Enumeración completa, 1 proveedor |
|---|---|
| Al suelo medido (1,1 s/directorio) | ~4,7 min |
| Al coste del caso poblado (3–4 s/directorio) | ~13–17 min |

Compárese con lo que costaba preguntar fragmento a fragmento por un volumen de 1 TB: **30
días en Drive** (§5) y **2,4 días en OneDrive** (§6). La conclusión de §5 y §6 se confirma y
se puede cerrar: `list` era necesario, y resuelve el problema.

Pero deja uno nuevo, más pequeño y de diseño: **el prefijo de dos caracteres ahora cuesta
dinero.** Existe para no acumular millones de objetos en un mismo directorio, que es lo que
degrada a los almacenes planos. En los jerárquicos el incentivo va justo al revés: cada nivel
de anidamiento es una petición más por enumeración. Es el mismo patrón que el tamaño de
trozo, y probablemente la misma respuesta: **que dependa del proveedor.**

### Qué dice esta red de las medidas del 18

Dos cosas opuestas, y conviene separarlas.

**La relación de ancho de banda parecía aguantar, y no aguanta.** Con el método de §6
—`fragmento_de_4_mib` menos `integridad_binaria`, que son 8 MiB movidos:

| | 2026-09-18 | 2026-09-21, mañana |
|---|---:|---:|
| Drive | ~3,2 MB/s | ~1,4 MB/s |
| OneDrive | ~1,9 MB/s | ~0,85 MB/s |
| **Drive / OneDrive** | **1,7×** | **1,7×** |

Esta red es unas 2,3 veces más lenta en absoluto, pero la proporción se reproduce con dos
decimales.

> **Corregido en §8.** Una tercera tanda da 0,43×: la proporción no solo cambia de tamaño,
> cambia de sentido. Dos muestras que coinciden no son una constante. Lo que ocupaba este
> hueco —que la conclusión de §6 sobre bytes era «robusta frente al cambio de red»— estaba
> apoyado en n = 2 y era un error.

**El suelo por operación no aguanta.** Y esa es la cifra sobre la que §6 apoya su conclusión
más fuerte:

| `borrar_inexistente` (1 op) | 2026-09-18 | 2026-09-21, mañana |
|---|---:|---:|
| Drive | 2 443 ms | 1 267 ms |
| OneDrive | 515 ms | 774 ms |
| **Drive / OneDrive** | **4,7×** | **1,6×** |

Drive mejoró y OneDrive empeoró. El "**4,7 veces más barato por operación**" de §6 **no se
reproduce**. En el peor caso de Drive, el `get` que falla, la ventaja pasa de 12,9× a 4,0×:
sigue existiendo, pero es otra magnitud.

> **Zanjado en §8**, aunque no como se esperaba: repetida la medida desde esta misma red y
> con esta misma cuenta, el suelo de Drive se mueve un 29 % entre dos tandas del mismo día.
> El instrumento no tiene resolución para el efecto que la hipótesis proponía.

La hipótesis más plausible está en el propio §5: OpenDAL resuelve cada ruta de Drive
**buscándola**, así que el coste depende de lo que haya en la cuenta. La cuenta de Drive del
21 se creó para esta prueba y está vacía; la del 18 no. Si es eso, el número de §6 no estaba
mal: estaba midiendo también el contenido de la cuenta, que es una variable que nadie había
declarado. **Sin verificarlo es solo una hipótesis**, y la forma de comprobarla es repetir
contra las dos cuentas desde la misma red.

### Cuánto fiarse de estos números

Menos que de los de §5 y §6, no más. Cinco pasadas, una tarde, una red doméstica y cuentas
distintas de las de entonces. Lo único que aquí se puede dar por cerrado es lo cualitativo:
las once comprobaciones pasan, enumerar cuesta una petición por directorio, y `list` convierte
el `scrub` de inviable en caro. Los milisegundos concretos son de esta tarde y de esta línea.

## 8. Tercera tanda: qué se reproduce y qué no

Cinco pasadas más, la noche del **2026-09-21**, misma máquina, misma red y **las mismas
cuentas** que las de §7, nueve horas después. Las once comprobaciones vuelven a pasar en las
cinco, en las dos nubes.

Esta tanda existe por una razón concreta: §7 dejó abierta una hipótesis y dio por cerrada una
conclusión. Se cumple lo contrario de lo que se esperaba en las dos.

| Comprobación | OneDrive | Drive |
|---|---:|---:|
| `ida_y_vuelta` (64 KiB) | 2 006 ms (disp. 265 %) | 5 741 ms (disp. 45 %) |
| `sobrescritura` | 2 640 ms (38 %) | 5 391 ms (12 %) |
| `not_found_distinguible` | 639 ms (32 %) | 4 292 ms (10 %) |
| `borrar_inexistente` | 667 ms (36 %) | 1 634 ms (42 %) |
| `exists_refleja_el_borrado` | 3 011 ms (31 %) | 6 021 ms (11 %) |
| `integridad_binaria` (4 KiB) | 1 813 ms (29 %) | 3 662 ms (31 %) |
| `clave_con_forma_de_fragmento` | 1 549 ms (109 %) | 5 828 ms (16 %) |
| `fragmento_de_4_mib` | 4 755 ms (79 %) | 10 563 ms (18 %) |
| `seis_escrituras_en_paralelo` | 1 492 ms (82 %) | 3 326 ms (21 %) |
| `listado_enumera_lo_escrito` | 6 563 ms (33 %) | 15 535 ms (10 %) |
| `listado_de_prefijo_vacio` | 593 ms (32 %) | 1 522 ms (21 %) |

### El ruido de fondo se come la hipótesis

§7 se preguntaba por qué el suelo por operación de Drive había caído de 2 443 ms a 1 267 ms,
y proponía que dependiera de cuánto hubiera en la cuenta, porque OpenDAL resuelve cada ruta
de Drive buscándola. Para comprobarlo hacía falta medir otra vez desde la misma red.

| `borrar_inexistente` (1 operación, 0 bytes) | 09-18, servidor | 09-21 mañana | 09-21 noche |
|---|---:|---:|---:|
| Drive | 2 443 ms | 1 267 ms | 1 634 ms |
| OneDrive | 515 ms | 774 ms | 667 ms |
| **Drive / OneDrive** | **4,7×** | **1,6×** | **2,5×** |

Las dos últimas columnas son **la misma cuenta, la misma máquina y la misma línea**, con nueve
horas de diferencia. Y aun así el suelo de Drive se mueve un **29 %**.

Ese es el resultado, y no es el que se buscaba: **el instrumento no tiene resolución para la
pregunta.** Una mediana de cinco pasadas varía casi un tercio entre dos tandas idénticas en
todo lo controlable, así que atribuir a «lo que hay en la cuenta» una diferencia de 1,9× entre
dos días distintos es contar una historia sobre ruido. La hipótesis no queda refutada: queda
**descartada por no ser comprobable con esta medición**. Comprobarla exigiría un experimento
de verdad —la misma cuenta poblada y vacía, alternando, muchas repeticiones— y eso mide una
propiedad de Google, no de LibreDisk.

Lo que sí se puede cerrar es la cifra: **el «4,7 veces más barato por operación» de §6 no se
sostiene y no debe citarse.** En tres tandas ha salido 4,7×, 1,6× y 2,5×. Lo único constante
es el signo: una operación contra OneDrive es más barata que contra Drive.

### Y de paso se cae una conclusión que §7 daba por robusta

§7 comparó el ancho de banda de las dos nubes en dos tandas, vio la misma proporción y
escribió que era «robusta frente al cambio de red». Con una tercera tanda:

| MB/s reales (`fragmento_de_4_mib` − `integridad_binaria`, 8 MiB) | 09-18 | 09-21 mañana | 09-21 noche |
|---|---:|---:|---:|
| Drive | 3,3 | ~1,4 | **1,22** |
| OneDrive | 2,0 | ~0,85 | **2,85** |
| **Drive / OneDrive** | 1,7× | 1,7× | **0,43×** |

No es que la proporción cambie de tamaño: **cambia de sentido.** Esta noche OneDrive mueve
datos más del doble de rápido que Drive, después de dos tandas moviéndolos casi la mitad de
rápido.

Dos muestras que coinciden no son una constante, son dos muestras que coinciden. Escribir
«robusta» con n = 2 fue un error de quien esto escribe, y la frase queda corregida en §7.

### Lo que sí se reproduce: las proporciones **dentro** de un proveedor

Comparar dos nubes entre sí no funciona. Comparar dos operaciones de la misma nube sí, y
mucho:

| `not_found_distinguible` / `borrar_inexistente` | 09-18 | 09-21 noche |
|---|---:|---:|
| Drive | 2,73× | **2,63×** |
| OneDrive | 1,01× | **0,96×** |

Con tres días, dos redes, dos máquinas y dos cuentas de por medio, estos dos números se
repiten con dos cifras. Y tiene sentido: las dos comprobaciones de una misma nube se miden
en la misma tanda, contra la misma cuenta y con la misma red debajo, así que todo lo que se
mueve entre tandas se cancela al dividir. Entre proveedores no se cancela nada.

De ahí sale la regla de método para el resto del proyecto: **medir proporciones dentro de un
proveedor, no entre proveedores.** Y las dos que se sostienen son justo las que sostienen el
diseño:

- **En Drive, preguntar por algo que no existe cuesta 2,6 veces una operación normal.** Es el
  peor caso, es estable, y es exactamente lo que hacen `scrub` y `repair`.
- **En OneDrive cuesta lo mismo que cualquier otra cosa.** Graph direcciona por ruta; Drive
  busca.

### La coincidencia de §7 era una coincidencia

§7 encontró que enumerar un prefijo vacío costaba 1 133 ms en OneDrive y 1 137 ms en Drive
—cuatro milisegundos de diferencia— y tuvo el buen criterio de no creérselo sin repetirlo.
Hizo bien:

| `listado_de_prefijo_vacio` | 09-21 mañana | 09-21 noche |
|---|---:|---:|
| OneDrive | 1 133 ms | **593 ms** |
| Drive | 1 137 ms | **1 522 ms** |

De coincidir en cuatro milisegundos a diferir en 2,6 veces. **El coste de enumerar sí lo pone
el proveedor**, y la frase de §7 que sugería lo contrario queda anulada.

En cambio `listado_enumera_lo_escrito` en Drive pasa de 15 148 ms a 15 535 ms: un 2,6 % en
nueve horas. Drive fue el proveedor estable de esta tanda y OneDrive el que se movió, que es
lo contrario de lo que sugería la dispersión de cada uno. Otra razón para no fiarse de una
sola tanda.

### Qué se da por cerrado y qué no

**Cerrado:** el suelo por operación es de **cientos de milisegundos a segundos** en las dos
nubes, en las tres tandas, en dos redes. Un `scrub` fragmento a fragmento de 1 TB no cabe en
ningún presupuesto de tiempo, `list` es necesario, y preguntar por lo que falta es el caso
caro en Drive y no en OneDrive. Todo lo que el diseño apoya en órdenes de magnitud sigue en
pie.

**No cerrado, y probablemente no cerrable así:** cualquier cifra que compare las dos nubes
entre sí. No hay ninguna proporción entre proveedores en este documento que haya sobrevivido
a tres tandas. **Las que quedan escritas son de la tanda en que se midieron, y nada más.**

## 9. Primer volumen real sobre dos nubes, y qué dice cada una de su espacio

Fecha: 2026-09-22. Cuentas reales de OneDrive y Google Drive, desde esta máquina y esta red.
Topología `espejo:2`, o sea una copia entera en cada nube.

### 9.1 Lo que pasó

| | |
|---|---|
| Crear el volumen | instantáneo: es escribir la cabecera en los dos |
| Subir 20,0 MiB (2 trozos) | **3 min 6 s** |
| Bajarlos y comprobar el SHA-256 | **1 min 32 s**, idéntico byte a byte |
| Enumerar los dos proveedores (`espacio --medir`) | **37 s** |

La bajada no descarga los dos lados: con `mirror` basta el primer fragmento que conteste, y
por eso tarda la mitad que la subida en vez de lo mismo.

Los 37 s de la enumeración **no contradicen** los 5 a 17 minutos estimados en §7: aquel
cálculo era para un volumen con los 257 directorios del prefijo de dos caracteres poblados, y
este tiene dos trozos, o sea dos directorios. El coste sigue dependiendo del árbol y no del
número de bytes, que es justo lo que decía §7.

### 9.2 Cuánto dice cada nube que le queda

Una petición por proveedor, no una enumeración:

| | Total | Ocupado en la cuenta | Este volumen |
|---|---|---|---|
| OneDrive | 15,0 GiB | 921,6 MiB (6 %) | 20,0 MiB |
| Google Drive | 17,0 GiB | 13,5 GiB (79 %) | 20,0 MiB |

Las dos primeras columnas son de la cuenta entera —las fotos del usuario cuentan tanto como
los fragmentos— y salen de la API de cupo de cada servicio. La tercera es de LibreDisk y sale
de la enumeración. Son cifras distintas y cuestan cosas distintas: por eso en la línea de
órdenes la tercera va detrás de `--medir`.

### 9.3 El dato que obliga a no fiarse de la resta

**Después de subir 20,0 MiB, OneDrive siguió diciendo 921,6 MiB.** Tres consultas repartidas
en unos quince minutos, en procesos distintos, dieron la misma cifra. Los 20 MiB son un 2,2 %
de esa cantidad, así que no es un redondeo que la esconda: la cuenta de uso que devuelve el
proveedor **va por detrás de lo que uno acaba de escribir**. Cuánto por detrás, este
experimento no lo dice.

Eso tiene una consecuencia directa en la interfaz, y está implementada: el tramo de la barra
que corresponde a LibreDisk se recorta a lo que el proveedor dice que hay ocupado
(`min(del_volumen, usado)`). Sin ese recorte, un volumen recién escrito pintaría un tramo
mayor que la barra entera y la ventana enseñaría más del 100 % de una cuenta que está al 6 %.

No es un fallo de OneDrive ni hay nada que arreglar: es que las dos cifras se miden en
momentos distintos y por sistemas distintos. Lo que no se puede hacer es restarlas y
presentar el resultado como si fuera una medida.

## 10. El permiso mínimo de Google basta

Fecha: 2026-09-22. Aplicación propia registrada en Google Cloud, cliente de tipo escritorio,
con **un único ámbito: `drive.file`** — acceso solo a los ficheros que la propia aplicación
crea. Cuenta real, volumen `concat` sobre Google Drive.

Esto no era obvio. La biblioteca que usamos busca carpetas por nombre para resolver rutas, y
con un permiso restringido eso solo funciona sobre lo que la aplicación haya creado ella
misma. Si hubiera hecho falta el ámbito amplio `drive`, Google lo clasifica como restringido
y exige una evaluación de seguridad anual de terceros.

| Operación | Resultado |
|---|---|
| Renovar el token y leer el cupo de la cuenta | 1,3 s |
| Crear el volumen | inmediato |
| Subir 3,0 MiB | 19 s |
| Bajarlo y comparar el SHA-256 | idéntico |
| `ls` sobre la raíz | correcto |
| Enumerar el volumen entero (`scrub`) | 6,6 s, 3 trozos, 0 degradados |
| Medir lo que ocupa | 3,0 MiB |

**Todo lo que LibreDisk hace cabe dentro de `drive.file`.**

### 10.1 Y no ve lo que no creó

La otra mitad, que es la que sostiene lo que promete la política de privacidad. Apuntando la
aplicación nueva a `/libredisk-pruebas` —la carpeta donde vive el volumen de §9, creado por
una aplicación **distinta** con permiso total— la respuesta es:

```
no hay ningun volumen en estos proveedores
```

No es que no sepa leerlo: es que **no lo ve**. Para esa aplicación esos ficheros no existen.
Es la comprobación directa de que «solo los ficheros que LibreDisk crea» describe lo que pasa
y no lo que nos gustaría que pasara.

### 10.2 Lo que esto ahorra

- No hace falta la evaluación de seguridad de terceros que Google exige para los ámbitos
  restringidos, que es anual y cuesta miles de dólares.
- La pantalla de permiso le pide al usuario algo proporcionado: acceso a lo que el programa
  vaya a crear, no a todo su Drive. Quien la lea puede aceptar sin encogerse.

## 11. Honestidad sobre el rendimiento

Posicionarlo correctamente desde el README evita el 90% de las malas reseñas:

- **Bueno para:** biblioteca de medios, archivo, fotos, datasets, ISOs, datos tibios.
- **Malo para:** disco de sistema, bases de datos, compilaciones, cualquier E/S aleatoria.

## Licencia

**AGPL-3.0** es la elección por defecto: el nombre del proyecto promete *libre*, y la AGPL
impide que un tercero lo convierta en un SaaS cerrado. Si más adelante quieres una vía
comercial (soporte, versión alojada), el patrón estándar es AGPL + CLA, que te deja
relicenciar. Si lo que priorizas es adopción y contribuciones corporativas, Apache-2.0 o
MPL-2.0 son las alternativas.
