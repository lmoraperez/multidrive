# Hoja de ruta

Cada versión es una pregunta de riesgo que se responde. No se empieza la siguiente sin
haber contestado la anterior.

## v0.1 — "¿Funciona la idea?" (Linux, CLI)

Pila elegida: **Rust + OpenDAL**.

- [x] `trait Backend` + adaptador OpenDAL (memoria, directorio local, S3, WebDAV)
- [x] Cifrado por trozo (XChaCha20-Poly1305 + Argon2id), AAD ligado al id del trozo
- [x] Motor RAID: `concat`, `espejo:n`, `rs:k,m` con Reed-Solomon
- [x] Almacén de trozos direccionado por contenido (BLAKE3) con verificación al leer
- [x] `scrub` y `repair` como operaciones de biblioteca
- [x] Suite de conformidad de proveedores + configuracion por entorno (`multidrive check`)
- [x] Constructores de OneDrive y Google Drive (OAuth2 con token de refresco)
- [x] Google Drive probado contra una cuenta real: las nueve comprobaciones pasan.
      Medidas y consecuencias en [VIABILIDAD.md](VIABILIDAD.md#5-medidas-reales-contra-google-drive)
- [x] OneDrive probado contra una cuenta real. Medidas y comparación con Drive en
      [VIABILIDAD.md](VIABILIDAD.md#6-medidas-reales-contra-onedrive)
- [ ] Mismo trato para S3 y WebDAV
- [x] `list` en el `trait Backend`: en Drive, preguntar por algo que no existe cuesta 6,5 s,
      y eso es justo lo que hacía `scrub`. Un volumen de 1 TB serían 30 días en serie.
      Ya se enumera: `ChunkStore::scrub_many` gasta **una llamada a `list` por proveedor** en
      vez de una por fragmento, y un test que cuenta llamadas lo verifica en vez de suponerlo.
      El coste deja de depender del número de trozos; lo que queda depende del árbol (ver más
      abajo).
      Un proveedor que no se deja enumerar se declara en `ScrubReport::unlistable` en vez de
      darse por vacío: una caída pasajera no puede leerse como pérdida total
- [x] OneDrive vuelve a pasar la suite con las once comprobaciones, ya con `list`. Hizo falta
      subir OpenDAL de 0.57.0 a 0.59.2, y con ello el MSRV de 1.85 a **1.91**: el lister de
      OneDrive en 0.57 consultaba versiones de cada fichero porque leía la *capacidad* del
      servicio en vez de lo que pedía el llamante, y construía la URL con la ruta duplicada.
      En debug reventaba con un panic; en release fallaba al deserializar. Corregido aguas
      arriba en 0.59.2, que es la primera con el arreglo y exige 1.91
- [x] Google Drive vuelve a pasar la suite con las once comprobaciones, ya con `list`. No hizo
      falta tocar nada: el lister de Drive no tenía el fallo del de OneDrive
- [ ] Decidir el anidamiento de `shard_key` en proveedores jerárquicos. Medido en
      [VIABILIDAD.md §7](VIABILIDAD.md#7-enumerar-qué-cuesta-list): OpenDAL recorre el árbol
      nivel a nivel y paga una petición por directorio — ~1,1 s el suelo en las dos nubes,
      3-4 s con contenido debajo. Con `chunks/<2 hex>/` son 257 directorios por proveedor, o
      sea entre 5 y 17 minutos por enumeración completa. El prefijo de dos caracteres existe
      para no acumular millones de objetos en un mismo directorio, que es lo que degrada a los
      almacenes planos; en los jerárquicos el incentivo va justo al revés. Probablemente deba
      depender del proveedor, igual que el tamaño de trozo
- [x] Repetidas las medidas desde esta red (§8). La hipótesis —que el coste de Drive depende
      de cuánto haya en la cuenta— **queda descartada por no ser comprobable con esta
      medición**: entre dos tandas del mismo día, con la misma cuenta, la misma máquina y la
      misma línea, el suelo de Drive se mueve un 29 %. El instrumento no tiene resolución para
      el efecto. De paso cayó una conclusión que §7 daba por robusta: la proporción de ancho
      de banda entre las dos nubes no solo cambia de tamaño, **cambia de sentido** (1,7×,
      1,7×, 0,43×)
- [x] Regla de método para lo que queda de proyecto: **medir proporciones dentro de un
      proveedor, no entre proveedores**. Las de dentro se repiten con dos cifras a través de
      tres tandas, dos redes y dos cuentas —en Drive, preguntar por lo que no existe cuesta
      2,6 veces una operación normal; en OneDrive, 1,0— porque comparten tanda, cuenta y red,
      así que todo lo demás se cancela al dividir. Ninguna proporción **entre** proveedores ha
      sobrevivido a las tres tandas
- [ ] Tamano de trozo por proveedor: 16 MiB se queda corto donde el coste es por operacion
- [x] Trocear ficheros en trozos de 16 MiB. `ChunkStore::put_file` toma un `AsyncRead` y no un
      `&[u8]`, porque un fichero no tiene por qué caber en memoria; devuelve un `Manifest`
      (trozos en orden, longitud y tamaño de trozo). `read_at` baja **solo los trozos que el
      rango toca**, que es lo que hace viable el montaje, y lo verifica un test que cuenta
      descargas. El troceado es de tamaño fijo, no por contenido: insertar un byte al
      principio reescribe el fichero entero, y a cambio localizar un offset es una división
- [x] Manifiestos COW. El árbol vive en los proveedores como nodos inmutables, y cada nodo es
      un trozo más: pasa por el mismo `ChunkStore` que los datos, así que hereda cifrado,
      RAID, direccionamiento por contenido y deduplicación sin código propio. Los nombres de
      fichero quedan cifrados, y hay un test que lo comprueba contra los blobs. Modificar algo
      reescribe la rama hasta la raíz y termina intercambiando un único puntero, que es el
      **único objeto mutable** del sistema: va en espejo a todos los proveedores, no repartido
      con RAID, porque tiene que leerse aunque solo sobreviva uno. Como nada se sobrescribe,
      una raíz vieja sigue siendo legible: eso son **instantáneas sin código dedicado**
- [ ] ~~Índice local (SQLite o `redb`)~~. **Descartado**: la caché de trozos lo hace
      innecesario. Un índice de rutas serviría para evitar la petición por nivel en un
      `lookup` en frío, pero como los nodos del árbol *son* trozos, la caché general ya los
      guarda, y sin esquema, sin migraciones y sin invalidación. El direccionamiento por
      contenido hace que la solución general salga más simple que la especializada
- [x] Caché local en disco. Segundo escalón: memoria, disco, proveedores. Guarda el blob
      **cifrado**, no el contenido: cachear en claro sería más rápido pero dejaría los datos
      del usuario sin cifrar en la máquina local, que es justo lo que este proyecto promete
      que no pasa. Sobrevive al reinicio reconstruyendo su índice del directorio, se verifica
      el hash al leer —una caché corrupta en silencio sería peor que no tenerla— y va separada
      por volumen, porque dos volúmenes pueden tener el mismo trozo sellado con claves
      distintas y se invalidarían mutuamente
- [x] Escritura diferida. `put --diferido` prepara en local y vuelve enseguida; `sync` sube.
      El invariante que la sostiene: **nunca se publica una raíz que apunte a trozos que
      todavía no estén en los proveedores**. Si una subida falla, la raíz se queda esperando y
      el volumen se sigue viendo como antes de la escritura — un estado viejo, pero íntegro.
      Sobrevive a un corte: la cola se recupera del diario al arrancar
- [x] Montaje en lectura-escritura. `multidrive mount Z: --escritura`: el Explorador crea
      carpetas, `Copy-Item` copia, borrar funciona y borrar un directorio con contenido se
      rechaza. Lo escrito queda en la cola de escritura diferida hasta `multidrive sync`, y
      cada descriptor acumula los tramos en un buffer que se fusiona cuando son contiguos —
      sin eso, copiar en bloques de 1 MiB reescribiría el mismo trozo de 16 MiB una vez por
      bloque
- [x] **Amplificación de escritura**, atacada donde importa. `sync` calcula lo alcanzable
      desde la raíz pendiente y **no sube lo que ya nadie referencia**. Medido con la misma
      copia de 3 MB por el montaje: de 13 trozos preparados se suben **4 (3,0 MiB)** y se
      descartan **9 (3,0 MiB)**. Los objetos en el proveedor pasan de **22 a 5** y el espacio
      de **6,1 MB a 3,1 MB**.
      Es seguro descartar porque un trozo preparado se escribió en esta sesión y la raíz que
      ya estuviera publicada es anterior: no puede referenciarlo. Si el recorrido falla no se
      descarta nada, que es el lado prudente del error
- [x] `rename` en el montaje. Renombrar y mover desde el Explorador funcionan. Los dos
      cambios —quitar del origen, poner en el destino— salen en **una sola raíz**: si fueran
      dos publicaciones habría una generación intermedia en la que el fichero no está en
      ninguno de los dos sitios, y un lector podría pillarla. Hay un test que lo comprueba
      abriendo la raíz anterior y la nueva
- [x] Resolución **sin distinguir mayúsculas** en la capa de montaje. El árbol sigue
      distinguiendo —en Linux un sistema de ficheros distingue, y el núcleo sirve a los dos—
      y la traducción vive solo en el adaptador de WinFsp: `canonica` devuelve la forma
      guardada, y al crear se traduce el padre pero se respeta la forma que escribió el
      usuario, que es lo que significa `case_preserved_names`.
      Primero se prueba la ruta tal cual, que es el caso normal porque Windows devuelve los
      nombres como se los dimos al listar; solo si falla se baja por el árbol comparando sin
      distinguir
- [x] `multidrive scrub` y `rebuild` expuestos en la CLI. `scrub` recorre el árbol, enumera
      los proveedores y dice qué le falta a cada trozo y dónde; si un nodo no se deja leer,
      avisa de que el recorrido quedó **incompleto** en vez de dar un resumen tranquilizador
      sobre lo poco que pudo mirar
- [x] El volumen, expuesto en la CLI: `volume create`, `ls`, `put`, `get`, `cat`, `mkdir`,
      `rm`, `volume status`, `scrub` y `rebuild`. Para abrir un volumen sabiendo solo la frase
      de paso hizo falta una **cabecera en claro** (`multidrive-volume.json`, replicada en
      todos los proveedores) con la sal, la topología y el orden de los proveedores: la sal no
      puede ir cifrada porque sin ella no hay clave, y el orden es parte del formato porque el
      índice de un fragmento es la posición de su proveedor
- [x] Capa de reintentos de OpenDAL. No es una optimización, es corrección: probando contra
      cuentas reales se perdió **una escritura suelta a OneDrive** —que dejó un volumen con
      menos redundancia de la pedida— y **una lectura suelta a Drive**, que hizo que un
      `scrub` se saltara medio árbol. Reintentar es seguro en las cinco operaciones del trait
      *porque* los trozos son inmutables

**Criterio de éxito:** montar un volumen `rs(2,1)` sobre 3 backends, copiar 10 GB, borrar un
backend entero, y que el volumen siga íntegro y se reconstruya.

> Estado: **el criterio se cumple a nivel de volumen**, verificado el 2026-09-21 contra
> cuentas reales. Un `rs(2,1)` sobre Google Drive, OneDrive y un directorio local: se sube un
> fichero, se destruye un proveedor entero, el fichero se sigue leyendo byte a byte idéntico,
> `rebuild` regenera los cuatro fragmentos que faltaban en 34 s y `scrub` vuelve a dar
> redundancia completa.
>
> Con dos salvedades que hay que decir: fue **1 MB y no 10 GB**, y **no hay montaje** todavía,
> que es lo que exige el resto de la lista.

## v0.2 — "¿Sobrevive a Windows?"

> **Windows se adelantó a Linux**, invirtiendo el orden original. El motivo es práctico: la
> máquina de desarrollo es Windows, y FUSE no se puede ni compilar ahí. La capa VFS que ambos
> necesitan ya estaba hecha, así que el trabajo era el adaptador.

- [x] Capa de montaje con WinFsp. `multidrive mount Z:` y el volumen aparece como una unidad
      más: el Explorador lista carpetas, `Copy-Item` copia ficheros y escribir se rechaza.
      **Solo lectura**, porque escribir a través del montaje exige antes una caché con
      escritura diferida: sin ella cada `write` de 4 KiB sería una subida síncrona a varias
      nubes, y ningún programa espera eso. **Y es lento**, por la misma razón: cada operación
      va a la nube. Sirve para demostrar que funciona, no para usarlo
- [x] CI multiplataforma (GitHub Actions: linux + windows)
- [x] Instalador MSI. `packaging/windows/multidrive.wxs` más un guión que compila, firma y
      empaqueta. Instala en Archivos de programa y añade la carpeta al PATH de la máquina.
      **Se niega a instalarse sin WinFsp**, con el enlace: una herramienta que se instala y
      luego no puede montar nada es peor que una que avisa. Probado entero en esta máquina:
      instalar, ejecutar, desinstalar sin dejar rastro y reinstalar. Lo construye también CI
      en cada cambio, porque un instalador que solo se compila el día de la publicación es un
      instalador que se descubre roto ese día
- [x] Instalador completo que **encadena el de WinFsp sin modificar**, que es lo único que
      permite su excepción FLOSS: rempaquetar su controlador sería modificarlo. Detecta si ya
      está y no lo toca, y **no lo desinstala** al quitar MultiDrive, porque puede haber otros
      programas usándolo. El guión comprueba la huella SHA-256 y la firma Authenticode del
      paquete de WinFsp antes de empaquetarlo: se redistribuye software de otro y verificar
      que es el suyo es lo mínimo. Probado el ciclo entero, incluido que al desinstalar
      WinFsp se queda
- [x] El MSI suelto sigue existiendo, para quien ya administra WinFsp por su cuenta. Dentro
      del encadenador se oculta su entrada en «Programas y características»: si no, salen dos
      MultiDrive y quitar la equivocada deja la otra registrada sobre algo que ya no está
- [x] La excepción de WinFsp pide su aviso de copyright y el enlace a su repositorio en la
      interfaz y en la documentación. Va en el README, en AVISOS.txt —que se instala junto al
      programa— y en la salida de `multidrive mount`, que es donde se está usando
- [x] WiX fijado a la 5.0.2. De la 6 en adelante hay que aceptar la licencia de la Open
      Source Maintenance Fee, que es de pago para uso comercial; la 5 es la última bajo MS-RL
      sin esa condición. Subir de versión es una decisión de licencia, no de empaquetado
- [ ] Certificado de firma de código. Es lo único de v0.2 que no se resuelve escribiendo
      código: hay que comprar o solicitar un certificado. Las tres vías, lo que bloquea cada
      una y por qué **no hace falta un EV** están en [FIRMA.md](FIRMA.md). El guión ya firma
      binario y MSI en cuanto haya con qué; mientras tanto publica el SHA-256

      **Aplazado a propósito el 28 de septiembre de 2026**, no olvidado: por ahora no va a
      haber certificado. Queda sin marcar porque el aviso de editor desconocido sigue ahí, y
      eso es cierto lo decida quien lo decida. Lo que sí se exige mientras tanto es que el
      sitio lo diga antes de la descarga —ya lo dice, en «Todavía no»— y que el SHA-256 esté
      publicado, que es la única comprobación que le queda a quien baja una copia.

      Consecuencia práctica, para no llevarse el susto más adelante: **v0.2 no se puede dar
      por cerrada**, así que «no se empieza la siguiente sin haber contestado la anterior» se
      rompe aquí a sabiendas. Lo que sigue se hace igual, pero eso es una decisión tomada, no
      un despiste.
- [x] Servicio de Windows, a través del lanzador de WinFsp en vez de escribiendo uno propio:
      ya arranca con la máquina, corre como LocalSystem y está instalado y firmado. `multidrive
      servicio registrar` y el volumen se conecta desde el Explorador como unidad de red.
      **La frase de paso no se guarda en ninguna parte**: Windows la pide al conectar y el
      lanzador la entrega por la entrada estándar, no por la línea de órdenes ni por el
      registro ni por el entorno. Probado entero: frase equivocada rechazada sin montar nada,
      frase correcta montando, leyendo, escribiendo y subiendo desde LocalSystem. Ver
      [SERVICIO.md](SERVICIO.md)
- [x] Fichero de configuración. Las credenciales salían solo del entorno, que es correcto para
      una persona en una consola e imposible para un servicio: LocalSystem no ve el `.env` de
      nadie. Mismas claves que las variables, para poder copiar el `.env` sin traducirlo. El
      entorno sigue ganando al fichero, y `multidrive config ver` dice qué claves está tapando
- [x] `MULTIDRIVE_` como prefijo, en vez de `MULTIDRIVE_TEST_`. Nació cuando esto solo
      configuraba la suite de conformidad; ahora es como se configura el producto. El antiguo
      se sigue aceptando: nadie debería reescribir su configuración porque el proyecto crezca
- [x] Arreglado que el montaje solo funcionara si la carpeta `bin` de WinFsp estaba en el
      PATH. `winfsp_init` hace un `LoadLibrary` del nombre a secas, el instalador de WinFsp no
      toca el PATH, y un servicio no tiene el de ningún usuario: funcionaba en la consola de
      quien se lo había añadido a mano y fallaba en cuanto salía de ahí. Ahora la DLL se
      localiza por el registro, como hacen las propias herramientas de WinFsp. **El mensaje de
      error anterior culpaba al PATH del usuario**, que es como este fallo sobrevivió hasta
      que lo destapó la prueba del servicio
- [x] Caché de lectura en memoria, acotada por bytes y con desalojo del menos usado. **No
      necesita invalidación**, y no por simplificar: los trozos son inmutables y su nombre es
      su contenido, así que una entrada cacheada no puede quedar obsoleta. Lo único mutable
      del volumen es el puntero de raíz, que no pasa por la caché. Medido a través del
      montaje sobre un proveedor local: leer 7,6 MiB pasa de **2 718 ms a 23 ms**, y contra
      una nube de verdad la diferencia es mucho mayor porque la primera lectura son segundos

- [x] Sincronización automática mientras el volumen está montado. Un hilo de fondo sube lo
      pendiente cada `--sync-cada` segundos (30 por defecto) y al desmontar se sube lo que
      quede antes de salir: dejar cosas a medias justo al desmontar sería la peor forma de
      perderlas, porque el usuario cree que ha terminado.
      Trabaja sobre una **copia del almacén**, no sobre el volumen: subir puede tardar
      minutos, y sostener el cerrojo del volumen durante ese rato bloquearía todas las
      escrituras. La copia comparte cachés y cola, así que ve lo mismo sin estorbar
- [x] Arreglada una carrera que la sincronización de fondo destapaba: `sync` leía los trozos
      pendientes y la raíz por separado, así que una escritura colada entre las dos lecturas
      podía hacer que se publicara una raíz cuyos trozos nuevos no estaban subidos — justo el
      invariante que la escritura diferida existe para sostener. Ahora se leen de una vez y
      la raíz solo se publica si no ha cambiado. Hay un test que provoca la carrera de verdad,
      con un proveedor que cuela una escritura en mitad de la subida, y que **falla si se
      quita el arreglo**

## v0.3 — "¿Lo puede usar alguien que no sea yo?"
- [x] **Conectar OneDrive y Drive desde el navegador.** Se pulsa conectar, se abre el
      navegador en la página del proveedor, entras en tu cuenta y das permiso. Ya no se
      teclea ningún token: el flujo de código de autorización con redirección a `localhost`,
      que es el que usan las aplicaciones de escritorio. Tus credenciales **no pasan por
      MultiDrive**: van del navegador al proveedor, y aquí solo llega un código de un solo uso
- [x] Con PKCE, aunque la aplicación lleve secreto y no sea obligatorio. El motivo es
      concreto: el redirector es `localhost`, así que cualquier otro programa de la máquina
      podría quedarse escuchando y recoger el código. Con PKCE ese código no sirve sin un
      valor que solo conoce este proceso y que nunca viaja por el navegador. El reto está
      comprobado contra el vector del apéndice B del RFC 7636
- [x] El `state` se comprueba al volver, y el token de refresco **no pasa por la página**:
      se queda en el proceso hasta que se guarda, y se borra de memoria en cuanto está en el
      fichero. Que la página sea nuestra y corra en el mismo proceso no es motivo para
      pasear por ella acceso permanente a una cuenta
- [x] A Google se le pide `drive.file`, o sea **solo los ficheros que MultiDrive cree**. Pedir
      el Drive entero sería pedir permiso sobre las fotos del usuario para guardar en una
      carpeta, y además Google lo clasifica como ámbito restringido, que obliga a una
      evaluación de seguridad anual de terceros
- [x] **Las credenciales de la aplicación pueden venir dentro del programa**, y entonces la
      ventana no las pide: conectar una cuenta es pulsar el botón y entrar. Son dos cosas
      que se llaman igual y no lo son — el ID y el secreto identifican a *MultiDrive* y son
      iguales para todo el mundo; el token identifica a *tu cuenta*. Pedirle a alguien el ID
      de una aplicación que no es suya es pedirle un dato que no tiene
- [x] Se leen del entorno al compilar, así que el repositorio no las lleva escritas, y un
      `build.rs` hace que cambiarlas obligue a recompilar: sin eso, cambiar la variable y
      reconstruir dejaría el binario con las de antes y nadie se enteraría hasta que un
      usuario no pudiera conectar
- [x] Y se pueden sustituir **sin recompilar**, que es la salida para quien choque con la
      cuota compartida: un proyecto compartido lo es también para los límites del proveedor,
      y si alguien abusa lo pagan todos los de esa versión
- [x] `multidrive config ver` dice si esta copia trae aplicación registrada. Es lo que decide
      si al usuario se le piden dos datos de desarrollador o solo se le pide pulsar un botón
- [x] Fuera los botones de probar y guardar en los proveedores de nube: el de conectar ya
      hace todo. Y donde siguen haciendo falta —los que se rellenan a mano— queda **uno
      solo**: «Probar sin guardar» comprobaba y no hacía nada, que es una versión peor del
      otro, porque el de guardar ya prueba antes y se niega si falla
- [x] Arreglado que `hidden` no ocultara nada sobre un contenedor con `display: flex`. El
      atributo trae su `display: none` de la hoja del navegador y **cualquier regla de autor
      le gana**: los botones se ocultaban y el CSS los volvía a enseñar. Arreglado para toda
      la página de una vez, en vez de recordarlo caso por caso
- [x] La carpeta dentro de la cuenta ya no se pregunta. Es siempre la misma y da igual cómo
      se llame, porque dentro solo hay blobs opacos que nadie va a abrir desde la web del
      proveedor: pedir una decisión a quien no tiene motivo para tomarla es ruido. Se sigue
      pudiendo poner en el fichero de configuración
- [x] Y esa carpeta se llama ya `/multidrive` y no `/multidrive-pruebas`, que era el nombre de
      cuando esto solo configuraba la suite de conformidad. Una carpeta llamada «pruebas» en
      la cuenta de alguien invita a borrarla. **Quien tuviera un volumen en la carpeta vieja
      sin declararla tiene que escribir `MULTIDRIVE_<PROVEEDOR>_ROOT=/multidrive-pruebas`**
- [x] **Conectar una cuenta de nube la guarda, sin pedir otro clic.** Conectar la cuenta *es*
      configurar el proveedor: obligar a pulsar «Probar y guardar» después era trabajo que no
      aportaba nada. Sigue probando antes de escribir, como siempre
- [x] Y la pantalla se refresca sola: el proveedor aparece en la lista de arriba y el
      formulario queda listo para el siguiente, con la ranura que toca ahora. Añadir varias
      cuentas seguidas es el caso normal, no la excepción
- [x] Los botones de probar y guardar solo salen en los tipos que se rellenan a mano
- [x] **Varias cuentas del mismo servicio, de verdad.** Ya cabían por las ranuras numeradas,
      pero faltaban las dos cosas que hacen que sirvan: que el navegador deje **elegir** la
      cuenta, y que no se pueda meter la misma dos veces
- [x] `prompt=select_account` en los dos. Google llevaba solo `prompt=consent`, que enseña la
      pantalla de permisos pero **usa la sesión que ya esté abierta**; OneDrive no llevaba
      `prompt` ninguno. Añadir una segunda cuenta habría reconectado la primera sin avisar
- [x] **Se rechaza añadir una cuenta que ya está.** No es cosmético: dos proveedores sobre la
      misma cuenta se ven como dos, cuentan como dos al elegir topología y se anuncian como
      «aguanta perder uno», pero el día que se pierda esa cuenta se pierden las dos copias a
      la vez. La ventana estaría prometiendo una redundancia que no existe. Comprobado contra
      una cuenta real: intentar repetirla se rechaza nombrando cuál era
- [x] Se compara un **identificador opaco**, no el correo: `permissionId` en Drive y el id de
      la unidad en Graph. Para comparar sirve igual y así no hay que guardar ni imprimir la
      dirección de nadie. Un proveedor que no conteste no bloquea el alta — negarse a añadir
      uno porque otro está caído sería peor que el riesgo que se evita
- [x] La ventana dice cuántas hay ya de ese tipo, con qué nombre se guardará la nueva y que
      las anteriores no se pisan. Los nombres son `gdrive`, `gdrive1`, `gdrive2`… — la primera
      sin número, que es la convención con la que nacieron las carpetas locales
- [x] **`mirror` se llama ahora `espejo` y `stripe`, `union`.** Y los nombres viejos se
      siguen leyendo, que no es una comodidad: la topología va escrita en la cabecera del
      volumen, en los proveedores, y nadie va a ir a reescribir esa cabecera por haberle
      cambiado el nombre a una palabra. Si dejaran de interpretarse, esos volúmenes dejarían
      de abrirse
- [x] Lo que se enseña es la topología **interpretada y no la cadena guardada**: un volumen
      creado antes del cambio tiene `stripe:2` grabado, y mostrarlo tal cual sería usar dos
      vocabularios a la vez. Comprobado con una cabecera escrita a mano con el nombre viejo
- [x] Arreglado que `header::read` se tragara **todos** los errores y los convirtiera en «no
      hay ningún volumen». Un token caducado o un corte de red se presentaban como «aquí no
      hay nada», que es una afirmación muy distinta y de las que asustan: parece que los datos
      han desaparecido. Ahora distingue las tres situaciones —no está, está, no se pudo
      preguntar— y si uno falla pero otro trae la cabecera sigue adelante, que para eso va
      replicada
- [x] **Abrir y retirar el volumen desde la ventana.** El formulario de la frase de paso ya
      estaba, pero no se veía: su visibilidad se decidía **solo dentro de la rama de «hay un
      problema»**, así que quien creaba un volumen y volvía a esa pantalla leía «hace falta la
      frase de paso» sin tener dónde escribirla. Ahora se decide siempre
- [x] Retirar pide **escribir los nombres de los proveedores**, igual que en la línea de
      órdenes, y enseña antes la topología, los proveedores y cuántos fragmentos y bytes se
      van a borrar. Un botón que solo hay que pulsar se pulsa sin leer, y esto no tiene vuelta
      atrás. El botón de borrar no se enciende hasta que lo escrito coincide
- [x] Antes de borrar se desmonta y se cierra el volumen: borrarlo con algo montado dejaría
      una unidad sirviendo algo que ya no existe
- [x] La lógica de retirar vive en el núcleo y la usan las dos interfaces. Dos copias de un
      borrado acaban discrepando en qué borran, y la que discrepa se lleva algo por delante.
      De paso dejó de enumerar dos veces, que contra una nube son minutos duplicados
- [x] **`multidrive volume destroy`**, que no existía: no había forma de retirar un volumen.
      Borra sus fragmentos, su raíz y su cabecera, y **solo eso** — si guardaste otras cosas
      en esa carpeta siguen donde estaban, comprobado con un test contra carpetas locales
- [x] Confirmar no es teclear `--si`: hay que escribir los nombres de los proveedores, que es
      lo que obliga a mirar la lista de lo que se va a borrar. Y antes enseña el recuento real
      de fragmentos y bytes, que cuesta una enumeración y convierte «se va a borrar el
      volumen» en una cifra que se puede mirar
- [x] Se niega si un proveedor no se deja enumerar, y se niega si los proveedores
      configurados no son los del volumen. Las dos por lo mismo: retirarlo a medias y decir
      que terminó dejaría fragmentos sueltos que ya nadie podría identificar
- [x] **Arreglada una fuga de credenciales en los mensajes de error.** El error de OpenDAL
      trae la URL entera de la petición, y Google pone el token de refresco y el
      `client_secret` en la propia URL: cualquier fallo de renovación los publicaba en
      registros, capturas e informes de fallo. Ahora se redactan por **nombre del parámetro**
      y no por aspecto del valor — adivinar qué parece un secreto es una carrera que se
      pierde. Lo encontró un borrado real, y lo fija un test con el texto exacto que apareció
- [x] **Comprobado contra una cuenta real que el permiso mínimo basta.** Con la aplicación
      propia y un único ámbito `drive.file` —solo los ficheros que MultiDrive crea— funciona
      todo: crear volumen, subir, bajar con el SHA-256 idéntico, enumerar y medir. No era
      obvio, porque la biblioteca resuelve rutas buscando carpetas por nombre. Cifras en
      [VIABILIDAD.md §10](VIABILIDAD.md#10-el-permiso-mínimo-de-google-basta)
- [x] Y comprobado que **no ve lo que no creó**: apuntada a la carpeta de un volumen hecho
      por otra aplicación, responde que ahí no hay nada. Es la verificación directa de lo que
      promete la política de privacidad, no una promesa
- [x] Aplicación registrada en **Google Cloud**, cliente de tipo escritorio, y política de
      privacidad publicada en multidrive.net
- [x] **Topología `lineal`: las capacidades desiguales se suman.** Cada trozo va **entero** a
      un proveedor, elegido a partir de su propio identificador en proporción a lo que cada
      uno aporta. Con 15 GB + 5 GB + 1 TB da **1,02 TB**, donde espejo da 5 y unión 15.
      Comparación en [ARQUITECTURA.md](ARQUITECTURA.md#union-contra-lineal)
- [x] La colocación **se calcula, no se apunta**, y eso es lo que mantiene en pie el resto:
      `scrub` sigue enumerando una vez por proveedor y `rescue` sigue reconstruyendo sin
      ninguna base de datos local. Una tabla de colocación *sería* esa base de datos, y
      perderla sería perder el volumen
- [x] Reparto por máximo ponderado y no un resto de división: con el resto, añadir un
      proveedor movería casi **todos** los trozos. Medido en un test: añadir el cuarto a tres
      mueve el 25 % y **solo hacia el nuevo**, nunca entre los viejos
- [x] `tolerated_failures` deja de derivarse de una resta. En `lineal`, «hace falta 1
      fragmento de n» no significa que sobren `n − 1`: significa que cada trozo está en uno
      concreto. La resta habría dicho que aguanta `n − 1` caídas, que es la mentira más cara
      que puede decir esa función porque es el número con el que se elige la topología
- [x] Leer en `lineal` es **una petición**, no una por proveedor. Lo fija un test que cuenta
      llamadas
- [x] **Añadir un proveedor a un volumen lineal que ya existe**, que es el caso de «me he
      quedado sin espacio». Se copia solo el trozo que cambia de dueño y se borra su copia
      vieja **después** de publicar la cabecera; lo demás ni se lee. Los pesos de los que ya
      estaban no se recalculan: cada peso que cambiara movería trozos, y eso convertiría
      añadir una cuenta en reescribir el volumen. Un test mide que se mueve la fracción que
      aporta el recién llegado (±10 %) y que **ningún** trozo cambia de manos entre los
      veteranos; otro escribe doce ficheros, añade una cuenta y los vuelve a leer
- [x] Se dice **antes de empezar** cuánto hay que mover: `fraccion_que_se_mueve`. Si los pesos
      de los que ya estaban cambiaran, esa cuenta no valdría, y entonces devuelve el peor caso
      en vez de un número bonito que no se cumple
- [x] La interfaz de escritorio lo hace con un botón, y se niega **con la unidad montada o con
      algo sin subir**: lo que se escribiera mientras tanto se colocaría con el reparto viejo
      y, al publicar la cabecera, se buscaría donde no está
- [x] **Retirar un proveedor de un volumen lineal** (30 de septiembre de 2026), y es barato: el
      sorteo ponderado tiene, vista del otro lado, la misma propiedad que hace barato añadir. Al
      quitar un peso los únicos trozos que cambian de dueño son **los que tenía el que se va**;
      entre los que quedan no se mueve nada. Retirar una cuenta de cuatro mueve un cuarto, no el
      volumen
- [x] Lo que sí hay que comprobar antes es que **quepa**. Al añadir siempre cabe —lo que se mueve
      va a un sitio recién llegado y vacío—; al retirar hay que meter lo del que se va en lo que
      les quede libre a los demás. `ChunkStore::medir_retirada` lo calcula entero primero y lo
      enseña por proveedor, y si no cabe **no se mueve nada**: descubrirlo a mitad no cuesta solo
      el tiempo, deja el volumen con media mudanza hecha y a quien lo mire sin saber en qué
      estado está
- [x] Y quien no declara cupo **no cuenta como que cabe**: sale aparte, como «no se puede
      asegurar». Decir que cabe apoyándose en una cuenta que no ha contestado es la clase de
      promesa que este programa no hace
- [ ] Solo se puede retirar **el último de la lista**. El índice de cada proveedor es parte de la
      clave de sus fragmentos, así que quitar uno de en medio correría a los de detrás y dejaría
      lo escrito ilocalizable. Se rechaza diciendo eso y enseñando el orden. Hacerlo general
      significa reescribir el volumen entero, que ya sabe hacerse —es el camino de `rs` y
      `espejo`— pero cuesta lo que cuesta y no se ha enganchado
- [x] **Aplicación registrada en Azure** para OneDrive (28 de septiembre de 2026). Ni
      evaluación de ámbitos ni tope de usuarios, al contrario que Google. Conectada una cuenta
      real por el navegador y pasadas **las once comprobaciones** de `multidrive check` con
      nuestras propias credenciales dentro del binario
- [x] Y dos trampas que costaron la primera tarde, ya escritas en
      [APLICACION-OAUTH.md](APLICACION-OAUTH.md) con el motivo: la plataforma tiene que ser
      **Web** y no «móviles y de escritorio», porque mandamos `client_secret` y un cliente
      público que presenta un secreto se rechaza con `AADSTS700025`; y la redirección tiene que
      escribirse con **`localhost`** y no `127.0.0.1`, porque el portal no deja escribir la
      segunda con `http` y la regla de que el puerto se ignora al comparar está documentada
      para la primera. Cada proveedor lleva ahora el suyo en `Oauth::anfitrion`
- [x] **Las credenciales van dentro de la copia que se distribuye.** El guion de empaquetado
      las lee de un fichero que no está en el repositorio y las pone en el entorno de la
      compilación, donde `option_env!` las recoge. Sin ellas el instalador sale igual, pero al
      añadir un proveedor pide un «ID de aplicación», que es lo que nadie que instala un
      programa sabe de qué le hablan. Comprobado de dos formas: buscando los bytes dentro del
      `.exe` construido y con `multidrive config ver`
- [x] **Aplicación publicada en producción** en Google (27 de septiembre de 2026). No hizo
      falta pasar verificación: `drive.file` está clasificado como ámbito no sensible, y los
      no sensibles no la exigen. Es consecuencia directa de haber pedido el permiso mínimo.
      Quita el tope de cuentas de prueba y, sobre todo, **la caducidad de siete días de los
      tokens de refresco**, que es la que muerde sin avisar. Los emitidos durante las pruebas
      no resucitan: esas cuentas hay que volver a enlazarlas una vez
- [x] **Se pueden añadir proveedores desde la ventana.** Elegir tipo, rellenar los campos y
      «Probar y guardar»: escribe, lee, compara y borra contra el proveedor, y **solo si eso
      sale bien** escribe la configuración. Guardar primero y descubrir después que las
      credenciales estaban mal deja un proveedor que no responde, y a partir de ahí todo lo
      que el programa enumere o mida sale con un hueco que hay que ir a explicar
- [x] Lo mismo sin ventana: `multidrive config add FS` toma los valores **por la entrada
      estándar**, no por argumentos. Aquí dentro hay tokens y claves secretas, y lo que se
      escribe en la línea de órdenes acaba en el historial del intérprete y en la lista de
      procesos. Es la misma regla que sigue la frase de paso
- [x] **Todos los tipos numerados, no solo las carpetas locales.** Antes cabía *una* cuenta
      de cada servicio, o sea cuatro nubes como mucho: ni siquiera daban para un `rs(4,2)`,
      que pide seis. La promesa del proyecto es repartir entre cuentas distintas, y con una
      sola cuenta por servicio no se podía cumplir. Ahora `GDRIVE2_*` es otra cuenta de Drive
- [x] Un catálogo único de tipos y campos, del que salen los formularios de la ventana, la
      ayuda de la línea de órdenes y la validación. Antes eran tres copias de la misma lista,
      y una copia de una lista es una lista que acaba discrepando: al numerar todos los tipos,
      el texto de ayuda habría seguido diciendo que solo se numeran las carpetas
- [x] La configuración se escribe en el perfil del usuario y **nunca** en el fichero de
      máquina. `ProgramData` y `/etc` los lee cualquier usuario del equipo, y ahí dentro hay
      acceso permanente a las cuentas. En Unix nace con permisos `0600`. Un servicio sí
      necesita ese fichero, pero eso se decide a mano y a sabiendas
- [x] Escribir la configuración conserva comentarios y orden, reconoce la clave escrita como
      la escribe la gente (`export`, espacios alrededor del `=`) para no dejar dos líneas
      para la misma clave, y va por temporal y renombrado como el resto del proyecto
- [x] La ventana dice con todas las letras que configurar un proveedor **no** lo mete en un
      volumen que ya exista, y manda al botón que sí lo hace. Son cosas de coste muy distinto,
      y decir que ya está dentro sería la mentira más cara de esa pantalla
- [x] **La unidad montada ya no dice 7,9 EB.** Windows y `df` recibían un número fijo y
      enorme, con un comentario que decía que no había forma honesta de dar un tamaño: la
      había dejado de haber en cuanto se supo el cupo de cada proveedor. Ahora declara el del
      proveedor más apretado por `shards_needed`, que es la misma función que usa el motor
      para leer. Comprobado sobre el volumen real de las dos nubes: **15,00 GiB de tamaño y
      3,46 GiB libres**, porque Drive va al 79 %
- [x] Y sin pagar una petición por repintado, que era el motivo real del número falso: mide
      una tarea de fondo cada cinco minutos y deja el resultado en dos enteros atómicos que
      el sistema lee sin bloquear. Mientras no hay medida se vuelve al número enorme a
      propósito —un cero significaría «disco lleno» y ningún programa reintentaría escribir
- [x] **Probado contra dos nubes reales de punta a punta.** Volumen `espejo:2` sobre las
      cuentas de OneDrive y Google Drive: crear, subir 20 MiB, bajarlos y comprobar el
      SHA-256 —idéntico—, enumerar y medir. Cifras y método en
      [VIABILIDAD.md §9](VIABILIDAD.md#9-primer-volumen-real-sobre-dos-nubes-y-qué-dice-cada-una-de-su-espacio)
- [x] Barra de espacio por proveedor, en la ventana y en `multidrive espacio`. Enseña **tres
      cifras que no se mezclan**: lo que cabe en la cuenta, lo que hay ocupado en ella y lo
      que ocupa este volumen. Las dos primeras son una petición al proveedor; la tercera
      obliga a enumerar, así que va detrás de `--medir` y de «Revisar»
- [x] La cifra de lo que ocupa el volumen **sale de la enumeración que ya hacía `scrub`**:
      los seis proveedores mandan el tamaño en la misma respuesta que los nombres, así que no
      cuesta ni una petición más. Lo fija un test que cuenta llamadas
- [x] Un proveedor sin cupo que declarar —un bucket S3 se factura por lo que ocupa— sale como
      «sin límite declarado» y no como una barra vacía; uno que no contesta sale con el
      motivo y no con un cero. Un cero diría que está vacío, que es una afirmación
- [x] El tramo de MultiDrive se recorta a lo que el proveedor dice que hay ocupado. Medido:
      tras escribir 20 MiB, OneDrive seguía dando la misma cifra un cuarto de hora después. Sin el
      recorte, un volumen recién escrito pintaría más del 100 % de una cuenta al 6 %
- [x] Topología `union:n`, que es RAID 0: todo el espacio sumado y el ancho de banda
      repartido, a cambio de que **perder un solo proveedor pierda el volumen entero**. No es
      `rs(k,0)` porque eso no existe —Reed-Solomon necesita al menos un fragmento de
      paridad—, y tenerla aparte hace que «sin redundancia» se lea en el nombre en vez de
      esconderse en un cero
- [x] En la ventana va **la última** de la lista y su precio sale en rojo diciendo que puede
      perderse todo. El orden no es estético: la primera es el valor por defecto, y quien no
      sepa elegir se queda con ella
- [x] Arreglado que con tres proveedores se ofrecieran `espejo:3` y `rs:1,2`, que **son la
      misma opción**: con un único fragmento de datos, cada fragmento de paridad acaba siendo
      una copia entera. Lo cubre un test que compara por lo que aguanta y lo que cuesta, no
      por el nombre, para que cace también el duplicado que traiga la próxima topología
- [x] Arreglado que un `get` fallido dejara fichero. Creaba el destino antes de leer, así que
      una bajada rota dejaba un fichero a medias con pinta de resultado y, si el destino ya
      existía, **destruía una copia buena por intentar bajar otra**. Ahora va a un temporal y
      se renombra al terminar, igual que el diario de escritura
- [x] GUI de escritorio en Tauri, **sin Node**. El frontend es HTML, CSS y JavaScript a mano:
      Tauri no obliga a usar un empaquetador, y meter npm traería un segundo ecosistema de
      dependencias a un proyecto que acaba de fijar `Cargo.lock` precisamente para no tenerlas
      sueltas. También deja la construcción y CI con una sola herramienta
- [x] Enseña lo que a la línea de órdenes le cuesta enseñar: topología, proveedores, margen de
      caídas, sobrecoste, reparto, salud del volumen y —lo que más se malinterpreta— **qué hay
      escrito solo en esta máquina** y todavía sin subir
- [x] La salud reparte los trozos en tres categorías **excluyentes**, y el orden importa: un
      trozo sin fragmentos suficientes ya no se lee, y llamarlo «degradado» sería el peor error
      de esa pantalla, porque degradado invita a esperar y perdido a actuar. Con algún
      proveedor sin enumerar se dice que las cifras son un mínimo y no una medida
- [x] La frase de paso se escribe en la ventana, no sale del proceso, no se guarda y el campo
      se vacía en cuanto deja de hacer falta. Al cerrar el volumen se suelta la clave derivada
- [x] Las capacidades de Tauri quedan en lo mínimo: sin sistema de ficheros, sin red y sin
      shell desde el webview. Todo lo que hace falta pasa por las órdenes de este binario, que
      ya sabe lo que puede tocar
- [x] Asistente de creación de volumen. Lista los proveedores ya configurados, ofrece las
      topologías que caben con ese número **ordenadas de más segura a más aprovechada**, y
      enseña el precio en román paladino: «aguanta perder 2 proveedores, aprovechas el 33 %
      del espacio». El valor por defecto es el prudente, porque quien no sepa elegir se queda
      con el primero. Se deja fuera `m = 0`, que no es redundancia ninguna y no debería poder
      elegirse sin querer de una lista
- [x] La frase se pide **dos veces**. Sin recuperación posible, una errata al teclearla es un
      volumen perdido antes de empezar
- [x] No toca credenciales de nube. Se crea sobre lo que ya esté configurado, y se dice. Un
      formulario que guarda mal un token de OAuth es peor que no tener formulario, y eso va
      aparte
- [x] Montar y desmontar desde la ventana. **Siempre en lectura y escritura**: una unidad que
      no deja escribir no es una unidad, y tenerlo como casilla obligaba a decidir algo que
      nadie quiere decidir al montar. La orden lo sigue admitiendo, que ahí sí tiene sentido.
      Con escritura arranca el sincronizador de fondo, cuyos avisos no van a ninguna consola
      —una ventana no tiene— sino al panel. Cerrar el volumen desmonta primero: dejar una
      unidad sirviendo un volumen que la ventana dice haber cerrado sería mentir en la peor
      dirección
- [x] **Una X al lado de cada proveedor para olvidarlo.** Pregunta antes, y dice lo que no
      hace: no borra nada de la nube. Con el volumen abierto se niega si el proveedor es suyo
      —quitarlo de la configuración no lo saca del volumen, solo lo deja sin poder abrirse— y
      manda a `providers remove`, que sí mueve los trozos. Si la cuenta sigue apareciendo
      porque viene del entorno o del fichero de la máquina, lo dice en vez de fingir
- [x] El recorrido completo de la ventana —crear, montar, escribir por la unidad y desmontar—
      **está probado a mano** sobre un volumen de directorios locales
- [ ] Automatizarlo. Ni `SendKeys` ni un `WM_CHAR` puesto a mano llegan a un WebView2, que
      lleva su propia cadena de entrada, así que el cableado de JavaScript se comprueba a mano
      o no se comprueba. La lógica que no es cableado sí vive en funciones con tests
- [ ] **Estimador de coste mensual** por topología y proveedor
- [x] **Indicador honesto de «guardado localmente» vs «a salvo en la nube»**, en la ventana y
      por fichero. Antes decía «5 trozo(s), 12 MiB SOLO EN ESTA MÁQUINA», que no responde a lo
      que se pregunta quien va a apagar el equipo —no es «cuántos trozos faltan», es «qué
      pierdo si lo apago ahora»— y encima mezcla datos con nodos del árbol: un fichero de un
      byte deja tres trozos. Ahora salen las rutas, de mayor a menor
- [x] Las rutas se **derivan** recorriendo el árbol pendiente, no se anotan: el diario vive en
      claro en el disco local y solo lleva identificadores, que son hashes. Anotar la ruta
      habría metido ahí los nombres de los ficheros de alguien
- [x] Se distingue de que falte **el índice**: los datos pueden estar arriba y la raíz sin
      publicar, y entonces desde otro equipo no se ve nada aunque no falte ningún byte. Son
      dos frases distintas porque son dos cosas distintas
- [x] **Cerrar se niega si queda algo sin subir**, después de intentar subirlo: negarse sin
      haberlo intentado sería pedirle al usuario que resuelva lo que podíamos resolver
      nosotros. Enseña los ficheros y deja elegir, porque a veces no hay más remedio —sin red y
      hay que irse— y a veces es el descuido que cuesta los datos. El foco lo tiene
      «Dejarlo abierto»: un Enter de más no puede ser lo que deje los ficheros aquí
- [x] Y **desmontar hace la última subida**, porque el sincronizador de fondo vive dentro del
      montaje: al desmontar deja de subir nadie, así que dejarlo para después era dejarlo para
      nunca
- [x] El sincronizador de fondo **ya llega a la pantalla**. Guardaba lo que iba haciendo en un
      campo que no leía nadie: un sincronizador que falla en silencio es justo el que no hay
      que tener

## v0.4 — Nubes de consumo
- [ ] OAuth para Dropbox y pCloud (Google Drive y OneDrive ya tienen backend en v0.1)
- [ ] MEGA: sin servicio en OpenDAL. O `rclone serve webdav` por delante, o el crate `mega`
      no oficial. Ver [PRUEBAS-PROVEEDORES.md](PRUEBAS-PROVEEDORES.md#mega)
- [ ] Limitador de tasa por proveedor y *backoff* adaptativo
- [ ] Avisos claros de Términos de Servicio en la UI

## v0.5 — Robustez
- [x] `multidrive rescue`: reconstruir metadatos solo desde los backends. Enumera los trozos,
      los descifra y busca los directorios a los que no apunta nadie: son las raíces
      candidatas, incluidas las de versiones anteriores, y se describen por su contenido para
      poder elegir sin mirar hashes. **No hizo falta la cabecera autodescriptiva por trozo**
      que preveía ARQUITECTURA: la longitud sellada se deduce porque `split` rellena hasta un
      múltiplo de *k*, y el tag de XChaCha20-Poly1305 desambigua. Se añade también
      `multidrive header export`/`import`, que es el respaldo que de verdad importa: la
      cabecera lleva la sal, y sin sal no hay clave
- [x] Montaje FUSE en Linux. `multidrive mount /ruta/vacia`, con las mismas reglas que el lado
      de Windows: bloquear en los hilos del kernel es correcto porque no son del runtime, y
      escribir se acumula por descriptor y se materializa al cerrar. La diferencia que lo
      cambia todo es que **FUSE habla de inodos y WinFsp de rutas**, así que hay una tabla que
      traduce en los dos sentidos y que renombrar tiene que mover entera: si no, renombrar una
      carpeta dejaría a sus hijos apuntando a una ruta que ya no existe. Compila, pasa la
      suite y clippy en Linux
- [x] Y **no hay que traducir mayúsculas**: en Linux el sistema distingue y el árbol también.
      Haber dejado esa traducción en la capa de montaje de Windows, y no más abajo, es lo que
      hace que el adaptador de FUSE no tenga que saber nada del asunto
- [x] **Montado de verdad**, contra el contenedor de `multidrive.net`: lee un fichero
      de 3 MB con el mismo SHA-256 que el original, lo escribe de vuelta igual, crea
      directorios anidados, renombra una carpeta con contenido dentro sin que se pierda lo de
      dentro, y `rmdir` sobre un directorio no vacío responde *Directory not empty*, que es lo
      que hace que `rm -r` baje primero en vez de rendirse. El sincronizador subió solo y al
      desmontar acabó lo que quedaba. Caché al 93 % de aciertos
- [x] El job `fuse` de CI **pasa**: los seis tests montan **sin privilegios** en un runner de
      GitHub, o sea pasando por `fusermount3`. El adaptador vale
- [x] Y su log dio el dato que faltaba: el runner es ubuntu-24.04 con **`fusermount3` 3.14.0**
      y el del servidor es **3.18.2**. Eso movió la sospecha de sitio, y dos pruebas más la
      cerraron. **Como root falla igual**, y ahí la transición setuid no hace nada porque ya
      se es root. **Por un descriptor alto (100) falla igual y con el mismo mensaje**, cuando
      si los estuviera cerrando diría que el `fstat` falló, no que no es un socket.
      Conclusión: **no es el LXC ni el setuid, es ese `fusermount3`.** La hipótesis del
      contenedor, que estuvo escrita aquí un día entero y en tres mensajes, era falsa
- [ ] Queda un cabo, y es de honestidad: solo hay **una** máquina con 3.18.2, así que «esa
      versión» y «esa máquina» no se pueden separar del todo. Haría falta un Ubuntu 26.04 que
      no fuera ese contenedor. No bloquea nada: con 3.14 monta sin privilegios y con 3.18.2
      monta como root, porque `fuser` intenta antes el `mount(2)` directo y ahí el ayudante no
      pinta nada
- [x] Añadir un proveedor para **más seguridad**: `rs(k,m)` pasa a `rs(k,m+1)` sin mover ni un
      fragmento de los que ya hay. Se apoya en una propiedad que no se da por supuesta y tiene
      tres tests propios: ampliar la paridad no cambia los fragmentos anteriores, porque los
      de datos dependen solo de `k` y los de paridad que ya existían salen iguales de la matriz
      nueva. `multidrive providers add --mas-seguridad`
- [x] Y **se puede interrumpir sin consecuencias**, que es lo que lo hace usable en algo que
      tarda horas: mientras la cabecera no cambie, el volumen se lee con la topología vieja y
      los fragmentos viejos, que no se han tocado. Lo único a medias son fragmentos de paridad
      que todavía no lee nadie. Volver a lanzarlo se salta los que ya están. **La cabecera es
      el único punto de compromiso**, y no se publica si falla un solo trozo: prometer una
      redundancia que no está es peor que no ampliar
- [x] Probado de punta a punta: volumen `rs(2,1)` sobre tres directorios, se añade el cuarto,
      **se destruyen dos proveedores enteros** y el fichero vuelve byte a byte idéntico. Antes
      de ampliar, perder dos lo dejaba ilegible, y hay un test que lo comprueba en ese orden:
      sin esa mitad, el test pasaría igual aunque ampliar no hiciera nada
- [x] Proveedores de directorio local numerados (`FS1_ROOT`, `FS2_ROOT`…), que es lo que
      permite montar un volumen con redundancia de verdad sin tocar ninguna nube. El nombre va
      pegado al número de la variable y no a la posición: si dependiera del orden, quitar el
      segundo de tres renombraría al tercero y cada fragmento se leería del sitio equivocado
      sin que la cabecera se enterase
- [x] Añadir un proveedor para **más espacio**: `rs(k,m)` pasa a `rs(k+1,m)`. Cambian las
      fronteras del reparto, así que hay que releer y volver a subir el volumen entero, y por
      eso se escribe en un **reparto nuevo**: la clave de fragmento lleva una marca (`.r2.`) y
      las dos versiones conviven. Mientras la cabecera no cambie, el volumen se lee con la
      vieja y sus fragmentos, que nadie ha tocado. El reparto 0 no lleva marca, para que los
      volúmenes creados antes de que esto existiera se sigan abriendo
- [x] **El reparto viejo no se borra hasta comprobar que el nuevo está entero.** Que una
      subida no diera error dice que se escribió, no que esté ahí, y lo que se iba a borrar es
      lo único que quedaría si no lo estuviera. Se enumera y se cuenta antes de destruir la
      red de seguridad; si falta algo, no se borra nada y se dice
- [x] Retirar un proveedor: `multidrive providers remove <nombre> --menos-seguridad` o
      `--menos-espacio`, simétrico a añadir. Si el que se va es **el último** y `k` no cambia,
      no se mueve ni un fragmento: es publicar la cabecera y tirar lo que sobra. Si es uno de
      en medio, todos los de detrás cambian de índice —y el índice es donde vive el
      fragmento—, así que va por reparto nuevo como `--mas-espacio`
- [x] **Funciona aunque el proveedor ya no responda**, que es el caso que de verdad aprieta:
      la cuenta suspendida. Si no está configurado se sustituye por uno vacío, que es
      exactamente lo que el motor de RAID ya sabe tratar, y se avisa. Lo que **no** se hace es
      fingir que se ha liberado su espacio: borrar contra el suplente «funcionaría» y contarlo
      sería mentir sobre una cuenta a la que ya no llegamos
- [x] Quedarse sin redundancia no puede pasar por descuido: retirar hasta `m = 0` se rechaza y
      se explica que para eso hay que crear un volumen nuevo y copiar
- [ ] Instantáneas y retención
- [ ] macOS (FSKit o NFS loopback; macFUSE tiene problemas de licencia)

## Móvil (Android)

> El diseño de la aplicación —el contrato de `DocumentsProvider`, la frontera con Rust, el
> componente Kotlin de los certificados y por dónde se empieza— está en
> [ANDROID.md](ANDROID.md). Aquí queda el análisis de por qué Android no es un tercer objetivo
> de montaje, y el registro de lo que se ha ido comprobando.

> **Al día 1 de octubre de 2026 ya existe**, en [`android/`](../android), y hace el recorrido
> entero: crear el volumen desde el propio teléfono, conectar cuentas por el navegador, traerse
> las demás con la maleta, abrir, aparecer en la app de Archivos, navegar, leer ficheros por
> rangos, **escribir, crear carpetas, renombrar y borrar**, y recibir lo que se comparte desde
> otra aplicación.
>
> Escribir dejó de estar fuera de alcance cuando llegó el testigo renovable: avisa en vez de
> bloquear, y lo que se publica sobre una raíz que ya cambió se reaplica sobre la nueva. Dos
> equipos en ficheros distintos conviven.
>
> Está publicada en Google Play como `com.jlsoltech.multidrive`, en pruebas internas.
>
> Lo que sigue debajo es el análisis previo, que se conserva porque explica por qué la forma es
> la que es. **Donde diga que el móvil es de solo lectura, ya no lo es.**

Analizado en septiembre de 2026 y verificado contra el código. **No es un tercer objetivo de
montaje**, sino un modelo de integración distinto: por eso tiene sección propia en vez de una
casilla en v0.2.

### Qué se reaprovecha

El núcleo no tiene ataduras al sistema operativo: **cero `cfg(windows)`/`cfg(unix)`** en todo
el workspace, y `std::fs` solo aparece en tests y en el comando `demo`. El `trait Backend` es
`async put/get/delete/exists` sobre bytes.

Comprobado cruzando a `aarch64-linux-android`:

| Crate | Resultado |
|---|---|
| `multidrive-crypto`, `multidrive-raid` | compilan sin NDK |
| `multidrive-core` → `backend` → `opendal` | requiere el NDK: `cc-rs` no encuentra `clang`, lo arrastra `aws-lc-sys` vía `rustls` |

Lo segundo no es un problema de diseño. Se resuelve instalando el NDK y apuntando
`CC_aarch64-linux-android`, o cambiando `rustls` al proveedor `ring`.

### Por qué no se puede montar

Android es Linux, pero **FUSE exige root**: `/dev/fuse` no es accesible para una app normal.
Desde Android 11 el propio sistema usa FUSE para el almacenamiento con ámbito, y justamente
por eso no deja montar a nadie más.

La vía sin root es el **Storage Access Framework**: implementar un `DocumentsProvider`. El
volumen aparece en la app de Archivos y en el selector del sistema, y otras apps leen y
escriben por `ContentResolver`. Es lo que hacen Cryptomator y Round Sync.

Pero SAF **no es un montaje POSIX**: no hay `open()` por ruta ni `mmap`, y una app que use
rutas crudas no ve nada. Para lectura aleatoria haría falta
`StorageManager.openProxyFileDescriptor` (API 26+), que entrega un descriptor con callbacks
— eso sí encaja con un almacén de trozos más caché local.

El coste sobre la arquitectura es real: la capa VFS necesitaría un **tercer consumidor con
semántica distinta**. FUSE y WinFsp se parecen entre sí; SAF no. Más un puente JNI.

### Restricciones propias del móvil

| Problema | Consecuencia |
|---|---|
| Doze y límites de segundo plano | la cola de subida no puede correr sin más: exige `WorkManager` y servicio en primer plano |
| Datos móviles | `rs(4,2)` sube 1,5×, y leer exige *k* fragmentos de *k* proveedores distintos. Política de solo-WiFi obligatoria |
| Batería | Argon2id + Reed-Solomon + XChaCha20 por trozo es intensivo en CPU |

Sumado a las medidas contra Drive (~6,5 s por operación, ver
[VIABILIDAD.md](VIABILIDAD.md#5-medidas-reales-contra-google-drive)), sobre red móvil la
latencia pasa a ser el factor dominante.

### Conflicto con el alcance de v1

v1 es **un solo escritor**. Un móvil añadido a un volumen que el escritorio también escribe
*es* el problema de escritura concurrente que está fuera de alcance. Android empieza por
tanto **en solo lectura**, o espera al *lease* renovable de v0.5.

### Dónde encaja

Ya no por falta de capa VFS: `Volume` tiene `lookup`, `read_dir`, `read_at` y `manifiesto`,
que es justo lo que un `DocumentsProvider` necesita. Ver
[Recuperación ante desastre](ARQUITECTURA.md#recuperación-ante-desastre) para lo que un lector
de solo lectura necesita: el id de la raíz y nada más.

Esta sección decía además que faltaban «índice local y caché». **Ya no es cierto**, y llevaba
un tiempo sin serlo: el índice local está descartado en v0.1 —la caché de trozos lo hace
innecesario— y la caché en disco está hecha. El único bloqueo que queda es el modelo de un
solo escritor, y ese se resuelve empezando en solo lectura.

- [x] Job de CI que cruza `multidrive-crypto` y `multidrive-raid` a `aarch64-linux-android`
      (29 de septiembre de 2026). No prueba que Android funcione —no hay versión de Android—
      sino que **nadie ate el núcleo al sistema operativo sin darse cuenta**. Cuesta segundos
      porque esos dos cruzan sin NDK
- [x] **El núcleo entero cruza a `aarch64-linux-android`** (29 de septiembre de 2026), con el
      NDK r30. `aws-lc-sys` —debajo de `rustls`, debajo de `reqwest` y de OpenDAL— era lo
      único que pedía un compilador de C, tal y como estaba previsto. Se eligió el NDK y no
      mover rustls al proveedor `ring`: el NDK es dependencia **de compilación** y no cambia
      un byte de lo que se distribuye, mientras que cambiar de proveedor de TLS tocaría la
      pila del programa que la gente ya usa en Windows, para habilitar un Android que todavía
      no existe. `rustls-no-provider` está disponible en `reqwest` y en
      `opendal-http-transport-reqwest` por si algún día conviene
- [x] `packaging/android/comprobar.ps1`, que lee la ruta del NDK de `ANDROID_NDK_HOME` en vez
      de llevarla escrita: una ruta absoluta de la máquina de alguien no le sirve a nadie más
- [x] Y la CI lo comprueba en dos pasos separados: `crypto` y `raid` sin NDK, y `backend` y
      `core` con el que trae el runner. Separados porque **dicen cosas distintas**: si falla el
      primero, el fallo es del núcleo; si falla solo el segundo, es del entorno. Si el runner
      deja de traer NDK el paso falla en vez de saltarse solo, porque un paso que se salta y
      sigue en verde enseña a no mirarlo
- [x] El límite portable queda dibujado, y es exactamente el que hace falta: `crypto`, `raid`,
      `backend` y `core`. La línea de órdenes **no** cruza, porque arrastra `fuser`. No es un
      problema que resolver: FUSE no existe para Android, y aunque existiera no se podría usar
- [x] Y **enlaza**, no solo compila: `cargo build --release --target aarch64-linux-android -p
      multidrive-core --example android` produce un ELF de aarch64 de 17 MiB. Compilar y
      enlazar no son lo mismo, y era lo segundo lo que faltaba por ver
- [x] **La aplicación de Android va a necesitar un componente Kotlin**, y esto no estaba
      escrito. `reqwest` arrastra `rustls-platform-verifier`, que en Android busca el almacén
      de certificados del sistema **a través de la JVM**: exige un puntero al `Context` de una
      aplicación y una clase Kotlin en el APK. No hay escapatoria por features: `reqwest`
      0.13.5 solo ofrece `rustls` y `rustls-no-provider`, y los dos traen el verificador de
      plataforma; los certificados empaquetados (`webpki-roots`) no son una opción desde ahí
- [x] Comprobado en el teléfono, y **es peor de lo que parecía**: no devuelve un error de
      certificado, **entra en pánico** —`Expect rustls-platform-verifier to be initialized`—
      así que se lleva el proceso por delante. Un error se maneja; un pánico en mitad de una
      petición de red, en una aplicación, es un cierre inesperado. Conviene saberlo antes de
      escribir la aplicación y no después
- [x] La comprobación se hizo **sin subir credenciales al teléfono**: una petición HTTPS a un
      sitio público basta para saber si el verificador funciona, y falla exactamente igual que
      le fallaría a una cuenta de nube. Subir el fichero de configuración habría puesto tokens
      de refresco en claro en un móvil para averiguar lo mismo
- [x] Por eso la prueba de concepto se parte en dos, y la primera mitad **no toca la red**:
      `crypto/examples/android` mide Argon2id, que es el único coste de abrir un volumen que
      no depende de internet, el único comparable de verdad contra el escritorio —mismo
      trabajo, otra CPU— y el que decide si hace falta otra parametrización o solo otra
      interfaz. En este equipo, en release, la mediana es **22 ms**; en `debug` son 424, que
      es la clase de diferencia que invalidaría la comparación si no se mira
- [x] **Corrida en un teléfono de verdad** (TECNO POVA 5, Android 14, aarch64), el 29 de
      septiembre de 2026. Argon2id: **mediana de 95 ms** sobre siete vueltas, contra 22 ms en
      el equipo de escritorio en release. Poco más de cuatro veces, y por debajo de la décima
      de segundo: **no es un problema, y no hace falta otra parametrización**. Que eso importe
      no es evidente: los parámetros van ligados al volumen, así que si el móvil hubiera
      necesitado otros, un volumen creado en el escritorio se abriría lento en el móvil para
      siempre
- [x] Una primera pasada dio 60 ms y la siguiente 94. Por eso se mide con siete vueltas y se
      da la mediana: una sola pasada es un semáforo, no una medida, y esto ya estaba escrito
      para `multidrive check`
- [x] **Medida la red desde el móvil** (29 de septiembre de 2026), con la aplicación abriendo
      un `lineal:4` de verdad: cabecera 3063 ms, clave 156 ms, abrir 1685 ms, listar la raíz
      1229 ms. **Seis segundos para abrir y listar, de los cuales 156 ms son CPU.** La caché
      deja de ser una optimización y pasa a ser lo que hace usable la aplicación, y Argon2id
      —la sospecha inicial— no es el problema. Detalle en [ANDROID.md](ANDROID.md)

## Llevarse la configuración a otro equipo

Decidido y hecho el 27 de septiembre de 2026: **la maleta**, el bulto sellado en la nube del
propio usuario. Sin servidor, sin cuentas y sin tocar la política de privacidad. Cómo funciona
por dentro, en [ARQUITECTURA.md](ARQUITECTURA.md#la-maleta-llevarse-la-configuración-a-otro-equipo).

- [x] `multidrive config publicar` y `multidrive config recuperar`, y los dos botones
      equivalentes en la ventana
- [x] La frase de paso **no viaja dentro**, con un test que lo comprueba de las dos formas:
      que no sale al abrir la maleta y que sus bytes no están en el bulto
- [x] Sal propia, distinta de la del volumen: el bulto se abre sin que haya volumen —el caso
      de un equipo recién instalado— y no comparte clave con los trozos
- [x] Al traerla, lo que ya está con otro valor no se pisa: se dice y hace falta
      `--sobrescribir`. Pisar una credencial que funciona con otra de una maleta vieja es la
      forma más rápida de quedarse sin acceso a una cuenta
- [x] Se replica en todos los proveedores, como la cabecera, y al recogerla se prueban todas:
      en una cuenta puede haber quedado una de antes de cambiar la frase
- [x] **La maleta se guarda sola al crear el volumen**, con la frase que se acaba de escribir,
      y en la ventana **también en cada apertura**. Abrir exige tener configurados todos los
      proveedores del volumen, así que lo guardado nunca se queda corto. Sin el refresco al
      abrir, añadir una cuenta más tarde dejaba la maleta vieja y el otro equipo con una
      configuración que no abre nada
- [x] Esta línea decía «al crear y en cada apertura» a secas, y era falsa en el terminal:
      `volume create` no guardaba nada, solo lo hacía la ventana. Quien creaba un volumen
      desde la consola se quedaba sin maleta **creyendo que la tenía**, que es peor que no
      tenerla. Ya la guarda también, con un aviso si no puede: el volumen ya existe cuando eso
      corre, y tumbarlo por un fichero de configuración sería cambiar un incordio por un
      desastre. Lo que sigue siendo solo de la ventana es el refresco al abrir, y ahora está
      escrito
- [x] **En la ventana no hay botón de guardarla**: no hace falta. Solo aparece la de traerla,
      y solo cuando la cuenta recién enlazada ya la trae. A mano sigue estando
      `multidrive config publicar`
- [x] **El viaje entero, entre dos equipos y contra nubes reales** (28 de septiembre de 2026),
      sobre un `lineal:4` de OneDrive y tres cuentas de Drive. Se escribió un fichero desde
      este equipo y se leyó desde otro que **solo tenía enlazado OneDrive**: la maleta salió de
      ahí, trajo las tres cuentas de Drive, `volume status` dio `lineal:4` con generación 1, y
      `cat` devolvió el texto tal cual. Es el eslabón que faltaba: hasta aquí estaba visto que
      se recupera la configuración, no que con ella se lean los datos
- [x] Eliminar el volumen está junto a «Abrir volumen» con el volumen cerrado, y junto a
      «Cerrar» con el volumen abierto. Cada botón pinta su confirmación al lado: una
      confirmación que aparece en otra parte de la página es una que nadie lee
- [x] **Probada contra cuentas de nube reales** (28 de septiembre de 2026), sobre un
      `espejo:3` de tres cuentas de Drive. La prueba se hizo con un `%APPDATA%` aparte que
      solo tenía enlazada **una** de las tres, que es lo que de verdad se quería comprobar:
      no vale ver que el fichero sube y baja, hay que ver que con una sola cuenta se recupera
      el resto. Salió: encontró la maleta en `gdrive1`, trajo las otras dos claves, dejó la
      que ya estaba —diciendo que coincidía, en vez de pisarla en silencio— y `volume status`
      abrió el volumen desde ese equipo
- [x] Y antes, con una frase de paso equivocada a propósito: **«hay maleta pero no se pudo
      abrir»**. Distinguir «no hay» de «no es tu frase» importa, porque son dos cosas
      distintas que hacer, y el mensaje lo dice

Lo que sigue sin hacerse, y por qué se descartó, queda abajo: lo que aquí se guarda son las
llaves, y un servidor con las de todo el mundo vale mucho más para un atacante que el portátil
de cualquiera. Las cuentas en la web solo se ganan el coste el día que haya que **compartir**
configuración entre personas, que es otro problema.
Analizado el 25 de septiembre de 2026, sin decidir. Hoy, quien cambia de equipo vuelve a
enlazar cada cuenta a mano: la pregunta era si la aplicación podría guardar volúmenes y
proveedores tras un usuario y una contraseña creados en la web del proyecto.

Lo primero, por lo que se suele pasar de largo: **eso no es «la configuración», son las
llaves**. El fichero lleva `REFRESH_TOKEN`, `CLIENT_SECRET`, `ACCESS_KEY_ID` y
`SECRET_ACCESS_KEY`, o sea acceso permanente a las nubes de quien lo tenga. Un servidor con
las de todos los usuarios vale mucho más para un atacante que el portátil de cualquiera de
ellos, y hoy [la política de privacidad](../site/privacidad.html) —la registrada en la
pantalla de consentimiento de Google— dice que no hay servidores. Eso se puede cambiar, pero
se cambia a sabiendas.

Tres caminos, de menos a más infraestructura:

- **La configuración en tu propia nube.** Al enlazar la primera cuenta, buscar ahí un bulto
  sellado con la frase de paso, al lado de la cabecera del volumen que ya se guarda en cada
  proveedor, y sacar de él los demás proveedores y el volumen. Equipo nuevo: entrar en una
  cuenta y escribir la frase. Mismo resultado que las cuentas, sin servidor, sin correos que
  custodiar y sin tocar la política. Es extender lo que el volumen ya hace.
- **Exportar e importar un fichero cifrado.** Una orden que saca la configuración sellada y
  otra que la mete. Un día de trabajo, cero infraestructura, todo a mano. Sirve de puente
  hacia cualquiera de los otros dos.
- **Cuentas en la web, con cifrado de extremo a extremo.** El modelo de 1Password o Bitwarden:
  de la contraseña salen dos cosas, una que se envía para identificarse y otra que nunca sale
  del equipo y es la que cifra; el servidor guarda un bulto que no puede abrir. Frente a un
  gestor web tiene una ventaja real: al ser aplicación nativa, un servidor comprometido no
  puede colar código. A cambio: un servicio que mantener y respaldar, obligaciones de
  protección de datos por guardar correos, la política reescrita y actualizada en Google
  Cloud, y que **olvidar la contraseña es perder la configuración** —si se pudiera recuperar,
  es que alguien más puede leerla—.

El orden de preferencia es ese mismo. Las cuentas solo se ganan el coste el día que haga falta
**compartir** configuración entre varias personas, que es un problema distinto del de llevarse
la propia de un equipo a otro.

## Carga automática de carpetas desde el móvil — **por analizar**

La función que tiene Nextcloud: se elige una carpeta local —`/DCIM/Camera`— y una remota
—`/Photos/`—, y lo que aparezca en la primera sube sola a la segunda, con condiciones de red y
de batería. Anotado el 1 de octubre de 2026 tras ver su diálogo de ajustes, que es un buen
catálogo de las decisiones que hay que tomar.

### Lo que decide: la frase de paso

Subir sola significa **escribir cuando el usuario no está delante**. Para escribir hace falta la
clave maestra, y hoy esa vive solo en memoria mientras hay sesión abierta, muere con el proceso
y no se guarda en ninguna parte. Es una propiedad de la que la aplicación presume.

Android mata el proceso cuando le conviene —medido en esta misma casa, con teléfonos HONOR y
Xiaomi—, así que «subir mientras la aplicación esté abierta» no es una función, es una
casualidad. Quedan dos caminos:

1. **No guardar nada**: la carga solo ocurre con la aplicación en primer plano. Honesto y casi
   inútil.
2. **Guardar la clave derivada en el Keystore de Android**, respaldado por hardware, atado a este
   teléfono y exigiendo pantalla desbloqueada (`setUnlockedDeviceRequired`). Es lo que hacen
   Nextcloud, Signal y cualquier aplicación que trabaje sin su dueño delante.

El segundo cambia el modelo de amenaza y hay que decirlo sin adornos: **quien tenga el teléfono
desbloqueado tiene el volumen**. A cambio, no es la frase lo que se guarda, así que no sirve
para abrir el volumen en otro equipo ni se puede teclear en ningún sitio.

Tiene que ser una casilla que se activa a sabiendas, no el camino por defecto.

### Lo que encaja bien, si se hace con cuidado

**Cada escritura publica una raíz nueva.** Subir 500 fotos de una en una serían 500
publicaciones de raíz: 500 vueltas a la red, 500 ocasiones de chocar con un escritorio que esté
escribiendo, y una lentitud que no se arregla después.

La pieza para evitarlo **ya existe**: `tree::aplicar_varios` mete varios cambios en una sola
raíz, con rebase y reintento. Hoy es privada y habría que exponerla. Hecho así, una tanda de
fotos es una publicación, y el testigo apenas se nota.

Las condiciones del diálogo de Nextcloud —Wi-Fi no tarificada, solo cargando— son exactamente
las restricciones de `JobScheduler`, que además es parte del sistema: no obliga a meter androidx,
que esta aplicación no usa. Para la subida en sí hace falta un servicio en primer plano con su
notificación, porque puede durar minutos.

### Lo que hay que resolver y no es obvio

- **Cuota de los proveedores.** Una foto es un trozo, y un trozo con `rs:4,2` son seis
  peticiones. 500 fotos son 3000 llamadas a la API de Drive. Hace falta estrangular el ritmo y
  poder reanudar, no solo reintentar.
- **Registro local de lo ya subido.** Sin él, cada pasada vuelve a subirlo todo. Nextcloud lleva
  uno; aquí hace falta igual, y es también lo que responde a «¿qué hacer si el archivo ya
  existe?».
- **Espacio temporal.** El escritor de Android junta el fichero entero en la caché antes de
  subirlo —está documentado y es por cómo se trocea—, así que un vídeo de 4K necesita su tamaño
  libre en el teléfono. De uno en uno y limpiando detrás.
- **Trozos huérfanos.** Si una tanda muere después de escribir trozos y antes de publicar la
  raíz, esos trozos quedan sin referenciar. No rompen nada —son inmutables y por contenido— pero
  ocupan y hoy nadie los recoge.

### Lo que yo dejaría fuera de la primera versión

- **Borrar el original tras subirlo.** Nextcloud lo ofrece. Borrar las fotos de alguien con un
  sistema en pre-alfa no se hace.
- **Sincronización en dos sentidos.** Esto es subida, como la de Nextcloud. Bajar y reconciliar
  es otro problema.

### La interfaz, tomando la de Nextcloud como modelo

Decidido el 1 de octubre de 2026: la carga automática va en un menú de **Configuraciones**
dentro de la aplicación, y el explorador propio pasa a ser la vía principal —no el respaldo que
es hoy—, con la forma del de Nextcloud: barra con buscador y botón de menú, cajón lateral
(Todos los archivos, Favoritos, Recientes, Configuraciones, espacio usado al pie), lista con
icono, tamaño y fecha, botón flotante de añadir y barra inferior.

Eso cambia el rumbo que llevaba la aplicación, que era apoyarse en la app de Archivos del
sistema y tener el listado propio solo para teléfonos sin navegador de documentos. El proveedor
SAF **se queda igual**: es lo que permite que otras aplicaciones abran y guarden en el volumen,
y eso no lo sustituye ninguna pantalla nuestra.

**Lo importante es esto:** la interfaz de Nextcloud es la interfaz de un sistema **con
servidor**. Varias de sus piezas le salen baratas porque hay una base de datos detrás, y aquí
no la hay y además todo está cifrado. Por orden de coste:

| Pieza | Qué hace falta |
|---|---|
| Cajón, barra inferior, lista, botón flotante, Configuraciones | Solo trabajo de interfaz |
| «30,2 GB usado» | Ya existe: `espacio::combinar` y `cuota` |
| Fechas, «Recientes», ordenar por fecha | **Cambio de formato**: `Entry` no guarda ninguna |
| Favoritos | Decidir dónde viven |
| Miniaturas y «Multimedia» | Diseño nuevo: generarlas al subir |
| Buscar | Índice local; hoy sería recorrer el árbol entero |

**Las fechas.** `Entry` guarda `node`, `kind` y `len`, y nada más. Añadir un campo opcional con
`#[serde(default)]` es compatible hacia atrás —los nodos viejos se leen igual— pero lo que ya
está escrito no tiene fecha y no la va a tener hasta que se reescriba. Hay que decidir qué
enseña la lista mientras tanto, y no inventársela.

**Las miniaturas.** Nextcloud las genera en su servidor. Aquí los trozos están cifrados y en la
nube: hacer una miniatura de una foto es bajarla entera y descifrarla, así que una cuadrícula de
cincuenta son cincuenta descargas. La única forma sensata es **generarlas al subir y guardarlas
dentro del volumen**, lo que es espacio de más y una decisión de formato. Sin eso, «Multimedia»
y «Archivos recomendados» no se pueden hacer con honestidad.

**Los favoritos.** O viven en el árbol —y entonces marcar uno publica una raíz, y te siguen de
un equipo a otro— o viven solo en el teléfono —gratis, pero no te siguen—. Los de Nextcloud son
del servidor. Hay que elegir y decirlo.

**Buscar.** Nuestro árbol no tiene índice. Buscar es recorrerlo, y recorrerlo es bajar nodos de
la nube. Para un volumen pequeño pasa; para uno grande hace falta un índice local, que es otra
cosa que mantener al día.

### El escritorio es otro problema, más fácil

«Sincronizar una carpeta con la unidad montada» en el escritorio no necesita casi nada de lo de
arriba: la unidad **ya está montada** como una letra, la sesión está abierta con su frase, y
vigilar una carpeta y copiar es algo que hace cualquier herramienta existente. Si lo que hace
falta es eso, no comparte casi nada con la carga del móvil y conviene no mezclarlos.

## Versión web en multidrive.net — **por analizar**

Un explorador de archivos en el navegador, servido desde `multidrive.net`, sin instalar nada.
Anotado el 1 de octubre de 2026 para decidirlo más adelante; aquí queda lo que ya se sabe para
no volver a razonarlo desde cero.

### Lo que decide, y no es técnico

Las aplicaciones instaladas se entregan **una vez** y van firmadas: quien instala decide confiar
en esa versión, y nadie puede cambiársela sin que el sistema lo note. Una web se vuelve a
entregar **en cada carga**, desde un servidor nuestro. Quien controle `multidrive.net` —nosotros,
quien entre en el servidor, o quien pueda obligarnos— puede servir un JavaScript que se quede
con la frase de paso, y el usuario no tiene con qué compararlo.

Eso choca de frente con lo que la aplicación y el sitio dicen hoy: «ni el proveedor ni nadie más
ve lo que guardas». Con una versión web, multidrive.net pasa a ser alguien que podría verlo.

Las mitigaciones existen y hay que conocer sus límites: **SRI** fija los recursos secundarios
pero no el HTML de entrada, que es el que importa; la **compilación reproducible con hashes
publicados** solo sirve a quien compruebe, y después; una **PWA instalable** se actualiza por
service worker igual, así que no resuelve nada.

Proton y Bitwarden tienen este mismo problema y lo resuelven de la única forma honesta: diciendo
que su cliente web es el **menos** confiable de los suyos. Si esto se hace, la página tiene que
decirlo con todas las letras. Entonces es una opción legítima; callándolo, no.

### Lo técnico, que está más cerca de lo que parece

`multidrive-crypto` y `multidrive-raid` no dependen de nada del sistema: compilan a WASM tal
cual. `multidrive-core` solo toca el sistema a través del *trait* `Backend` —`put`, `get`,
`delete`, `exists`, `list`—, que es una costura limpia.

Haría falta:

1. **Separar el trait `Backend` de la implementación con OpenDAL**, para que el núcleo compile
   sin reqwest. Es el cambio estructural de verdad, y aprovecha igual al resto.
2. **Un backend sobre `fetch`** para Drive y Graph. Las dos APIs admiten CORS, así que los datos
   irían del navegador a la nube **directamente**, sin pasar por nuestro servidor. Esa parte de
   la promesa sí se mantiene.
3. **Sustituir los hilos** del testigo y el refrescador por `spawn_local` y temporizadores.
4. **Argon2id en un Web Worker**, o la pestaña se congela mientras deriva la clave.
5. Los **ficheros grandes**, que es lo espinoso: el escritorio los transmite por trozos y en
   navegador eso exige la File System Access API —solo Chrome y Edge— o que quepan en memoria.
   Safari de iPhone sería el punto débil, que es justo donde más falta haría.

### El premio

El OAuth se vuelve trivial: un redirect normal a `multidrive.net/callback` con PKCE, sin puerto
local, sin `127.0.0.1` y sin navegadores que no llegan a él. Es exactamente el agujero que costó
una tarde de diagnóstico con dos teléfonos prestados.

### Antes de arrancarlo

Depende de qué se busque, y hay caminos más baratos para algunas respuestas:

- **Acceso desde cualquier sitio sin instalar**: es esto, con el aviso por delante.
- **Llegar a iPhone**: es la vía más rápida, pero una aplicación nativa daría mejores garantías.
- **Quitarse de encima el lío del OAuth en Android**: mucho más barato cambiando el redirect a un
  esquema propio.

## Fuera de alcance por ahora
- Escritura concurrente desde varias máquinas (v1 es un solo escritor)
- Uso como disco de sistema o volumen de base de datos
- ~~Móvil como volumen de escritura~~ — **hecho**. Esperaba al *lease* renovable, y ese llegó:
  el testigo avisa en vez de bloquear y lo que se publica sobre una raíz que cambió se reaplica
  sola. Desde el móvil se escribe, se crea, se renombra y se borra. Ver
  [Móvil (Android)](#móvil-android)
- Guardar las llaves de los proveedores en un servidor nuestro sin cifrado de extremo a extremo
