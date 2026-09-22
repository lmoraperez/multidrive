# Montar el volumen como servicio en Windows

Sin esto, el volumen solo existe mientras alguien tenga una consola abierta con
`libredisk mount`. Con esto, se conecta desde el Explorador como una unidad de red más y
**Windows pide la frase de paso**.

## Por qué no hay un servicio propio

Porque WinFsp ya trae uno. `WinFsp.Launcher` arranca con la máquina, corre como LocalSystem y
sabe lanzar sistemas de ficheros bajo demanda. Escribir otro al lado sería duplicar un
servicio que ya está instalado, firmado y depurado. Lo único que hace LibreDisk es declararse
ante él con una clave en el registro.

## La frase de paso no se guarda

Un volumen de conocimiento cero **no se puede montar solo al arrancar**. Para descifrarlo hace
falta la frase, y si la máquina la sabe sin que nadie la escriba, es que está guardada en la
máquina: el cifrado seguiría protegiendo frente a los proveedores, pero no frente a quien se
lleve el equipo.

El lanzador de WinFsp resuelve exactamente esto:

1. Windows pide la contraseña al conectar la unidad.
2. Se la pasa al lanzador.
3. El lanzador nos la entrega por la **entrada estándar**.

No pasa por la línea de órdenes, ni por el registro, ni por una variable de entorno: los tres
sitios donde acabaría siendo legible por otros procesos. `libredisk servicio estado` lo
enseña: la orden registrada no contiene ninguna frase.

Si al conectar marcas **«recordar»**, la guarda el Gestor de credenciales de Windows. Eso es
una decisión tuya, tomada en un diálogo del sistema, no una política de LibreDisk.

## Las credenciales de los proveedores sí hacen falta en disco

Aquí no hay truco que valga. Un servicio arranca como LocalSystem, sin consola y sin sesión:
**no ve tu `.env`**. Los tokens de los proveedores tienen que estar en un fichero que pueda
leer.

```
C:\ProgramData\LibreDisk\libredisk.conf
```

El formato son las mismas claves que las variables de entorno, así que el fichero se escribe
copiando el `.env` que ya tenías:

```
LIBREDISK_GDRIVE_REFRESH_TOKEN=...
LIBREDISK_GDRIVE_CLIENT_ID=...
LIBREDISK_GDRIVE_CLIENT_SECRET=...
LIBREDISK_ONEDRIVE_REFRESH_TOKEN=...
```

Admite `export` delante, comentarios con `#` y comillas alrededor del valor, para no tener que
traducir nada. Y admite UTF-8 con marca de orden de bytes, que es lo que escriben el Bloc de
notas y PowerShell.

> **Ese fichero contiene credenciales.** Por defecto, `C:\ProgramData` lo pueden leer todos
> los usuarios del equipo. En una máquina de un solo usuario da igual; en una compartida,
> restringe sus permisos antes de escribir nada dentro.

Para ver dónde se busca y qué está ganando:

```powershell
libredisk config ruta    # los ficheros, y cuál existe
libredisk config ver     # qué claves hay y de dónde salen (nunca imprime valores)
```

El entorno gana al fichero: quien exporta una variable está diciendo algo sobre **esa**
ejecución. `config ver` avisa de las claves del fichero que el entorno está tapando.

## Registrar

Necesita administrador: la clave vive en HKLM.

```powershell
libredisk servicio registrar --escritura
libredisk servicio estado
```

Opciones: `--cache-mb`, `--disco-mb`, `--sync-cada` y `--nombre` (lo que aparece en la ruta de
red).

## Montar

Desde el Explorador, *Conectar a unidad de red*:

- Carpeta: `\libredisk\volumen`
- Marcar «Conectar con otras credenciales» y poner la frase de paso como contraseña.

O desde una consola:

```powershell
net use Z: \libredisk\volumen
```

> **Hace falta reiniciar una vez después de instalar WinFsp.** Su proveedor de red se registra
> en el arranque, así que hasta el primer reinicio `net use` responde «error de sistema 67: no
> se encuentra el nombre de red especificado» aunque todo lo demás esté bien.

## Cuando algo no arranca

El servicio no tiene dónde escribir, así que su salida va a un fichero:

```
C:\ProgramData\LibreDisk\libredisk.log
```

Ahí aparecen los dos fallos que se llevan casi todos los casos:

- **«no se pudo leer ninguna raíz»** — la frase de paso no es la del volumen.
- **«no hay ningún proveedor configurado»** — falta el fichero de configuración, o está en una
  ruta que LocalSystem no lee. `libredisk config ruta`.

## Quitar

```powershell
libredisk servicio quitar
```

No toca el volumen ni sus datos: eso solo era la forma de montarlo.
