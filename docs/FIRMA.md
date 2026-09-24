# Firma de código en Windows

Estado: **el MSI y el ejecutable se publican sin firmar.** Este documento explica qué cuesta
eso, cuáles son las opciones reales y por qué la decisión está parada en algo que no se
resuelve escribiendo código.

## Qué pasa sin firmar

Tres cosas, de menos a más grave:

1. El aviso de UAC del instalador dice **«Editor desconocido»** en rojo, en vez del nombre
   del proyecto en azul.
2. **SmartScreen** bloquea la primera ejecución con una pantalla azul a pantalla completa
   cuyo botón para continuar está escondido detrás de «Más información». La mayoría de la
   gente no sigue.
3. Un binario sin firmar no se distingue de uno manipulado. Para una herramienta que **pide
   una frase de paso y maneja las claves de todos tus datos**, eso no es un detalle
   cosmético: es exactamente el ataque que más daño haría.

Lo tercero es la razón por la que esto está en el roadmap y no en la lista de «estaría bien».

## Lo que MultiDrive **no** necesita

Un certificado **EV**, que es el caro y el que exige validación de organización.

Los EV hacen falta para firmar **controladores de modo núcleo**, porque desde Windows 10 el
portal de hardware de Microsoft solo acepta envíos firmados con EV. MultiDrive no trae ningún
controlador: el que monta la unidad es el de **WinFsp**, que ya viene firmado por su
proyecto, y `multidrive.exe` es un proceso de usuario corriente.

Un certificado **OV** basta. La diferencia práctica es que con EV la reputación de SmartScreen
es inmediata y con OV hay que ganársela con descargas; para un proyecto que empieza, esa
ventaja se paga varias veces su precio.

## Las tres vías

Desde junio de 2023 el CA/Browser Forum exige que la clave privada de cualquier certificado
de firma de código viva en **hardware certificado** (FIPS 140-2 nivel 2 o equivalente). Ya no
existe el `.pfx` que te descargabas y guardabas en el portátil. Eso cambia las opciones:

| Vía | Coste | Qué exige | Dónde encaja |
|---|---|---|---|
| **Azure Artifact Signing** (antes Trusted Signing) | ~10 $/mes (nivel básico) | Identidad validada. Organizaciones: EE. UU., Canadá, UE, RU. **Particulares: solo EE. UU. y Canadá** | La más barata *si la geografía cuadra* |
| **SignPath Foundation** | Gratis | Licencia OSI sin doble licencia comercial, **repositorio público**, proyecto ya publicado y mantenido | MultiDrive es AGPL-3.0: cumple todo menos que el repositorio sigue siendo privado |
| **CA comercial (OV)** | Cientos de € al año, más el token | Validación de la organización y un token hardware o un HSM | La vía que siempre está disponible |

### Lo que bloquea cada una

**Azure** es la más barata y la que menos mantenimiento tiene, pero la disponibilidad por
países es el problema: como particular hay que estar en EE. UU. o Canadá, y como organización
en EE. UU., Canadá, la UE o el Reino Unido. Microsoft ha dicho que irá ampliando. **Hay que
comprobar la lista vigente antes de descartarla**, porque cambia.

**SignPath** es gratis y es justo el caso de uso para el que existe, pero pide repositorio
público y una versión ya publicada. Hoy el repositorio de MultiDrive es privado, así que esta
vía **depende de publicarlo**, que es una decisión de proyecto, no de empaquetado.

**Una CA comercial** funciona siempre, y es la única que no depende ni de dónde estés ni de
cuándo se publique el repositorio. Es también la que cuesta dinero todos los años.

## Cómo firma la construcción

`packaging/windows/construir-msi.ps1` firma **el ejecutable antes de empaquetarlo** y después
**el MSI**. Ese orden importa: al revés quedaría un instalador firmado con un binario sin
firmar dentro, y SmartScreen mira el binario al ejecutarlo, no al instalarlo.

Sin certificado configurado no falla: avisa y sigue. Quien desarrolla tiene que poder
construir el MSI sin tener la clave de publicación, que idealmente no está en ninguna máquina
de desarrollo.

```powershell
# Con un certificado en el almacén de Windows (token hardware, HSM, Azure)
$env:MULTIDRIVE_SIGN_SHA1 = "<huella del certificado>"

# O con un fichero, que ya solo vale para certificados viejos o de prueba
$env:MULTIDRIVE_SIGN_PFX  = "C:\ruta\al\certificado.pfx"
$env:MULTIDRIVE_SIGN_PASS = "<contrasena>"

# Opcional: otro servidor de sellado de tiempo
$env:MULTIDRIVE_SIGN_TS   = "http://timestamp.digicert.com"

.\packaging\windows\construir-msi.ps1
```

El **sellado de tiempo** no es opcional aunque parezca un parámetro más. Sin él, la firma deja
de validar cuando caduque el certificado, y los instaladores ya repartidos se convierten en
«editor desconocido» de un día para otro. Con él siguen siendo válidos para siempre.

## Firmar el instalador encadenado

Un paquete encadenado no se firma como un fichero cualquiera. Lleva el motor y la carga útil
en el mismo ejecutable, y firmarlo de golpe rompe la firma en cuanto el motor se extrae a sí
mismo para ejecutarse. Hay que separar el motor, firmarlo, volver a unirlo y firmar el
conjunto:

```powershell
wix burn detach   instalador.exe -engine motor.exe
signtool sign ... motor.exe
wix burn reattach instalador.exe -engine motor.exe -o instalador.exe
signtool sign ... instalador.exe
```

`construir-msi.ps1` ya lo hace. Está aquí porque es el paso que se olvida y produce un
instalador que Windows rechaza a medio instalar, no al empezar.

## Mientras tanto

El script imprime el **SHA-256** del MSI al terminar. Publicarlo junto a la descarga no
sustituye a una firma —quien pueda alterar el fichero probablemente pueda alterar la página—
pero permite comprobar una copia bajada de un tercero contra el número publicado en el
repositorio, que es una cadena distinta.

Es un parche. La casilla del roadmap no se marca hasta que haya firma de verdad.
