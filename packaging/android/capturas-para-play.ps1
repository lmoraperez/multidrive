<#
.SYNOPSIS
    Prepara las capturas del telefono para la ficha de Google Play.

.DESCRIPTION
    Play no acepta las capturas tal como salen del movil. Pide que **el lado largo no pase del
    doble del corto**, y un telefono moderno es mas estrecho que eso: 1080 x 2460 son 2,28 a 1.
    Subidas asi, la consola las rechaza sin decir muy bien por que.

    Lo que se hace aqui es anadir borde a los lados hasta llegar a 2 a 1 justo, **sin recortar
    nada**. Recortar seria perder la barra de estado o el pie, y el pie es donde sale la
    version. El borde va del color de fondo de la aplicacion, asi que no se nota que hay borde.

    No hace falta ImageMagick: basta `System.Drawing`, que viene con Windows.

.PARAMETER Origen
    Carpeta con las capturas. Por defecto, `site/capturas`.

.PARAMETER Destino
    Donde dejar las de Play. Por defecto, una carpeta en el escritorio, porque son para
    subirlas a mano y no para guardarlas en el repositorio: ahi ya estan las originales, y
    tener las dos versiones invita a editar la que no toca.

.PARAMETER Fondo
    El color del borde. Por defecto el fondo de la aplicacion.
#>
[CmdletBinding()]
param(
    [string]$Origen,
    [string]$Destino,
    [string]$Fondo = "#0E1116"
)

$ErrorActionPreference = "Stop"

if (-not $Origen) {
    $Origen = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) "site\capturas"
}
if (-not $Destino) {
    $escritorio = [Environment]::GetFolderPath("Desktop")
    $Destino = Join-Path $escritorio "MultiDrive capturas para Google Play"
}

if (-not (Test-Path $Origen)) { throw "no existe la carpeta de origen: $Origen" }
New-Item -ItemType Directory -Force -Path $Destino | Out-Null

Add-Type -AssemblyName System.Drawing

$color = [System.Drawing.ColorTranslator]::FromHtml($Fondo)
$brocha = New-Object System.Drawing.SolidBrush $color

# El maximo que admite Play. Si algun dia lo cambian, se cambia aqui y no en diez sitios.
$razonMaxima = 2.0

$hechas = 0
foreach ($f in Get-ChildItem -Path $Origen -Filter *.png) {
    $original = [System.Drawing.Image]::FromFile($f.FullName)
    try {
        $alto = $original.Height
        $ancho = $original.Width

        # El ancho minimo para no pasarse de la razon permitida, redondeado hacia arriba y a
        # par: un ancho impar deja el contenido medio pixel descentrado.
        $minimo = [Math]::Ceiling($alto / $razonMaxima)
        if ($minimo % 2 -ne 0) { $minimo++ }
        $anchoNuevo = [Math]::Max($ancho, $minimo)

        $lienzo = New-Object System.Drawing.Bitmap $anchoNuevo, $alto
        $g = [System.Drawing.Graphics]::FromImage($lienzo)
        try {
            $g.FillRectangle($brocha, 0, 0, $anchoNuevo, $alto)
            $g.DrawImage($original, [int](($anchoNuevo - $ancho) / 2), 0, $ancho, $alto)
        } finally {
            $g.Dispose()
        }

        $salida = Join-Path $Destino $f.Name
        $lienzo.Save($salida, [System.Drawing.Imaging.ImageFormat]::Png)
        $lienzo.Dispose()

        $razon = [Math]::Round($alto / $anchoNuevo, 2)
        Write-Host ("{0}: {1}x{2} -> {3}x{2}  (razon {4}:1)" -f $f.Name, $ancho, $alto, $anchoNuevo, $razon)
        $hechas++
    } finally {
        $original.Dispose()
    }
}

$brocha.Dispose()
Write-Host ""
Write-Host "$hechas capturas en $Destino"
