# Registrar la aplicación de LibreDisk

Esto es lo que hace que conectar una cuenta de OneDrive o de Google Drive sea **pulsar un
botón y entrar en tu cuenta**, sin pegar nada.

## Las dos cosas que se llaman «credenciales»

Confundirlas es lo que hacía que el formulario pidiera cosas de desarrollador:

| | Qué identifica | De quién es | Dónde vive |
|---|---|---|---|
| `client_id` + `client_secret` | **la aplicación** LibreDisk | del proyecto, iguales para todo el mundo | dentro del binario |
| El token de refresco | **la cuenta** del usuario | de cada usuario | en su fichero de configuración |

Sin aplicación registrada, cada usuario tiene que registrar una suya, que es el estado del
que venimos.

## El secreto no es secreto, y eso está bien

Va dentro de un binario que se distribuye, así que cualquiera puede sacarlo. No es un
descuido: Google lo dice con todas las letras para las aplicaciones instaladas, y es
exactamente el motivo de que exista PKCE. Lo que protege el flujo no es el secreto, es que el
código de autorización no sirve sin un valor que solo conoce el proceso que lo pidió.

Lo que sí trae de verdad compartir una aplicación es que **la cuota es compartida**. Si
alguien abusa, el proveedor puede limitar o suspender el proyecto entero y eso afecta a todos
los que usen esa versión. Por eso se puede sustituir sin recompilar (ver más abajo).

## Google Drive

Todo esto vive bajo **Plataforma de autenticación de Google** (*Google Auth Platform*), en
`console.cloud.google.com/auth/overview`. **No** está en «APIs y servicios → Pantalla de
consentimiento», que es a donde mandan casi todas las guías que se encuentran por ahí:
Google lo reorganizó y lo repartió en cuatro secciones.

Lo que más despista: **los ámbitos ya no están en la pantalla de consentimiento**, están en
«Acceso a los datos» como sección aparte.

1. En [Google Cloud Console](https://console.cloud.google.com/), crea un proyecto.
2. Habilita la **Google Drive API** (esto sí sigue en «APIs y servicios → Biblioteca»).
3. **Marca** (*Branding*): nombre de la aplicación, correo de asistencia y **enlace a una
   política de privacidad**. `libredisk.chambaya.net` ya está publicado y sirve para alojarla.
4. **Público** (*Audience*): tipo **externo**. Mientras esté «en pruebas» solo entran las
   cuentas que añadas como usuarios de prueba.
5. **Acceso a los datos** (*Data Access*) → «Añadir o quitar ámbitos»: añade
   **`.../auth/drive.file`** y **solo ese**. Es el que LibreDisk pide, y es el que evita la
   evaluación de seguridad anual de terceros que Google exige para los ámbitos amplios como
   `.../auth/drive`.
6. **Clientes** (*Clients*) → «Crear cliente» → Tipo de aplicación: **Aplicación de
   escritorio**.

   > **Tiene que ser «Aplicación de escritorio», no «Aplicación web».** Solo ese tipo admite
   > redirección a un puerto cualquiera de `127.0.0.1`, que es lo que necesita el flujo. Un
   > cliente web exige declarar cada redirección por adelantado, y el puerto lo elige el
   > sistema en cada ejecución.

El cliente va el último a propósito: las tres secciones anteriores definen lo que verá el
usuario cuando lo pulse.

Al publicar el proyecto, Google pide una revisión de marca —no de seguridad, al ser un ámbito
no restringido— para quitar el aviso de «aplicación sin verificar» y el límite de usuarios de
prueba.
## OneDrive

1. En [Azure Portal](https://portal.azure.com/) → Microsoft Entra ID → **Registros de
   aplicaciones** → Nuevo registro.
2. Tipos de cuenta admitidos: **cuentas personales y de cualquier organización**, que es lo
   que corresponde al extremo `common` que usa la biblioteca.
3. Plataforma: **Aplicaciones móviles y de escritorio**, con redirección `http://localhost`.
4. Permisos de API: Microsoft Graph, delegados, **`Files.ReadWrite`** y **`offline_access`**.
   Ese par no se elige: es el que OpenDAL usa al renovar, y pedir otro daría un token que
   deja de valer en la primera renovación.
5. Certificados y secretos: crea un **secreto de cliente**.

   > Hace falta aunque sea una aplicación de escritorio, porque la biblioteca que usamos
   > manda siempre `client_secret` al renovar. Está comprobado en el código de
   > `opendal-service-onedrive`.

## Meterlas en el programa

Se leen del entorno **al compilar**, así que el repositorio no las lleva escritas:

```sh
LIBREDISK_APP_GDRIVE_CLIENT_ID=... \
LIBREDISK_APP_GDRIVE_CLIENT_SECRET=... \
LIBREDISK_APP_ONEDRIVE_CLIENT_ID=... \
LIBREDISK_APP_ONEDRIVE_CLIENT_SECRET=... \
cargo build --release
```

Un `build.rs` hace que cambiarlas obligue a recompilar. Sin eso, cambiar la variable y volver
a construir dejaría el binario con las de antes y nadie se enteraría hasta que un usuario no
pudiera conectar su cuenta.

Para comprobar que han entrado:

```sh
libredisk config ver     # "Trae aplicacion registrada para: ..."
```

## Sustituirlas sin recompilar

Quien choque con la cuota compartida, o quiera usar su propia aplicación, pone en su fichero
de configuración:

```
LIBREDISK_APP_GDRIVE_CLIENT_ID=...
LIBREDISK_APP_GDRIVE_CLIENT_SECRET=...
```

Eso gana a lo compilado. Y por encima de las dos manda lo que tenga esa cuenta concreta
(`LIBREDISK_GDRIVE2_CLIENT_ID`), porque quien escribe eso está diciendo «para esta cuenta,
mi aplicación».

## El día de Android

Registrar hoy un cliente de escritorio **no cierra ninguna puerta**: un mismo proyecto puede
tener varios clientes de tipos distintos —escritorio, Android, iOS— compartiendo una sola
pantalla de consentimiento y una sola verificación. Lo que no vale es usar un cliente web en
una aplicación móvil, porque el secreto acabaría dentro del APK.

Lo que sí habrá que resolver ese día, y conviene tenerlo anotado: **un cliente de tipo Android
no tiene secreto**, y la biblioteca que usamos manda siempre `client_secret` al renovar,
tanto en Drive como en OneDrive. O se le enseña a omitirlo, o el lector de Android tendrá que
manejar sus propios tokens en vez de delegar en OpenDAL. El análisis de Android está en
[ROADMAP.md](ROADMAP.md#móvil-android).
