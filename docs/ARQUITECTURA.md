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
