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

Dos pasadas de la suite de conformidad (`libredisk check`) contra una cuenta real, el
2026-09-18: la primera en frío, la segunda con las carpetas ya creadas. **Las nueve
comprobaciones pasan en ambas**: Drive cumple el contrato de `Backend`.

| Comprobación | 1.ª (frío) | 2.ª (caliente) | Ops | Por op (2.ª) |
|---|---:|---:|---:|---:|
| `ida_y_vuelta` (64 KiB) | 10 718 ms | 8 906 ms | 2 | ~4 453 ms |
| `sobrescritura` | 8 990 ms | 7 760 ms | 3 | ~2 587 ms |
| `not_found_distinguible` | 7 152 ms | 6 530 ms | 1 | **6 530 ms** |
| `borrar_inexistente` | 2 864 ms | 2 319 ms | 1 | **2 319 ms** |
| `exists_refleja_el_borrado` | 16 343 ms | 9 264 ms | 5 | ~1 853 ms |
| `integridad_binaria` (4 KiB) | 5 533 ms | 5 097 ms | 2 | ~2 549 ms |
| `clave_con_forma_de_fragmento` | 9 979 ms | 8 299 ms | 2 | ~4 150 ms |
| `fragmento_de_4_mib` | 8 349 ms | 7 981 ms | 2 | ~3 991 ms |
| `seis_escrituras_en_paralelo` | 7 293 ms | 5 624 ms | 6 en paralelo | ~937 ms amortizado |
| **Total** | **77 221 ms** | **61 780 ms** | | |

### El arranque en frío no era la explicación

La segunda pasada solo baja un 20 % en total, repartido por todas las comprobaciones en vez
de concentrado en la primera. Crear las carpetas la primera vez costaba algo, pero poco: el
coste por operación es **estado estacionario**, no calentamiento. Eso refuerza la conclusión
en lugar de matizarla.

### El coste es por operación, no por byte

`borrar_inexistente` es el instrumento más limpio: una sola operación, cero bytes,
**2,3 segundos**. Ese es el suelo. La comparación que lo remata, en la pasada caliente:

- 4 KiB de ida y vuelta: **5 097 ms**
- 4 MiB de ida y vuelta: **7 981 ms**

Mil veces más datos por 1,6 veces el tiempo, y la proporción se repite en las dos pasadas.
Drive no cobra en ancho de banda, cobra en *round trips*: OpenDAL localiza cada ruta
buscándola en Drive antes de tocarla, y esa búsqueda domina todo lo demás.

### La consulta que falla es la cara

El dato más incómodo, consistente en las dos pasadas: **un `get` de algo que no existe cuesta
6,5 s, casi tres veces un `exists` que sí resuelve (1,9 s).** Buscar y no encontrar es el peor
caso de Drive.

Y resulta que ese es exactamente el perfil de trabajo de `scrub` y `repair`: preguntar por
fragmentos que quizá falten. La operación que más va a usar el mantenimiento del volumen es
la más lenta del proveedor.

### Consecuencias para el diseño

1. **`scrub` no puede ser un `exists` por fragmento.** Un volumen de 1 TB en trozos de 16 MiB
   con `rs(4,2)` son ~393 000 objetos. A 1,9 s el caso bueno son 8 días en serie; a 6,5 s el
   caso malo —que es el que importa, porque scrub existe para encontrar lo que falta— son 30.
   Ni con 20 operaciones en paralelo baja de un día. Hay que **enumerar** el proveedor y
   comparar contra el manifiesto. El `trait Backend` no tiene `list` a propósito; esta medida
   es el argumento para añadirlo.

2. **Los trozos de 16 MiB se quedan cortos para Drive.** [ARQUITECTURA.md](ARQUITECTURA.md)
   los justifica como compromiso. Con un suelo de ~2,3 s por operación, leer 1 MiB cuesta
   prácticamente lo mismo que leer 16. El tamaño de trozo debería ser **por proveedor**.

3. **El paralelismo funciona y hay que usarlo a fondo.** Seis escrituras simultáneas: 5,6 s
   frente a ~2,5 s una sola. Seis veces el trabajo por 2,2 veces el tiempo, sin un solo 429.
   El reparto RAID entre proveedores sale casi gratis; falta encadenar también *trozos* en
   vuelo, no solo fragmentos.

### Qué significa para el volumen montado

Un trozo de 16 MiB en `rs(4,2)` son 4 fragmentos de 4 MiB que se bajan en paralelo:
aproximadamente **5–7 s por trozo**, o unos 2,5 MB/s por flujo secuencial. Leer un archivo de
1 GB de forma ingenua serían ~7 minutos.

Esto convierte la caché local con lectura anticipada en **un requisito previo al montaje
FUSE**, no en una mejora posterior: sin varios trozos en vuelo, el montaje sería inusable
para el caso de uso que el propio README promete.

### Cuánto fiarse de estos números

Dos pasadas, una cuenta, una red. La varianza entre ellas llega al **43 %** en una
comprobación (`exists_refleja_el_borrado`), así que ninguna cifra suelta vale como dato
publicable: lo que aguanta es el orden de magnitud y las proporciones, que sí se repiten.
Para publicar haría falta una mediana de varias pasadas, y medir un `rs(4,2)` completo en vez
de operaciones sueltas.

## 6. Honestidad sobre el rendimiento

Posicionarlo correctamente desde el README evita el 90% de las malas reseñas:

- **Bueno para:** biblioteca de medios, archivo, fotos, datasets, ISOs, datos tibios.
- **Malo para:** disco de sistema, bases de datos, compilaciones, cualquier E/S aleatoria.

## Licencia

**AGPL-3.0** es la elección por defecto: el nombre del proyecto promete *libre*, y la AGPL
impide que un tercero lo convierta en un SaaS cerrado. Si más adelante quieres una vía
comercial (soporte, versión alojada), el patrón estándar es AGPL + CLA, que te deja
relicenciar. Si lo que priorizas es adopción y contribuciones corporativas, Apache-2.0 o
MPL-2.0 son las alternativas.
