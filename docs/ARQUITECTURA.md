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
| `concat` (JBOD) | 1 de 1 | nada | 1,0× |
| `mirror` | 1 de n | n−1 caídas | n× |
| `rs(k,m)` | k de k+m | m caídas | (k+m)/k × |

Ejemplo práctico: `rs(4,2)` con 6 proveedores → sobrevive a que 2 te suspendan la cuenta,
con 1,5× de sobrecoste de espacio.

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
  copia local. Para eso cada trozo llevará una cabecera autodescriptiva con el ID de su
  manifiesto. **Todavía sin implementar.**
- Un lector de solo lectura —el caso de [Android](ROADMAP.md#móvil-android)— necesita
  exactamente una cosa: el id de la raíz. No replica una base ni reproduce un diario, y baja
  solo la rama del árbol que el usuario abre, que es justo el patrón de acceso de SAF. Si los
  metadatos vivieran solo en una base local, un lector externo tendría que replicarla entera.

Android **no es el motivo** de esta decisión —lo son `rescue` y la falta de renombrado
atómico— pero pide exactamente la misma propiedad. Conviene saber que no cuesta nada extra.

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
