# Hoja de ruta

Cada versión es una pregunta de riesgo que se responde. No se empieza la siguiente sin
haber contestado la anterior.

## v0.1 — "¿Funciona la idea?" (Linux, CLI)

Pila elegida: **Rust + OpenDAL**.

- [x] `trait Backend` + adaptador OpenDAL (memoria, directorio local, S3, WebDAV)
- [x] Cifrado por trozo (XChaCha20-Poly1305 + Argon2id), AAD ligado al id del trozo
- [x] Motor RAID: `concat`, `mirror:n`, `rs:k,m` con Reed-Solomon
- [x] Almacén de trozos direccionado por contenido (BLAKE3) con verificación al leer
- [x] `scrub` y `repair` como operaciones de biblioteca
- [x] Suite de conformidad de proveedores + configuracion por entorno (`libredisk check`)
- [x] Constructores de OneDrive y Google Drive (OAuth2 con token de refresco)
- [x] Google Drive probado contra una cuenta real: las nueve comprobaciones pasan.
      Medidas y consecuencias en [VIABILIDAD.md](VIABILIDAD.md#5-medidas-reales-contra-google-drive)
- [x] OneDrive probado contra una cuenta real. Medidas y comparación con Drive en
      [VIABILIDAD.md](VIABILIDAD.md#6-medidas-reales-contra-onedrive)
- [ ] Mismo trato para S3 y WebDAV
- [x] `list` en el `trait Backend`: en Drive, preguntar por algo que no existe cuesta 6,5 s,
      y eso es justo lo que hacía `scrub`. Un volumen de 1 TB serían 30 días en serie.
      Ya se enumera: `ChunkStore::scrub_many` gasta **una llamada a `list` por proveedor** en
      vez de una por fragmento, y un test que cuenta llamadas lo verifica en vez de suponerlo.
      El coste deja de depender del número de trozos; lo que queda depende del árbol (ver más
      abajo).
      Un proveedor que no se deja enumerar se declara en `ScrubReport::unlistable` en vez de
      darse por vacío: una caída pasajera no puede leerse como pérdida total
- [x] OneDrive vuelve a pasar la suite con las once comprobaciones, ya con `list`. Hizo falta
      subir OpenDAL de 0.57.0 a 0.59.2, y con ello el MSRV de 1.85 a **1.91**: el lister de
      OneDrive en 0.57 consultaba versiones de cada fichero porque leía la *capacidad* del
      servicio en vez de lo que pedía el llamante, y construía la URL con la ruta duplicada.
      En debug reventaba con un panic; en release fallaba al deserializar. Corregido aguas
      arriba en 0.59.2, que es la primera con el arreglo y exige 1.91
- [x] Google Drive vuelve a pasar la suite con las once comprobaciones, ya con `list`. No hizo
      falta tocar nada: el lister de Drive no tenía el fallo del de OneDrive
- [ ] Decidir el anidamiento de `shard_key` en proveedores jerárquicos. Medido en
      [VIABILIDAD.md §7](VIABILIDAD.md#7-enumerar-qué-cuesta-list): OpenDAL recorre el árbol
      nivel a nivel y paga una petición por directorio — ~1,1 s el suelo en las dos nubes,
      3-4 s con contenido debajo. Con `chunks/<2 hex>/` son 257 directorios por proveedor, o
      sea entre 5 y 17 minutos por enumeración completa. El prefijo de dos caracteres existe
      para no acumular millones de objetos en un mismo directorio, que es lo que degrada a los
      almacenes planos; en los jerárquicos el incentivo va justo al revés. Probablemente deba
      depender del proveedor, igual que el tamaño de trozo
- [ ] Repetir las medidas de §5 y §6 desde la misma red que las de §7. El "4,7× más barato
      por operación" de OneDrive no se reprodujo el 2026-09-21 (salió 1,6×), y la hipótesis
      —que el coste de Drive depende de cuánto haya en la cuenta, porque OpenDAL resuelve
      rutas buscando— está sin verificar
- [ ] Tamano de trozo por proveedor: 16 MiB se queda corto donde el coste es por operacion
- [x] Trocear ficheros en trozos de 16 MiB. `ChunkStore::put_file` toma un `AsyncRead` y no un
      `&[u8]`, porque un fichero no tiene por qué caber en memoria; devuelve un `Manifest`
      (trozos en orden, longitud y tamaño de trozo). `read_at` baja **solo los trozos que el
      rango toca**, que es lo que hace viable el montaje, y lo verifica un test que cuenta
      descargas. El troceado es de tamaño fijo, no por contenido: insertar un byte al
      principio reescribe el fichero entero, y a cambio localizar un offset es una división
- [x] Manifiestos COW. El árbol vive en los proveedores como nodos inmutables, y cada nodo es
      un trozo más: pasa por el mismo `ChunkStore` que los datos, así que hereda cifrado,
      RAID, direccionamiento por contenido y deduplicación sin código propio. Los nombres de
      fichero quedan cifrados, y hay un test que lo comprueba contra los blobs. Modificar algo
      reescribe la rama hasta la raíz y termina intercambiando un único puntero, que es el
      **único objeto mutable** del sistema: va en espejo a todos los proveedores, no repartido
      con RAID, porque tiene que leerse aunque solo sobreviva uno. Como nada se sobrescribe,
      una raíz vieja sigue siendo legible: eso son **instantáneas sin código dedicado**
- [ ] ~~Índice local (SQLite o `redb`)~~. **Descartado**: la caché de trozos lo hace
      innecesario. Un índice de rutas serviría para evitar la petición por nivel en un
      `lookup` en frío, pero como los nodos del árbol *son* trozos, la caché general ya los
      guarda, y sin esquema, sin migraciones y sin invalidación. El direccionamiento por
      contenido hace que la solución general salga más simple que la especializada
- [x] Caché local en disco. Segundo escalón: memoria, disco, proveedores. Guarda el blob
      **cifrado**, no el contenido: cachear en claro sería más rápido pero dejaría los datos
      del usuario sin cifrar en la máquina local, que es justo lo que este proyecto promete
      que no pasa. Sobrevive al reinicio reconstruyendo su índice del directorio, se verifica
      el hash al leer —una caché corrupta en silencio sería peor que no tenerla— y va separada
      por volumen, porque dos volúmenes pueden tener el mismo trozo sellado con claves
      distintas y se invalidarían mutuamente
- [x] Escritura diferida. `put --diferido` prepara en local y vuelve enseguida; `sync` sube.
      El invariante que la sostiene: **nunca se publica una raíz que apunte a trozos que
      todavía no estén en los proveedores**. Si una subida falla, la raíz se queda esperando y
      el volumen se sigue viendo como antes de la escritura — un estado viejo, pero íntegro.
      Sobrevive a un corte: la cola se recupera del diario al arrancar
- [x] Montaje en lectura-escritura. `libredisk mount Z: --escritura`: el Explorador crea
      carpetas, `Copy-Item` copia, borrar funciona y borrar un directorio con contenido se
      rechaza. Lo escrito queda en la cola de escritura diferida hasta `libredisk sync`, y
      cada descriptor acumula los tramos en un buffer que se fusiona cuando son contiguos —
      sin eso, copiar en bloques de 1 MiB reescribiría el mismo trozo de 16 MiB una vez por
      bloque
- [x] **Amplificación de escritura**, atacada donde importa. `sync` calcula lo alcanzable
      desde la raíz pendiente y **no sube lo que ya nadie referencia**. Medido con la misma
      copia de 3 MB por el montaje: de 13 trozos preparados se suben **4 (3,0 MiB)** y se
      descartan **9 (3,0 MiB)**. Los objetos en el proveedor pasan de **22 a 5** y el espacio
      de **6,1 MB a 3,1 MB**.
      Es seguro descartar porque un trozo preparado se escribió en esta sesión y la raíz que
      ya estuviera publicada es anterior: no puede referenciarlo. Si el recorrido falla no se
      descarta nada, que es el lado prudente del error
- [x] `rename` en el montaje. Renombrar y mover desde el Explorador funcionan. Los dos
      cambios —quitar del origen, poner en el destino— salen en **una sola raíz**: si fueran
      dos publicaciones habría una generación intermedia en la que el fichero no está en
      ninguno de los dos sitios, y un lector podría pillarla. Hay un test que lo comprueba
      abriendo la raíz anterior y la nueva
- [x] Resolución **sin distinguir mayúsculas** en la capa de montaje. El árbol sigue
      distinguiendo —en Linux un sistema de ficheros distingue, y el núcleo sirve a los dos—
      y la traducción vive solo en el adaptador de WinFsp: `canonica` devuelve la forma
      guardada, y al crear se traduce el padre pero se respeta la forma que escribió el
      usuario, que es lo que significa `case_preserved_names`.
      Primero se prueba la ruta tal cual, que es el caso normal porque Windows devuelve los
      nombres como se los dimos al listar; solo si falla se baja por el árbol comparando sin
      distinguir
- [x] `libredisk scrub` y `rebuild` expuestos en la CLI. `scrub` recorre el árbol, enumera
      los proveedores y dice qué le falta a cada trozo y dónde; si un nodo no se deja leer,
      avisa de que el recorrido quedó **incompleto** en vez de dar un resumen tranquilizador
      sobre lo poco que pudo mirar
- [x] El volumen, expuesto en la CLI: `volume create`, `ls`, `put`, `get`, `cat`, `mkdir`,
      `rm`, `volume status`, `scrub` y `rebuild`. Para abrir un volumen sabiendo solo la frase
      de paso hizo falta una **cabecera en claro** (`libredisk-volume.json`, replicada en
      todos los proveedores) con la sal, la topología y el orden de los proveedores: la sal no
      puede ir cifrada porque sin ella no hay clave, y el orden es parte del formato porque el
      índice de un fragmento es la posición de su proveedor
- [x] Capa de reintentos de OpenDAL. No es una optimización, es corrección: probando contra
      cuentas reales se perdió **una escritura suelta a OneDrive** —que dejó un volumen con
      menos redundancia de la pedida— y **una lectura suelta a Drive**, que hizo que un
      `scrub` se saltara medio árbol. Reintentar es seguro en las cinco operaciones del trait
      *porque* los trozos son inmutables

**Criterio de éxito:** montar un volumen `rs(2,1)` sobre 3 backends, copiar 10 GB, borrar un
backend entero, y que el volumen siga íntegro y se reconstruya.

> Estado: **el criterio se cumple a nivel de volumen**, verificado el 2026-09-21 contra
> cuentas reales. Un `rs(2,1)` sobre Google Drive, OneDrive y un directorio local: se sube un
> fichero, se destruye un proveedor entero, el fichero se sigue leyendo byte a byte idéntico,
> `rebuild` regenera los cuatro fragmentos que faltaban en 34 s y `scrub` vuelve a dar
> redundancia completa.
>
> Con dos salvedades que hay que decir: fue **1 MB y no 10 GB**, y **no hay montaje** todavía,
> que es lo que exige el resto de la lista.

## v0.2 — "¿Sobrevive a Windows?"

> **Windows se adelantó a Linux**, invirtiendo el orden original. El motivo es práctico: la
> máquina de desarrollo es Windows, y FUSE no se puede ni compilar ahí. La capa VFS que ambos
> necesitan ya estaba hecha, así que el trabajo era el adaptador.

- [x] Capa de montaje con WinFsp. `libredisk mount Z:` y el volumen aparece como una unidad
      más: el Explorador lista carpetas, `Copy-Item` copia ficheros y escribir se rechaza.
      **Solo lectura**, porque escribir a través del montaje exige antes una caché con
      escritura diferida: sin ella cada `write` de 4 KiB sería una subida síncrona a varias
      nubes, y ningún programa espera eso. **Y es lento**, por la misma razón: cada operación
      va a la nube. Sirve para demostrar que funciona, no para usarlo
- [x] CI multiplataforma (GitHub Actions: linux + windows)
- [ ] Instalador MSI, servicio de Windows, certificado de firma de código
- [x] Caché de lectura en memoria, acotada por bytes y con desalojo del menos usado. **No
      necesita invalidación**, y no por simplificar: los trozos son inmutables y su nombre es
      su contenido, así que una entrada cacheada no puede quedar obsoleta. Lo único mutable
      del volumen es el puntero de raíz, que no pasa por la caché. Medido a través del
      montaje sobre un proveedor local: leer 7,6 MiB pasa de **2 718 ms a 23 ms**, y contra
      una nube de verdad la diferencia es mucho mayor porque la primera lectura son segundos

- [x] Sincronización automática mientras el volumen está montado. Un hilo de fondo sube lo
      pendiente cada `--sync-cada` segundos (30 por defecto) y al desmontar se sube lo que
      quede antes de salir: dejar cosas a medias justo al desmontar sería la peor forma de
      perderlas, porque el usuario cree que ha terminado.
      Trabaja sobre una **copia del almacén**, no sobre el volumen: subir puede tardar
      minutos, y sostener el cerrojo del volumen durante ese rato bloquearía todas las
      escrituras. La copia comparte cachés y cola, así que ve lo mismo sin estorbar
- [x] Arreglada una carrera que la sincronización de fondo destapaba: `sync` leía los trozos
      pendientes y la raíz por separado, así que una escritura colada entre las dos lecturas
      podía hacer que se publicara una raíz cuyos trozos nuevos no estaban subidos — justo el
      invariante que la escritura diferida existe para sostener. Ahora se leen de una vez y
      la raíz solo se publica si no ha cambiado. Hay un test que provoca la carrera de verdad,
      con un proveedor que cuela una escritura en mitad de la subida, y que **falla si se
      quita el arreglo**

## v0.3 — "¿Lo puede usar alguien que no sea yo?"
- [ ] GUI de escritorio (Tauri): asistente de volumen, salud, cola de subida
- [ ] **Estimador de coste mensual** por topología y proveedor
- [ ] Indicador honesto de "guardado localmente" vs "a salvo en la nube"

## v0.4 — Nubes de consumo
- [ ] OAuth para Dropbox y pCloud (Google Drive y OneDrive ya tienen backend en v0.1)
- [ ] MEGA: sin servicio en OpenDAL. O `rclone serve webdav` por delante, o el crate `mega`
      no oficial. Ver [PRUEBAS-PROVEEDORES.md](PRUEBAS-PROVEEDORES.md#mega)
- [ ] Limitador de tasa por proveedor y *backoff* adaptativo
- [ ] Avisos claros de Términos de Servicio en la UI

## v0.5 — Robustez
- [x] `libredisk rescue`: reconstruir metadatos solo desde los backends. Enumera los trozos,
      los descifra y busca los directorios a los que no apunta nadie: son las raíces
      candidatas, incluidas las de versiones anteriores, y se describen por su contenido para
      poder elegir sin mirar hashes. **No hizo falta la cabecera autodescriptiva por trozo**
      que preveía ARQUITECTURA: la longitud sellada se deduce porque `split` rellena hasta un
      múltiplo de *k*, y el tag de XChaCha20-Poly1305 desambigua. Se añade también
      `libredisk header export`/`import`, que es el respaldo que de verdad importa: la
      cabecera lleva la sal, y sin sal no hay clave
- [ ] Rebalanceo al añadir/retirar un proveedor
- [ ] Instantáneas y retención
- [ ] macOS (FSKit o NFS loopback; macFUSE tiene problemas de licencia)

## Móvil (Android)

Analizado en septiembre de 2026 y verificado contra el código. **No es un tercer objetivo de
montaje**, sino un modelo de integración distinto: por eso tiene sección propia en vez de una
casilla en v0.2.

### Qué se reaprovecha

El núcleo no tiene ataduras al sistema operativo: **cero `cfg(windows)`/`cfg(unix)`** en todo
el workspace, y `std::fs` solo aparece en tests y en el comando `demo`. El `trait Backend` es
`async put/get/delete/exists` sobre bytes.

Comprobado cruzando a `aarch64-linux-android`:

| Crate | Resultado |
|---|---|
| `libredisk-crypto`, `libredisk-raid` | compilan sin NDK |
| `libredisk-core` → `backend` → `opendal` | requiere el NDK: `cc-rs` no encuentra `clang`, lo arrastra `aws-lc-sys` vía `rustls` |

Lo segundo no es un problema de diseño. Se resuelve instalando el NDK y apuntando
`CC_aarch64-linux-android`, o cambiando `rustls` al proveedor `ring`.

### Por qué no se puede montar

Android es Linux, pero **FUSE exige root**: `/dev/fuse` no es accesible para una app normal.
Desde Android 11 el propio sistema usa FUSE para el almacenamiento con ámbito, y justamente
por eso no deja montar a nadie más.

La vía sin root es el **Storage Access Framework**: implementar un `DocumentsProvider`. El
volumen aparece en la app de Archivos y en el selector del sistema, y otras apps leen y
escriben por `ContentResolver`. Es lo que hacen Cryptomator y Round Sync.

Pero SAF **no es un montaje POSIX**: no hay `open()` por ruta ni `mmap`, y una app que use
rutas crudas no ve nada. Para lectura aleatoria haría falta
`StorageManager.openProxyFileDescriptor` (API 26+), que entrega un descriptor con callbacks
— eso sí encaja con un almacén de trozos más caché local.

El coste sobre la arquitectura es real: la capa VFS necesitaría un **tercer consumidor con
semántica distinta**. FUSE y WinFsp se parecen entre sí; SAF no. Más un puente JNI.

### Restricciones propias del móvil

| Problema | Consecuencia |
|---|---|
| Doze y límites de segundo plano | la cola de subida no puede correr sin más: exige `WorkManager` y servicio en primer plano |
| Datos móviles | `rs(4,2)` sube 1,5×, y leer exige *k* fragmentos de *k* proveedores distintos. Política de solo-WiFi obligatoria |
| Batería | Argon2id + Reed-Solomon + XChaCha20 por trozo es intensivo en CPU |

Sumado a las medidas contra Drive (~6,5 s por operación, ver
[VIABILIDAD.md](VIABILIDAD.md#5-medidas-reales-contra-google-drive)), sobre red móvil la
latencia pasa a ser el factor dominante.

### Conflicto con el alcance de v1

v1 es **un solo escritor**. Un móvil añadido a un volumen que el escritorio también escribe
*es* el problema de escritura concurrente que está fuera de alcance. Android empieza por
tanto **en solo lectura**, o espera al *lease* renovable de v0.5.

### Dónde encaja

Ya no por falta de capa VFS: `Volume` tiene `lookup`, `read_dir`, `read_at` y `manifiesto`,
que es justo lo que un `DocumentsProvider` necesita. Lo que falta ahora es lo que haría
soportable usarlo desde un móvil —índice local y caché— y resolver el modelo de un solo
escritor. Ver [Recuperación ante desastre](ARQUITECTURA.md#recuperación-ante-desastre) para
lo que un lector de solo lectura necesita: el id de la raíz y nada más.

Lo barato mientras tanto, para no perder la opción: un job de CI con
`cargo check --target aarch64-linux-android` sobre `libredisk-crypto` y `libredisk-raid`.
Pasan sin NDK, así que cuesta segundos y evita que la portabilidad se pudra sin avisar.

## Fuera de alcance por ahora
- Escritura concurrente desde varias máquinas (v1 es un solo escritor)
- Uso como disco de sistema o volumen de base de datos
- Móvil como volumen de escritura — ver [Móvil (Android)](#móvil-android): analizado, pospuesto, no descartado
