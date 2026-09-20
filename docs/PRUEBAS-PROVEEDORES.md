# Pruebas contra proveedores reales

Los tests unitarios corren contra un proveedor en memoria y no prueban nada de lo que rompe
un volumen de verdad: latencia, límites de peticiones, errores que el proveedor traduce a su
manera, contenido que vuelve alterado. Este documento explica cómo apuntar la **suite de
conformidad** a una cuenta real.

```sh
LIBREDISK_TEST_ONEDRIVE_ACCESS_TOKEN=... cargo run -- check
```

Una sola pasada sirve de semáforo, no de medida: contra nubes de consumo, dos ejecuciones
seguidas pueden diferir más de un 40 %. Para medir de verdad, repite:

```sh
cargo run -- check --pasadas 5
```

```
=== gdrive === (5 pasadas)
         comprobacion                        mediana      min      max   disp.
  ok     ida_y_vuelta                        8906 ms     8299    10718     27%
```

La columna `disp.` es `(max − min) / mediana`. Si es alta, cualquier cifra suelta es ruido.
Y una comprobación que falla en unas pasadas y en otras no se marca **`intermitente`**, que
es peor noticia que un fallo limpio: el proveedor cumple a ratos.

La suite escribe y borra objetos en una carpeta dedicada de cada cuenta
(`/libredisk-pruebas` por defecto) y no toca nada más. Si la limpieza falla, lo dice y
nombra el prefijo que quedó por borrar.

## Qué comprueba y por qué

| Comprobación | Qué se rompe si falla |
|---|---|
| `ida_y_vuelta` | Si un fragmento no vuelve byte a byte, el trozo entero es irrecuperable |
| `sobrescritura` | `repair` reescribe fragmentos existentes; sin sobrescritura, `repair` miente |
| `not_found_distinguible` | `scrub` separa "falta el fragmento" de "el proveedor está caído" por este error |
| `borrar_inexistente` | La limpieza tras un `put` parcial borra claves que quizá nunca se escribieron |
| `exists_refleja_el_borrado` | `scrub` pregunta `exists` antes de descargar: si miente, cuenta redundancia que no hay |
| `integridad_binaria` | Los fragmentos van cifrados; cualquier reinterpretación como texto los destruye |
| `clave_con_forma_de_fragmento` | `shard_key` anida dos niveles y usa un punto en el nombre |
| `fragmento_de_4_mib` | Un trozo de 16 MiB en `rs(4,2)` da fragmentos de 4 MiB: el tamaño real de trabajo |
| `seis_escrituras_en_paralelo` | El motor RAID escribe a los *n* proveedores a la vez: aquí asoman los límites de peticiones |
| `listado_enumera_lo_escrito` | `scrub` enumera en vez de preguntar: sin `list`, revisar 1 TB en Drive serían semanas |
| `listado_de_prefijo_vacio` | Un volumen recién creado no tiene nada que enumerar, y eso no es un fallo suyo |

Un fallo aquí no es un detalle de la prueba: es un proveedor que **no sirve como pata de un
RAID**.

## Empezar sin credenciales

Antes de pelearse con OAuth conviene comprobar que el arnés entero funciona:

```sh
LIBREDISK_TEST_FS_ROOT=$(mktemp -d) cargo run -- check
```

No prueba ninguna nube, pero recorre el mismo camino: configuración → suite → limpieza.

## OneDrive

OpenDAL habla con Microsoft Graph. Hacen falta credenciales de una aplicación propia.

