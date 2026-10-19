# Despliegue del sitio

`multidrive.net` se sirve **desde el servidor propio** (no GitHub Pages).

- DNS: `multidrive.net` y `www.multidrive.net` → `154.197.1.5`, la misma IP que
  `chambaya.net` y `nextcloud.chambaya.net`.
- Servidor web: **Caddy**, con TLS automático de Let's Encrypt.
- Raíz del sitio: `/var/www/multidrive` (propietario `www-data`).
- Fuente en este repo: [`site/`](../site).
- Acceso: `ssh soporte@multidrive.net`, con `sudo` sin contraseña. Es la misma máquina que
  `chambaya.net` --mismas claves de anfitrión-- pero se entra por el nombre del proyecto: el
  otro es de otra cosa y puede dejar de apuntar aquí cualquier día.

Bloques en `/etc/caddy/Caddyfile`:

```caddyfile
multidrive.net, www.multidrive.net {
	root * /var/www/multidrive
	encode zstd gzip
	header {
		Strict-Transport-Security "max-age=31536000; includeSubDomains"
		X-Content-Type-Options "nosniff"
		X-Frame-Options "SAMEORIGIN"
		Referrer-Policy "no-referrer"
	}
	file_server
}

libredisk.chambaya.net {
	redir https://multidrive.net{uri} permanent
}
```

El segundo bloque es el nombre de antes del cambio de marca. Redirige **conservando la
ruta**, y eso no es un detalle: la política de privacidad registrada en la pantalla de
consentimiento de Google apunta todavía a
`https://libredisk.chambaya.net/privacidad.html`. Mientras esa URL no se cambie en Google
Cloud, el bloque tiene que quedarse; quitarlo dejaría el consentimiento apuntando a un sitio
que no responde. La copia vieja en `/var/www/libredisk` ya no la sirve nadie.

## Publicar un cambio

El sitio son **cuatro páginas**: la portada y la política de privacidad, cada una en español y
en inglés. El español vive en la raíz y es el original; el inglés vive en `/en/`.

```sh
scp site/index.html site/privacidad.html soporte@multidrive.net:/tmp/
scp site/en/index.html soporte@multidrive.net:/tmp/en-index.html
scp site/en/privacidad.html soporte@multidrive.net:/tmp/en-privacidad.html
ssh soporte@multidrive.net '
  sudo install -o www-data -g www-data -m 644 /tmp/index.html /var/www/multidrive/index.html
  sudo install -o www-data -g www-data -m 644 /tmp/privacidad.html /var/www/multidrive/privacidad.html
  sudo mkdir -p /var/www/multidrive/en
  sudo install -o www-data -g www-data -m 644 /tmp/en-index.html /var/www/multidrive/en/index.html
  sudo install -o www-data -g www-data -m 644 /tmp/en-privacidad.html /var/www/multidrive/en/privacidad.html'
```

Los dos ficheros ingleses se copian con otro nombre a `/tmp` a propósito: los cuatro se llaman
igual de dos en dos, y mandarlos sin renombrar deja el ingles pisando al español en `/tmp`
--o al revés, según el orden-- y se instala dos veces la misma página sin que nada falle.

Respaldar antes lo que se va a pisar: `sudo cp -p fichero fichero.bak-$(date +%Y%m%d-%H%M%S)`.

Caddy sirve `/en/index.html` en `https://multidrive.net/en/` sin configuración ninguna: su
`file_server` busca el `index.html` del directorio. No hay que tocar el `Caddyfile` para esto.

### Las dos lenguas van juntas

**El español es el original y el inglés es la traducción**, y las dos páginas lo dicen en el
pie. Eso no es permiso para dejar el inglés atrás: una traducción vieja que afirma cosas que
ya no son ciertas es peor que no tener traducción, porque el lector no tiene forma de saber
que está leyendo algo caducado.

Así que un cambio de contenido en la portada se publica **en los dos idiomas en el mismo
despliegue**. Si no hay tiempo de traducirlo, no se publica a medias: se espera.

Lo mismo con el estilo. Las cuatro páginas llevan su CSS **dentro**, y el de `en/index.html`
es copia byte a byte del de `index.html` --se extrajo del fichero, no se escribió dos veces--.
Al cambiar el estilo de una hay que copiarlo a la otra:

