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
- [ ] Constructores S3 y WebDAV probados contra servicios reales (hoy solo compilan)
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
- [ ] OAuth para Google Drive, OneDrive, Dropbox, pCloud
- [ ] Limitador de tasa por proveedor y *backoff* adaptativo
- [ ] Avisos claros de Términos de Servicio en la UI

## v0.5 — Robustez
- [ ] `libredisk rescue`: reconstruir metadatos solo desde los backends
- [ ] Rebalanceo al añadir/retirar un proveedor
- [ ] Instantáneas y retención
- [ ] macOS (FSKit o NFS loopback; macFUSE tiene problemas de licencia)

## Fuera de alcance por ahora
- Escritura concurrente desde varias máquinas (v1 es un solo escritor)
- Uso como disco de sistema o volumen de base de datos
- Móvil
