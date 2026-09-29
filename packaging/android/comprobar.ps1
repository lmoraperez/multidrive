<#
.SYNOPSIS
    Comprueba que el nucleo sigue compilando para Android.

.DESCRIPTION
    No construye ninguna aplicacion: **no hay version de Android todavia**. Lo que hace es
    cruzar a `aarch64-linux-android` los crates que esa version usaria, para que la
    portabilidad no se pudra sin avisar.

    Cuatro crates y no el workspace entero, y el que falta es informativo: `multidrive` (la
    linea de ordenes) arrastra `fuser`, y FUSE no existe para Android. Tampoco haria falta:
    Android no deja montar --`/dev/fuse` no es accesible sin root-- asi que la integracion
    seria un `DocumentsProvider`, que se apoya en `multidrive-core` y nada mas.

    La ruta del NDK se lee del entorno y no se escribe aqui: una ruta absoluta de la maquina
    de alguien no le sirve a nadie mas.

.PARAMETER Ndk
    Carpeta del NDK. Por defecto, `$env:ANDROID_NDK_HOME`.

.PARAMETER Api
    Nivel de API de Android contra el que compilar. 26 por defecto, que es el minimo en el que
    existe `StorageManager.openProxyFileDescriptor`, que es lo que haria falta para leer por
    rangos desde el selector de ficheros del sistema.

.EXAMPLE
    $env:ANDROID_NDK_HOME = "C:\Users\quien-sea\Android\android-ndk-r30"
    .\packaging\android\comprobar.ps1
#>
[CmdletBinding()]
param(
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [int]$Api = 26
)

$ErrorActionPreference = 'Stop'

if (-not $Ndk) {
    Write-Error @'
No se sabe donde esta el NDK.

  $env:ANDROID_NDK_HOME = "C:\ruta\al\android-ndk-r30"

Se descarga de https://developer.android.com/ndk/downloads. Es dependencia de COMPILACION:
no entra en nada de lo que se distribuye, y se quita borrando la carpeta.
'@
}

if (-not (Test-Path $Ndk)) { Write-Error "El NDK no esta en '$Ndk'." }

# En Windows los envoltorios son .cmd; en Linux y macOS no tienen extension. Se mira cual hay
# en vez de decidirlo por el sistema: el que manda es el NDK que haya delante.
$bin = Join-Path $Ndk 'toolchains/llvm/prebuilt/windows-x86_64/bin'
if (-not (Test-Path $bin)) {
    $bin = Join-Path $Ndk 'toolchains/llvm/prebuilt/linux-x86_64/bin'
}
if (-not (Test-Path $bin)) { Write-Error "No encuentro las herramientas dentro de '$Ndk'." }

$clang = Join-Path $bin "aarch64-linux-android$Api-clang.cmd"
if (-not (Test-Path $clang)) { $clang = Join-Path $bin "aarch64-linux-android$Api-clang" }
if (-not (Test-Path $clang)) {
    Write-Error "Ese NDK no trae compilador para la API $Api. Prueba con otro -Api."
}

$ar = Join-Path $bin 'llvm-ar.exe'
if (-not (Test-Path $ar)) { $ar = Join-Path $bin 'llvm-ar' }

Write-Host "==> NDK   $Ndk"
Write-Host "==> API   $Api"

# `aws-lc-sys` --debajo de rustls, debajo de reqwest y de OpenDAL-- es lo unico que pide un
# compilador de C. Es la razon entera de que haga falta el NDK.
$env:CC_aarch64_linux_android = $clang
$env:AR_aarch64_linux_android = $ar
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $clang

$crates = @('multidrive-crypto', 'multidrive-raid', 'multidrive-backend', 'multidrive-core')
# No se llama `$args`: esa es una variable automatica de PowerShell y pisarla trae sorpresas.
$argumentos = @('check', '--target', 'aarch64-linux-android')
foreach ($c in $crates) { $argumentos += @('-p', $c) }

Write-Host "==> cargo $($argumentos -join ' ')"
# `cargo` escribe su progreso por la salida de error, que es lo normal y no es un fallo. Con
# `ErrorActionPreference = Stop` eso bastaria para abortar, asi que se suelta mientras corre y
# se mira el codigo de salida, que es lo unico que de verdad dice si fue bien.
$antes = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
& cargo @argumentos
$codigo = $LASTEXITCODE
$ErrorActionPreference = $antes
if ($codigo -ne 0) { exit $codigo }

Write-Host ''
Write-Host 'El nucleo cruza a Android. Esto no dice que haya version de Android: dice que'
Write-Host 'nadie lo ha atado al sistema operativo por el camino.'
