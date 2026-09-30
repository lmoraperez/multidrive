<#
.SYNOPSIS
    Genera el almacen de claves con el que se firman los APK que se reparten.

.DESCRIPTION
    Se ejecuta **una vez**. Lo que produce son dos ficheros que no estan en el repositorio y
    que hay que guardar en sitio seguro:

      packaging\android\multidrive.jks    el almacen, con la clave privada
      packaging\android\firma.conf        su contrasena

    # Por que importa no perderlos

    Android identifica una aplicacion por su paquete **y su firma**. Una actualizacion firmada
    con otra clave no es una actualizacion: es otra aplicacion, y el sistema se niega a
    instalarla encima. Quien ya tenga MultiDrive tendria que desinstalarla --perdiendo sus
    cuentas enlazadas-- para poder poner la siguiente.

    No hay forma de recuperarlos ni de volver a generarlos iguales. La contrasena sale al azar
    y no se guarda en ningun otro sitio.

    # Y por que importa no repartirlos

    Quien tenga el almacen puede firmar cualquier cosa y hacer que Android la instale encima de
    MultiDrive sin avisar a nadie, porque para el sistema seria la misma aplicacion.

    # Por que autofirmado y no un certificado de una autoridad

    Porque Android no usa autoridades para esto. La firma de una aplicacion no dice quien eres
    ante nadie: dice que la version nueva viene de quien hizo la vieja. Un certificado emitido
    por un tercero no anadiria nada, y no hace falta pagarlo. Es otra cosa distinta del
    certificado de firma de Windows.

.PARAMETER Jdk
    Carpeta del JDK, de donde sale `keytool`. Por defecto, `$env:JAVA_HOME`.

.PARAMETER Forzar
    Rehacer el almacen aunque ya exista. **Deja sin poder actualizar** a todo el que tenga
    instalada una version firmada con el anterior, asi que se pide a proposito.
#>
[CmdletBinding()]
param(
    [string]$Jdk = $env:JAVA_HOME,
    [switch]$Forzar
)

$ErrorActionPreference = 'Stop'

$aqui = Split-Path -Parent $MyInvocation.MyCommand.Path
$almacen = Join-Path $aqui 'multidrive.jks'
$conf = Join-Path $aqui 'firma.conf'

if ((Test-Path $almacen) -and -not $Forzar) {
    Write-Host "Ya hay un almacen en '$almacen'. No se toca."
    Write-Host "Rehacerlo dejaria sin poder actualizar a quien tenga la aplicacion instalada."
    Write-Host "Si de verdad es lo que quieres: firmar.ps1 -Forzar"
    exit 0
}

if (-not $Jdk) {
    Write-Error @'
No se sabe donde esta el JDK. Indicalo con -Jdk o exporta JAVA_HOME:

  $env:JAVA_HOME = "C:\ruta\al\jdk-21"
'@
}
$keytool = Join-Path $Jdk 'bin\keytool.exe'
if (-not (Test-Path $keytool)) {
    Write-Error "No hay keytool en '$keytool'. ¿Es esa la carpeta del JDK?"
}

# La contrasena sale al azar y se escribe directamente en el fichero: no pasa por la linea de
# ordenes ni por la pantalla. Lo que se teclea acaba en el historial del interprete y en la
# lista de procesos, y esta contrasena no se puede cambiar despues sin rehacer el almacen.
$bytes = New-Object byte[] 30
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
$clave = [Convert]::ToBase64String($bytes) -replace '[+/=]', ''

$texto = @"
# Contrasena del almacen de claves con el que se firman los APK de MultiDrive.
# Generada al azar. Si se pierde, el almacen no se puede volver a abrir y habra que
# empezar con otra clave, lo que obliga a desinstalar antes de actualizar.
almacen=$clave
clave=$clave
alias=multidrive
"@
[System.IO.File]::WriteAllText($conf, $texto, [System.Text.UTF8Encoding]::new($false))

if (Test-Path $almacen) { Remove-Item $almacen -Force }

# 10950 dias son treinta anos. Google Play exige que la validez pase de 2033 y lo razonable es
# que pase de largo: un certificado de firma caducado no se renueva, se cambia, y cambiarlo es
# justo lo que no se puede hacer sin romper las actualizaciones.
& $keytool -genkeypair -v `
    -keystore $almacen `
    -alias multidrive `
    -keyalg RSA -keysize 4096 `
    -validity 10950 `
    -storepass $clave -keypass $clave `
    -dname "CN=MultiDrive, O=MultiDrive, C=VE" | Out-Null

if ($LASTEXITCODE -ne 0) { Write-Error "keytool fallo con codigo $LASTEXITCODE" }

Write-Host "Almacen creado en '$almacen'."
Write-Host "Su contrasena esta en '$conf'. Ninguno de los dos va al repositorio."
Write-Host ""
Write-Host "GUARDA UNA COPIA DE LOS DOS, fuera de este equipo."
Write-Host "Sin ellos no se pueden publicar actualizaciones que se instalen encima."
