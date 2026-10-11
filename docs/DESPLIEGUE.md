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

```sh
scp site/index.html site/privacidad.html soporte@multidrive.net:/tmp/
ssh soporte@multidrive.net '
  sudo install -o www-data -g www-data -m 644 /tmp/index.html /var/www/multidrive/index.html
  sudo install -o www-data -g www-data -m 644 /tmp/privacidad.html /var/www/multidrive/privacidad.html'
```

Respaldar antes lo que se va a pisar: `sudo cp -p fichero fichero.bak-$(date +%Y%m%d-%H%M%S)`.

### Los logotipos de los proveedores

`index.html` los pide de `proveedores/`, asi que esa carpeta tiene que estar o la portada sale
con huecos. Van aparte del HTML porque no cambian casi nunca: subirlos en cada despliegue seria
mover un cuarto de mega para nada.

```sh
scp -r site/proveedores soporte@multidrive.net:/tmp/
ssh soporte@multidrive.net '
  sudo mkdir -p /var/www/multidrive/proveedores
  sudo install -o www-data -g www-data -m 644 /tmp/proveedores/*.png /var/www/multidrive/proveedores/'
```

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
