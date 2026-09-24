# Despliegue del sitio

`multidrive.net` se sirve **desde el servidor propio** (no GitHub Pages).

- DNS: `multidrive.net` y `www.multidrive.net` → `154.197.1.5`, la misma IP que
  `chambaya.net` y `nextcloud.chambaya.net`.
- Servidor web: **Caddy**, con TLS automático de Let's Encrypt.
- Raíz del sitio: `/var/www/multidrive` (propietario `www-data`).
- Fuente en este repo: [`site/`](../site).
- Acceso: `ssh soporte@chambaya.net`, con `sudo` sin contraseña.

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
scp site/index.html site/privacidad.html soporte@chambaya.net:/tmp/
ssh soporte@chambaya.net '
  sudo install -o www-data -g www-data -m 644 /tmp/index.html /var/www/multidrive/index.html
  sudo install -o www-data -g www-data -m 644 /tmp/privacidad.html /var/www/multidrive/privacidad.html'
```

Respaldar antes lo que se va a pisar: `sudo cp -p fichero fichero.bak-$(date +%Y%m%d-%H%M%S)`.

No hace falta recargar Caddy para cambios de contenido. Si se toca el `Caddyfile`:

```sh
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl reload caddy   # reload, NO restart: no interrumpe nextcloud.chambaya.net
```

> El `Caddyfile` también sirve Nextcloud en producción. Respaldar siempre antes de editarlo.
