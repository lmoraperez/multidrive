# Registrar la aplicación de MultiDrive

Esto es lo que hace que conectar una cuenta de OneDrive o de Google Drive sea **pulsar un
botón y entrar en tu cuenta**, sin pegar nada.

## Las dos cosas que se llaman «credenciales»

Confundirlas es lo que hacía que el formulario pidiera cosas de desarrollador:

| | Qué identifica | De quién es | Dónde vive |
|---|---|---|---|
| `client_id` + `client_secret` | **la aplicación** MultiDrive | del proyecto, iguales para todo el mundo | dentro del binario |
| El token de refresco | **la cuenta** del usuario | de cada usuario | en su fichero de configuración |

Sin aplicación registrada, cada usuario tiene que registrar una suya, que es el estado del
que venimos.

## Qué proveedores necesitan esto

Solo dos de los cinco, y solo uno da trabajo de verdad:

| Proveedor | ¿Registro? | |
|---|---|---|
| Carpeta local | No | No hay nadie a quien pedirle permiso |
| S3 (B2, R2, Wasabi, MinIO) | **No** | El usuario se genera una clave en su propia consola |
| WebDAV / Nextcloud | **No** | Usuario y contraseña, mejor una de aplicación |
| OneDrive | Sí, en Azure | Sin evaluación de ámbitos ni tope de usuarios |
| Google Drive | Sí | Política publicada. Sin verificación, por usar solo un ámbito no sensible |

El motivo de fondo: **OAuth existe para servicios donde el usuario entra con su contraseña y
el programa no debe verla nunca.** S3 y WebDAV no funcionan así — entregan una credencial que
el usuario puede pasar directamente, porque esa credencial ya es suya y no es la identidad de
una aplicación. Ahí no hay tercero, así que no hay nada que registrar ni que verificar.

Dicho de otro modo: **el coste es por proveedor OAuth, no por proveedor**. Si algún día se
añade Dropbox, Box o pCloud, cada uno traerá su propio registro; añadir otro compatible con
S3 no trae ninguno.

En el código esto no está escrito aparte: un tipo necesita registro exactamente cuando su
entrada del catálogo tiene `oauth: Some(...)`.

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
   política de privacidad**. `multidrive.net` ya está publicado y sirve para alojarla.
4. **Público** (*Audience*): tipo **externo**. Mientras esté «en pruebas» solo entran las
   cuentas que añadas como usuarios de prueba.
5. **Acceso a los datos** (*Data Access*) → «Añadir o quitar ámbitos»: añade
   **`.../auth/drive.file`** y **solo ese**. Es el que MultiDrive pide, y es el que evita la
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

### «En pruebas» y «en producción»

> **La aplicación de MultiDrive ya está publicada en producción** (27 de septiembre de 2026).
> Lo que sigue explica los dos estados, porque hace falta para entender los tokens caducados
> de antes y para quien registre su propia aplicación.

Un proyecto recién creado nace **en pruebas**, y ahí pasan dos cosas:

- Google rechaza a cualquier cuenta que no esté en la lista de usuarios de prueba, con un
  `Error 403: access_denied` y el texto «la app se está probando y solo los verificadores
  aprobados por los desarrolladores pueden acceder a ella». Se arregla en **Público →
  Usuarios de prueba**, que tiene tope.
- **Los tokens de refresco caducan a los siete días.** Este es el que muerde sin avisar: la
  cuenta se enlaza, funciona toda la semana, y el lunes siguiente deja de funcionar sin que
  nadie haya tocado nada. Publicar la aplicación deja de emitirlos con fecha de caducidad,
  pero **no resucita los que ya se emitieron**: las cuentas enlazadas durante las pruebas hay
  que volver a enlazarlas una vez.

En producción no hay tope de usuarios ni caducidad a los siete días. Un token de refresco solo
deja de valer si el usuario retira el permiso, si la cuenta pasa seis meses sin usarse, o si se
borra el cliente OAuth en la consola.

### Publicar: no hace falta verificación

`drive.file` está clasificado por Google como ámbito **no sensible**, y su documentación dice
que los ámbitos no sensibles **no obligan a pasar el proceso de verificación**. Así que para
que cualquiera pueda usar MultiDrive basta con **Público → Publicar aplicación**: no hay
revisión que esperar ni tope de usuarios.

Esto es consecuencia directa de haber elegido el permiso mínimo. Con el ámbito amplio `drive`
—que es *restringido*— habría hecho falta además una evaluación de seguridad anual de
terceros.

