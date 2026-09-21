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
- [ ] Trocear ficheros en trozos de 16 MiB (el almacén opera sobre un trozo suelto)
- [ ] Metadatos en SQLite + manifiestos COW
- [ ] Caché local en disco con escritura diferida
- [ ] Montaje FUSE de solo lectura → luego lectura/escritura
- [ ] `libredisk scrub` y `rebuild` expuestos en la CLI

**Criterio de éxito:** montar un volumen `rs(2,1)` sobre 3 backends, copiar 10 GB, borrar un
backend entero, y que el volumen siga íntegro y se reconstruya.

> Estado: el criterio ya se cumple **a nivel de trozo** (`cargo run -- demo`, y los tests de
> `libredisk-core` lo verifican en todas las combinaciones de caída). Falta que se cumpla a
> nivel de *volumen montado*, que es lo que exige el resto de la lista.

## v0.2 — "¿Sobrevive a Windows?"
- [ ] Capa de montaje con WinFsp
- [ ] Instalador MSI, servicio de Windows, certificado de firma de código
- [ ] CI multiplataforma (GitHub Actions: linux + windows)

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
- [ ] `libredisk rescue`: reconstruir metadatos solo desde los backends
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

No antes de la v0.3, y la razón es de dependencia, no de prioridad: **la capa VFS sobre la
que se apoyaría SAF todavía no existe.** Hoy `ChunkStore` opera sobre un trozo suelto
(`put(&[u8]) -> ChunkRef`); no hay fichero, ni ruta, ni inodo.

Lo barato mientras tanto, para no perder la opción: un job de CI con
`cargo check --target aarch64-linux-android` sobre `libredisk-crypto` y `libredisk-raid`.
Pasan sin NDK, así que cuesta segundos y evita que la portabilidad se pudra sin avisar.

## Fuera de alcance por ahora
- Escritura concurrente desde varias máquinas (v1 es un solo escritor)
- Uso como disco de sistema o volumen de base de datos
- Móvil como volumen de escritura — ver [Móvil (Android)](#móvil-android): analizado, pospuesto, no descartado
