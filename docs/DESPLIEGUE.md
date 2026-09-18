# Despliegue del sitio

`libredisk.chambaya.net` se sirve **desde el servidor propio** (no GitHub Pages).

- DNS: `libredisk.chambaya.net` → `154.197.1.5`, la misma IP que `chambaya.net` y
  `nextcloud.chambaya.net`.
- Servidor web: **Caddy**, con TLS automático de Let's Encrypt.
- Raíz del sitio: `/var/www/libredisk` (propietario `www-data`).
- Fuente en este repo: [`site/`](../site).

Bloque en `/etc/caddy/Caddyfile`:

```caddyfile
libredisk.chambaya.net {
	root * /var/www/libredisk
	encode zstd gzip
	header {
		Strict-Transport-Security "max-age=31536000; includeSubDomains"
		X-Content-Type-Options "nosniff"
		X-Frame-Options "SAMEORIGIN"
		Referrer-Policy "no-referrer"
	}
	file_server
}
```

## Publicar un cambio

```sh
sudo cp site/index.html /var/www/libredisk/index.html
sudo chown www-data:www-data /var/www/libredisk/index.html
```

No hace falta recargar Caddy para cambios de contenido. Si se toca el `Caddyfile`:

```sh
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl reload caddy   # reload, NO restart: no interrumpe nextcloud.chambaya.net
```

> El `Caddyfile` también sirve Nextcloud en producción. Respaldar siempre antes de editarlo.