## OneDrive

1. En [Azure Portal](https://portal.azure.com/) → Microsoft Entra ID → **Registros de
   aplicaciones** → Nuevo registro.
2. Tipos de cuenta admitidos: **cuentas personales y de cualquier organización**, que es lo
   que corresponde al extremo `common` que usa la biblioteca.
3. Plataforma: **Web** —no «Aplicaciones móviles y de escritorio»—, con redirección
   `http://localhost`, sin puerto.
4. Autenticación → **Flujos de cliente público: No**.
5. Permisos de API: Microsoft Graph, delegados, **`Files.ReadWrite`** y **`offline_access`**.
   Ese par no se elige: es el que OpenDAL usa al renovar, y pedir otro daría un token que
   deja de valer en la primera renovación.
6. Certificados y secretos: crea un **secreto de cliente**. El *Valor* solo se ve al crearlo.

### Por qué Web y no escritorio

Parece al revés de lo que es MultiDrive, pero lo decide una cosa concreta: **mandamos
`client_secret`**, y no por gusto. OpenDAL mete `client_secret=` en el cuerpo de cada
renovación, esté vacío o no —`opendal-service-onedrive`, `core.rs`, `refresh_tokens`—, así que
el registro tiene que ser uno que lo acepte.

Bajo «Aplicaciones móviles y de escritorio», Azure clasifica la aplicación como **cliente
público**, y un cliente público que presenta un secreto se rechaza con
`AADSTS700025: Client is public so neither 'client_assertion' nor 'client_secret' should be
presented`. De ahí también el paso 4: ese interruptor es el que de verdad decide.

### Por qué `localhost` y no `127.0.0.1`

Los dos van al mismo sitio, pero la redirección se compara **como texto** contra la
registrada, y aquí no se puede elegir:

- El portal **no deja escribir** una redirección `http` con `127.0.0.1`; hay que editar el
  manifiesto a mano.
- La regla de que **el puerto se ignora al comparar** —lo que nos permite abrir uno al azar en
  vez de reservar uno fijo— Microsoft la documenta para `localhost`.

Por eso cada proveedor lleva escrito el suyo en el catálogo (`Oauth::anfitrion`): `127.0.0.1`
en Google, que lo prefiere y admite cualquier puerto, y `localhost` en OneDrive. Con
`localhost` por delante se escucha además en `::1`, porque en Windows ese nombre resuelve
antes a IPv6 y el navegador se encontraría el puerto cerrado.

## Meterlas en el programa

Se leen del entorno **al compilar**, así que el repositorio no las lleva escritas:

```sh
MULTIDRIVE_APP_GDRIVE_CLIENT_ID=... \
MULTIDRIVE_APP_GDRIVE_CLIENT_SECRET=... \
MULTIDRIVE_APP_ONEDRIVE_CLIENT_ID=... \
MULTIDRIVE_APP_ONEDRIVE_CLIENT_SECRET=... \
cargo build --release
```

Un `build.rs` hace que cambiarlas obligue a recompilar. Sin eso, cambiar la variable y volver
a construir dejaría el binario con las de antes y nadie se enteraría hasta que un usuario no
pudiera conectar su cuenta.

El guion de empaquetado lo hace por su cuenta: lee un fichero `CLAVE=VALOR` que **no está en
el repositorio** —`packaging\windows\credenciales.conf`, que `.gitignore` deja fuera— y lo
pone en el entorno antes de llamar a cargo. Se puede apuntar a otro con
`-Credenciales <ruta>`, y si no hay fichero valen las variables que ya estén en el entorno.
Si no hay ninguna de las dos cosas **avisa y sigue**, igual que hace cuando falta el
certificado de firma: el instalador sale, pero pedirá esos datos al añadir un proveedor.

Para comprobar que han entrado:

```sh
multidrive config ver     # "Trae aplicacion registrada para: ..."
```

## Sustituirlas sin recompilar

Quien choque con la cuota compartida, o quiera usar su propia aplicación, pone en su fichero
de configuración:

```
MULTIDRIVE_APP_GDRIVE_CLIENT_ID=...
MULTIDRIVE_APP_GDRIVE_CLIENT_SECRET=...
```

Eso gana a lo compilado. Y por encima de las dos manda lo que tenga esa cuenta concreta
(`MULTIDRIVE_GDRIVE2_CLIENT_ID`), porque quien escribe eso está diciendo «para esta cuenta,
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