```sh
perl -0ne 'print $1 if m{(<style>.*?</style>)}s' site/index.html > /tmp/estilo.txt
perl -0pi -e 'BEGIN{local $/; open my $e, "<", "/tmp/estilo.txt"; our $c = <$e>; chomp $c}
              s{<style>.*?</style>}{$c}s' site/en/index.html
```

Va dentro, y no en un `.css` aparte, por lo mismo que los logotipos se publican aparte: un
fichero más que hace falta para que la página se vea bien es un fichero más que se puede
olvidar en el despliegue, y una página sin estilo no se nota hasta que alguien la abre.

### El spot

El video de la portada, en `/var/www/multidrive/video/`. Lo reproducen las dos portadas. En los
README del repositorio **no se reproduce**: github.com no permite video de dominios ajenos
--ver [site/video/LEEME.md](../site/video/LEEME.md#por-que-el-readme-no-lleva-reproductor)--,
asi que ahi va el cartel, que es una imagen, enlazado a la portada.

Son **dos** ficheros: el video y su cartel. El cartel es lo que el `<video>` pone en `poster`
--lo que se ve antes de pulsar-- y ademas es lo que sale en los dos README, enlazado aqui.
Publicar el video sin el cartel deja la portada con un hueco negro y los README con la imagen
rota.

```sh
scp site/video/multidrive-spot.mp4 site/video/cartel.png soporte@multidrive.net:/tmp/
ssh soporte@multidrive.net '
  sudo mkdir -p /var/www/multidrive/video
  sudo install -o www-data -g www-data -m 644 /tmp/multidrive-spot.mp4 /tmp/cartel.png /var/www/multidrive/video/
  rm -f /tmp/multidrive-spot.mp4 /tmp/cartel.png'
```

Van aparte del HTML por lo mismo que los logotipos, solo que aqui el video pesa 1,75 MB:
subirlo en cada despliegue de texto seria mover dos megas para nada.

`site/video/LEEME.md` y `site/video/cartel.svg` **no se publican**: el primero es la
documentacion y el segundo la fuente del cartel.

> **`cartel.png` lo pide tambien GitHub**, desde los dos README. Si se borra del servidor, la
> portada del repositorio se queda con una imagen rota, y eso no se ve desde aqui.

Dos cosas que comprobar despues de publicarlo, porque las dos fallan en silencio:

```sh
curl -sI https://multidrive.net/video/multidrive-spot.mp4 | grep -i "content-type\|accept-ranges"
```

- **`Content-Type: video/mp4`.** Si sale `application/octet-stream`, el navegador no lo
  reproduce: lo descarga. Caddy lo deduce de la extension y acierta solo, pero conviene verlo.
- **`Accept-Ranges: bytes`.** Es lo que permite mover la barra del reproductor sin bajar el
  fichero entero. El `file_server` de Caddy lo hace; un proxy mal puesto delante, no.

> **Al cambiar el video hay que acordarse de esto**, igual que con los logotipos. Y aqui el
> olvido se nota mas: un `index.html` nuevo que pide un video que no esta deja un hueco negro
> arriba de la portada, que es lo primero que se ve.

### Los logotipos de los proveedores

`index.html` los pide de `proveedores/`, asi que esa carpeta tiene que estar o la portada sale
con huecos. Van aparte del HTML porque no cambian casi nunca: subirlos en cada despliegue seria
mover un cuarto de mega para nada.

```sh
scp site/proveedores/*.png soporte@multidrive.net:/tmp/
ssh soporte@multidrive.net '
  sudo mkdir -p /var/www/multidrive/proveedores
  sudo install -o www-data -g www-data -m 644 /tmp/*.png /var/www/multidrive/proveedores/'
```

Solo los de ese nivel. `site/proveedores/originales/` guarda las imagenes grandes de las que
salen las pequenas y **no se publica**: son medio mega que nadie descarga.

> **Al anadir un logotipo nuevo hay que acordarse de esto.** Publicar solo el HTML deja la
> portada pidiendo una imagen que no existe, y eso no se ve hasta que alguien abre la pagina.

No hace falta recargar Caddy para cambios de contenido. Si se toca el `Caddyfile`:

```sh
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl reload caddy   # reload, NO restart: no interrumpe nextcloud.chambaya.net
```

> El `Caddyfile` también sirve Nextcloud en producción. Respaldar siempre antes de editarlo.

## Regenerar los iconos de la marca

Las fuentes están en [`marca/`](../marca): los `.svg` son el original y todo lo demás sale de
ellos. Los `.png` se rasterizan desde `icono.svg` con Edge sin ventana, uno por tamaño:

```sh
msedge --headless --disable-gpu --hide-scrollbars --default-background-color=00000000 \
       --window-size=256,256 --screenshot=marca/png/icono-256.png file:///…/icono-256.html
```

donde el HTML no es más que el `<svg>` con `width` y `height` puestos a ese tamaño y
`body{margin:0}`. El `--default-background-color=00000000` es lo que conserva la
transparencia de las esquinas redondeadas.

El `.ico` es la unión de los de 16, 32, 48 y 256 px, con los PNG **dentro** del contenedor
(lo que Windows llama un icono comprimido, admitido desde Vista). No hay herramienta para eso
en el equipo, así que se escribe la cabecera a mano desde PowerShell: `ICONDIR` de 6 bytes,
una `ICONDIRENTRY` de 16 por tamaño —con 0 donde va 256, que no cabe en un byte— y detrás los
PNG tal cual.

El ZIP que se descarga de `multidrive.net/marca.html` se arma con `Compress-Archive` a partir
de esa misma carpeta, y se publica como un fichero más del sitio.

## El formulario de las pruebas

Es lo único del sitio que no es un fichero estático. Recoge el correo de quien quiere entrar
en las pruebas de Google Play y lo apunta en una lista que se lee por SSH.

| | |
|---|---|
| Script | [`servidor/apuntarse.php`](../servidor/apuntarse.php) → `/var/www/multidrive-cgi/apuntarse.php` |
| Ruta pública | `POST /apuntarse` |
| La lista | `/var/lib/multidrive/betatesters.csv`, `0600`, de `www-data` |
| Límite de ritmo | `/var/lib/multidrive/ritmo`, se puede borrar entero sin perder nada |

### Por qué el script vive fuera de la raíz

Porque dentro de `/var/www/multidrive` una petición a `/apuntarse.php` —que no casa con el
matcher de Caddy— caería en `file_server`, y el servidor devolvería **el código fuente** en
vez de ejecutarlo. Fuera de la raíz eso no puede pasar ni por descuido.

El bloque en el Caddyfile:

```caddyfile
	@apuntarse path /apuntarse
	handle @apuntarse {
		root * /var/www/multidrive-cgi
		rewrite * /apuntarse.php
		php_fastcgi unix//run/php/php8.5-fpm.sock
	}

	handle {
		file_server
	}
```

El `handle` de abajo no es decorativo: sin él, `file_server` atendería también `/apuntarse` y
el matcher no serviría de nada.

### Publicar un cambio del script

```sh
scp servidor/apuntarse.php soporte@multidrive.net:/tmp/
ssh soporte@multidrive.net '
  php -l /tmp/apuntarse.php
  sudo install -o root -g root -m 644 /tmp/apuntarse.php /var/www/multidrive-cgi/apuntarse.php'
```

No hace falta recargar Caddy: php-fpm lee el fichero en cada petición. Recargar sí hace falta
si se toca el Caddyfile, y entonces `caddy validate` **antes** y `systemctl reload caddy`,
nunca `restart`.

### Leer y vaciar la lista

```sh
ssh soporte@multidrive.net 'sudo cat /var/lib/multidrive/betatesters.csv'
ssh soporte@multidrive.net 'sudo truncate -s 0 /var/lib/multidrive/betatesters.csv'
```

### Lo que hace y lo que no

Guarda la fecha, el correo en minúsculas y el idioma de la página. **No guarda la IP**: para
el límite de ritmo usa un resumen irreversible que se borra en una hora. Rechaza lo que no sea
un correo, no apunta dos veces el mismo, y tiene un campo trampa que los robots rellenan y las
personas no ven. Está contado en la política de privacidad, en las dos lenguas, porque recoger
un dato y no decirlo no es una opción.