1. [portal.azure.com](https://portal.azure.com) → **Microsoft Entra ID** → *App registrations*
   → *New registration*. Elige el tipo de cuenta que corresponda (personal, organización o
   ambas).
2. *Authentication* → *Add a platform* → **Web**, con `http://localhost:8080` como URI de
   redirección. Azure admite `http://` sin TLS solo para `localhost`, así que vale.

   > **No elijas *Mobile and desktop applications*.** Esa plataforma registra la aplicación
   > como **cliente público**, y un cliente público no puede presentar un secreto: el canje
   > del paso 5 falla con `AADSTS700025: Client is public so neither 'client_assertion' nor
   > 'client_secret' should be presented`. OpenDAL manda `client_secret` en cada refresco,
   > así que hace falta un **cliente confidencial**, que es lo que da la plataforma *Web*.
3. *API permissions* → *Microsoft Graph* → **Delegated** → `Files.ReadWrite` y
   **`offline_access`**. Sin `offline_access` no hay token de refresco y la prueba dejará de
   funcionar en una hora.
4. *Certificates & secrets* → *New client secret*.
5. Autoriza en el navegador:

   ```
   https://login.microsoftonline.com/common/oauth2/v2.0/authorize
     ?client_id=<CLIENT_ID>
     &response_type=code
     &redirect_uri=http://localhost:8080
     &response_mode=query
     &scope=offline_access%20Files.ReadWrite
   ```

   Te redirige a `localhost` con `?code=...` en la URL. Copia ese código y canjéalo:

   ```sh
   curl -s https://login.microsoftonline.com/common/oauth2/v2.0/token \
     -d client_id=<CLIENT_ID> -d client_secret=<CLIENT_SECRET> \
     -d grant_type=authorization_code -d code=<CODE> \
     -d redirect_uri=http://localhost:8080 | jq .refresh_token
   ```

```sh
export LIBREDISK_TEST_ONEDRIVE_REFRESH_TOKEN=...
export LIBREDISK_TEST_ONEDRIVE_CLIENT_ID=...
export LIBREDISK_TEST_ONEDRIVE_CLIENT_SECRET=...
export LIBREDISK_TEST_ONEDRIVE_ROOT=/libredisk-pruebas   # opcional
```

> **Atajo para una prueba suelta:** el
> [Graph Explorer](https://developer.microsoft.com/graph/graph-explorer) entrega un token de
> acceso válido una hora. Sirve para `LIBREDISK_TEST_ONEDRIVE_ACCESS_TOKEN` y evita registrar
> nada, pero no sirve para nada que deba seguir vivo mañana.
>
> Graph Explorer **solo ofrece los permisos que necesita la consulta escrita en la barra**.
> Con la de por defecto (`GET /v1.0/me`), `Files.ReadWrite` ni aparece en *Modify
> permissions* y parece que no se puede consentir. Pon
> `GET https://graph.microsoft.com/v1.0/me/drive/root/children`, ejecútala —dará 403—, y
> entonces sí aparece con su botón *Consent*. Si la ventana emergente no se abre, es el
> bloqueador del navegador: el botón se queda sin hacer nada y no da error.
>
> Comprueba el ámbito antes de gastar la hora, que el consentimiento puede quedarse a medias
> sin decirlo:
>
> ```sh
> cut -d. -f2 <<<"$TOK" | tr '_-' '/+' | base64 -d 2>/dev/null | jq -r .scp
> ```

### Dos detalles de OneDrive

- **La raíz se crea sola**, igual que en Drive: no hace falta crear `/libredisk-pruebas` a
  mano, el primer `put` la crea.
- **El borrado probablemente va a la papelera**, igual que en Drive: Graph implementa
  `DELETE /driveItem` como envío a la papelera de OneDrive, no como borrado permanente. No
  está verificado contra la cuenta —`exists_refleja_el_borrado` pasa, que es lo que le
  importa al contrato de `Backend`, y eso no distingue un caso del otro—, así que asume que
  tras cada tanda quedan unos 6 MB en la papelera hasta que la vacíes.

Las nueve comprobaciones pasan en cinco pasadas. Las latencias, y en qué se diferencian de
las de Drive, en [VIABILIDAD.md §6](VIABILIDAD.md#6-medidas-reales-contra-onedrive).

## Google Drive

1. [console.cloud.google.com](https://console.cloud.google.com) → proyecto nuevo → habilita
   la **Google Drive API**.
2. *OAuth consent screen* → tipo **External**, en modo *Testing*, y añade tu propia cuenta
   como usuario de prueba.

   > **En modo *Testing*, Google caduca los tokens de refresco a los 7 días.** Funcionará
   > toda la semana y un día empezará a fallar con `invalid_grant`. No es un fallo de
   > LibreDisk: hay que repetir el paso 4, o publicar la aplicación.
3. *Credentials* → *Create credentials* → **OAuth client ID** → tipo *Web application*, con
   `https://developers.google.com/oauthplayground` como URI de redirección.
4. En el [OAuth Playground](https://developers.google.com/oauthplayground): engranaje →
   *Use your own OAuth credentials* → pega el client ID y el secreto. Autoriza el ámbito
   `https://www.googleapis.com/auth/drive` y canjea el código por tokens.

```sh
export LIBREDISK_TEST_GDRIVE_REFRESH_TOKEN=...
export LIBREDISK_TEST_GDRIVE_CLIENT_ID=...
export LIBREDISK_TEST_GDRIVE_CLIENT_SECRET=...
export LIBREDISK_TEST_GDRIVE_ROOT=/libredisk-pruebas     # opcional
```

> **Atajo para una prueba suelta:** el OAuth Playground entrega un token de acceso válido
> una hora usando sus propias credenciales, sin crear ningún proyecto. Autoriza el ámbito
> `Drive API v3` en el paso 1, canjéalo en el paso 2 y ponlo en
> `LIBREDISK_TEST_GDRIVE_ACCESS_TOKEN`.

### Tres detalles de Drive que cuestan una tarde

Verificados en el código de `opendal-service-gdrive` 0.57, no supuestos:

- **No crees la carpeta raíz a mano.** El servicio llama a `ensure_dir` en el camino de
  escritura, así que crea `/libredisk-pruebas` y los niveles que hagan falta en el primer
  `put`.
- **El borrado va a la papelera, no es permanente.** Drive no tiene borrado directo por
  ruta: OpenDAL usa `files.trash`. La limpieza de la suite deja tu Drive sin esos archivos a
  la vista y `exists` responde correctamente, pero unos 6 MB se quedan ocupando cuota en la
  papelera hasta que la vacíes. Que `exists_refleja_el_borrado` pase confirma algo que
  importa para LibreDisk: el borrado a papelera es indistinguible de uno real desde el
  contrato de `Backend`.
- **Ámbito `drive` vs `drive.file`.** El ámbito `drive.file` (solo los archivos que crea la
  propia aplicación) encaja mejor con LibreDisk y debería ser el de la versión final: limita
  el daño de un token robado a los datos de LibreDisk en vez de a todo el Drive. Al
  cambiarlo hay que comprobar la resolución de rutas: OpenDAL localiza cada carpeta
  buscándola en Drive, y con `drive.file` esas búsquedas solo ven lo que creó la aplicación.
  Para la primera prueba, `drive` da menos problemas.

## S3 y WebDAV

```sh
export LIBREDISK_TEST_S3_BUCKET=... LIBREDISK_TEST_S3_ENDPOINT=... LIBREDISK_TEST_S3_REGION=...
export LIBREDISK_TEST_S3_ACCESS_KEY_ID=... LIBREDISK_TEST_S3_SECRET_ACCESS_KEY=...

export LIBREDISK_TEST_WEBDAV_ENDPOINT=https://nextcloud.example/remote.php/dav/files/usuario
export LIBREDISK_TEST_WEBDAV_USERNAME=... LIBREDISK_TEST_WEBDAV_PASSWORD=...
```

En Nextcloud usa una **contraseña de aplicación** (Ajustes → Seguridad), nunca la del
usuario: se revoca sola sin tocar la cuenta.

> Si el Nextcloud está en tu propia red, prueba desde el servidor o por IP interna. Una red
> sin NAT *hairpin* da tiempos de espera agotados desde la LAN y parece un backend roto
> cuando no lo está.

## Configuración a medias

Un proveedor sin ninguna variable se omite en silencio. Uno **a medio configurar** es un
error ruidoso, a propósito: la forma clásica de tener pruebas que no prueban nada es que un
`Option` se salte todo por una variable mal escrita.

```
Error: configuracion incompleta de 'ONEDRIVE': falta LIBREDISK_TEST_ONEDRIVE_ACCESS_TOKEN,
o bien LIBREDISK_TEST_ONEDRIVE_REFRESH_TOKEN + ..._CLIENT_ID + ..._CLIENT_SECRET
```

## MEGA

**Aplazado.** MEGA no tiene servicio en OpenDAL, así que no entra por el mismo camino que el
resto. Las dos vías posibles, para cuando toque:

- **`rclone serve webdav` por delante.** rclone sí habla MEGA; LibreDisk lo vería como un
  backend WebDAV más. Cero código nuevo, pero exige un demonio de rclone corriendo y añade
  un salto de red, así que MEGA dejaría de ser un proveedor nativo.
- **Crate [`mega`](https://crates.io/crates/mega) (MIT/Apache).** Implementación no oficial
  del protocolo. Daría un backend nativo y binario único, a costa de mantener un cliente de
  terceros y de lidiar con la capa de cifrado propia de MEGA.

## Coste y Términos de Servicio

Estas pruebas hacen peticiones reales: escriben unos 6 MB por proveedor y pasada, y los
borran. Es despreciable en coste, pero cuenta para los límites de peticiones. Si
`seis_escrituras_en_paralelo` falla con errores 429, no es un fallo de LibreDisk: es el
proveedor diciendo cuántas escrituras simultáneas tolera, que es justo el dato que se venía
a buscar.

Sobre el uso de nubes de consumo como almacén de bloques, ver
[VIABILIDAD.md](VIABILIDAD.md).
