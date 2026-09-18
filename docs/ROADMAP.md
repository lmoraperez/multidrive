# Hoja de ruta

Cada versión es una pregunta de riesgo que se responde. No se empieza la siguiente sin
haber contestado la anterior.

## v0.1 — "¿Funciona la idea?" (Linux, CLI)
- [ ] `trait Backend` + adaptadores: directorio local, compatible con S3, WebDAV
- [ ] Almacén de trozos direccionado por contenido (BLAKE3, 16 MiB)
- [ ] Cifrado por trozo (XChaCha20-Poly1305 + Argon2id)
- [ ] Motor RAID: `concat`, `mirror`, `rs(k,m)`
- [ ] Metadatos en SQLite + manifiestos COW
- [ ] Caché local en disco con escritura diferida
- [ ] Montaje FUSE de solo lectura → luego lectura/escritura
- [ ] `libredisk scrub` y `rebuild`

**Criterio de éxito:** montar un volumen `rs(2,1)` sobre 3 backends, copiar 10 GB, borrar un
backend entero, y que el volumen siga íntegro y se reconstruya.

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
