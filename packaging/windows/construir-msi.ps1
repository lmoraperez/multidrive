#Requires -Version 5.1
<#
.SYNOPSIS
    Compila LibreDisk y empaqueta el MSI.

.DESCRIPTION
    Necesita el SDK de .NET y la herramienta WiX:

        winget install Microsoft.DotNet.SDK.8
        dotnet tool install --global wix --version 5.0.2

    La version de WiX esta fijada a proposito. A partir de la 6, WiX exige aceptar la
    licencia de la Open Source Maintenance Fee, que es de pago para uso comercial; la 5 es
    la ultima bajo MS-RL sin esa condicion. Cambiar a la 6 o la 7 es una decision de
    licencia del proyecto, no un detalle de empaquetado: hasta que se tome, se queda aqui.

    Firma el resultado solo si se le dan las variables de entorno correspondientes. Un MSI
    sin firmar se instala igual, pero Windows lo anuncia como de "editor desconocido": ver
    docs/FIRMA.md antes de publicarlo en ningun sitio.

.PARAMETER Version
    Version del paquete. Por defecto, la del workspace. MSI solo admite tres numeros
    (mayor < 256, menor < 256, build < 65536), asi que un sufijo como "-alpha" no cabe: se
    distingue por el nombre del fichero, no por la version interna.
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$Salida = "$PSScriptRoot\..\..\target\msi"
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path "$PSScriptRoot\..\..").Path

# ---------------------------------------------------------------------------------------
# Utilidades
# ---------------------------------------------------------------------------------------

<#
.SYNOPSIS
    Ejecuta un programa externo y falla solo si su codigo de salida lo dice.

.DESCRIPTION
    Los argumentos van en un array con nombre, no sueltos: PowerShell intentaria interpretar
    un '-o' suelto como parametro de esta funcion, no del programa, y fallaria por ambiguo
    con -OutVariable.

    Con $ErrorActionPreference = 'Stop', PowerShell 5.1 convierte **cualquier linea que un
    .exe escriba en stderr** en un error terminante. cargo escribe ahi su progreso, asi que
    una compilacion correcta aborta el script a la primera linea de "Compiling". El codigo de
    salida es la unica senal fiable de si un programa externo fue bien.
#>
function Ejecutar {
    param(
        [Parameter(Mandatory)][string]$Programa,
        [string[]]$Argumentos = @()
    )

    $previo = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Programa @Argumentos
    } finally {
        $ErrorActionPreference = $previo
    }
    if ($LASTEXITCODE -ne 0) { throw "$Programa fallo con codigo $LASTEXITCODE" }
}

<#
.SYNOPSIS
    Firma un fichero, si hay con que.

.DESCRIPTION
    Sin certificado configurado no falla: avisa y sigue. Un desarrollador tiene que poder
    construir el MSI en su maquina sin tener la clave de publicacion, que idealmente no esta
    en ninguna maquina de desarrollo.

    La marca de tiempo no es opcional aunque el parametro lo parezca: sin ella, la firma
    caduca cuando caduque el certificado y los instaladores ya distribuidos dejan de
    validar. Con ella siguen siendo validos para siempre.

    Variables:
      LIBREDISK_SIGN_PFX   ruta al .pfx     (y LIBREDISK_SIGN_PASS, su contrasena)
      LIBREDISK_SIGN_SHA1  huella de un certificado ya instalado en el almacen de Windows,
                           que es lo que se usa con un token hardware o un HSM
#>
function Firmar {
    param([Parameter(Mandatory)][string]$Ruta)

    $pfx  = $env:LIBREDISK_SIGN_PFX
    $sha1 = $env:LIBREDISK_SIGN_SHA1
    if (-not $pfx -and -not $sha1) {
        Write-Warning "sin certificado: $(Split-Path -Leaf $Ruta) queda SIN FIRMAR (ver docs/FIRMA.md)"
        return
    }

    $signtool = Get-Command signtool -ErrorAction SilentlyContinue
    if (-not $signtool) {
        # El SDK de Windows no se pone en el PATH; hay que buscarlo donde lo deja.
        $signtool = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe' -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1
        if (-not $signtool) { throw "hay certificado configurado pero no se encuentra signtool.exe" }
    }
    $signtool = if ($signtool -is [System.IO.FileInfo]) { $signtool.FullName } else { $signtool.Source }

    $sello = if ($env:LIBREDISK_SIGN_TS) { $env:LIBREDISK_SIGN_TS } else { 'http://timestamp.digicert.com' }
    $parametros = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', $sello)
    if ($pfx) {
        $parametros += @('/f', $pfx)
        if ($env:LIBREDISK_SIGN_PASS) { $parametros += @('/p', $env:LIBREDISK_SIGN_PASS) }
    } else {
        $parametros += @('/sha1', $sha1)
    }
    $parametros += $Ruta

    Write-Host "==> firmando $(Split-Path -Leaf $Ruta)"
    Ejecutar -Programa $signtool -Argumentos $parametros
}

# ---------------------------------------------------------------------------------------
# Construccion
# ---------------------------------------------------------------------------------------

if (-not $Version) {
    $cargo = Get-Content "$repo\Cargo.toml" -Raw
    if ($cargo -notmatch '(?m)^\s*version\s*=\s*"([^"]+)"') {
        throw "no se pudo leer la version de Cargo.toml"
    }
    $Version = $Matches[1]
}
# El sufijo de preestreno no cabe en un ProductVersion de MSI.
$Version = ($Version -split '-')[0]

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    throw "falta la herramienta wix. Instalala con: dotnet tool install --global wix"
}

Write-Host "==> compilando libredisk.exe (release)"
Push-Location $repo
try {
    Ejecutar -Programa "cargo" -Argumentos @("build", "--release", "--locked")
} finally {
    Pop-Location
}

$bin = "$repo\target\release"
$exe = "$bin\libredisk.exe"
if (-not (Test-Path $exe)) { throw "no existe $exe" }

# Firmar el binario ANTES de empaquetarlo: al reves quedaria un MSI firmado con un
# ejecutable sin firmar dentro, que es justo lo que mira SmartScreen al ejecutarlo.
Firmar $exe

New-Item -ItemType Directory -Force $Salida | Out-Null
$msi = Join-Path (Resolve-Path $Salida).Path "libredisk-$Version-x64.msi"

Write-Host "==> empaquetando $msi"
Ejecutar -Programa "wix" -Argumentos @(
    "build", (Join-Path $PSScriptRoot "libredisk.wxs"),
    "-arch", "x64",
    "-d", "Version=$Version",
    "-d", "BinDir=$bin",
    "-d", "RepoDir=$repo",
    "-o", $msi)

Firmar $msi

Write-Host ""
Write-Host "MSI listo: $msi"
Get-FileHash $msi -Algorithm SHA256 | Format-List Algorithm, Hash
