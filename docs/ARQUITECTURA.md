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
| `concat` (JBOD) | 1 de 1 | nada | 1,0× |
| `mirror` | 1 de n | n−1 caídas | n× |
| `rs(k,m)` | k de k+m | m caídas | (k+m)/k × |

Ejemplo práctico: `rs(4,2)` con 6 proveedores → sobrevive a que 2 te suspendan la cuenta,
con 1,5× de sobrecoste de espacio.

### Metadatos
SQLite local (o `redb`) con el árbol de inodos y el mapa de trozos. **Se replica a la nube**
(diario WAL + instantánea periódica en *n* proveedores), porque perder el disco local no
puede significar perder el volumen.

Cada trozo lleva además una cabecera autodescriptiva con el ID de su manifiesto, para que
`libredisk rescue` pueda reconstruir el árbol escaneando los proveedores aunque no quede
ninguna copia de la base de metadatos.

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
