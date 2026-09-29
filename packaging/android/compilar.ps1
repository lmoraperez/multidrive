<#
.SYNOPSIS
    Construye el `.so` del nucleo y lo deja donde Gradle lo espera.

.DESCRIPTION
    Es el puente entre los dos sistemas de construccion. El nucleo se compila con cargo, igual
    que para el escritorio, y lo unico que se hace aqui de mas es copiarlo a `jniLibs`.

    Se hace asi y no con `cargo-ndk` ni con `externalNativeBuild` porque no hay nada que
    ganar: `cargo-ndk` haria por debajo lo mismo que estas tres variables de entorno, y a
    cambio seria una dependencia mas que instalar y una capa mas donde mirar cuando algo
    falle. El dia que haya que compilar cuatro arquitecturas y esto se vuelva incomodo, se
    cambia entonces.

.PARAMETER Ndk
    Carpeta del NDK. Por defecto, `$env:ANDROID_NDK_HOME`.

.PARAMETER Api
    Nivel de API contra el que compilar. 26, que es el `minSdk` de la aplicacion.

.PARAMETER Depuracion
    Compilar sin optimizar. Va mucho mas rapido y sirve para probar el cableado, pero **no
    para medir nada**: en el escritorio, Argon2id pasa de 22 ms a 424 entre release y debug.
#>
[CmdletBinding()]
param(
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [int]$Api = 26,
    [switch]$Depuracion
)

$ErrorActionPreference = 'Stop'

$raiz = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$abi = 'arm64-v8a'
$objetivo = 'aarch64-linux-android'

if (-not $Ndk) {
    Write-Error @'
No se sabe donde esta el NDK.

  $env:ANDROID_NDK_HOME = "C:\ruta\al\android-ndk-r30"
'@
}

$bin = Join-Path $Ndk 'toolchains/llvm/prebuilt/windows-x86_64/bin'
if (-not (Test-Path $bin)) { $bin = Join-Path $Ndk 'toolchains/llvm/prebuilt/linux-x86_64/bin' }
if (-not (Test-Path $bin)) { Write-Error "No encuentro las herramientas dentro de '$Ndk'." }

$clang = Join-Path $bin "aarch64-linux-android$Api-clang.cmd"
if (-not (Test-Path $clang)) { $clang = Join-Path $bin "aarch64-linux-android$Api-clang" }
if (-not (Test-Path $clang)) { Write-Error "Ese NDK no trae compilador para la API $Api." }

$ar = Join-Path $bin 'llvm-ar.exe'
if (-not (Test-Path $ar)) { $ar = Join-Path $bin 'llvm-ar' }

# `aws-lc-sys`, debajo de rustls, es lo unico que pide un compilador de C. Es la razon entera
# de que haga falta el NDK.
$env:CC_aarch64_linux_android = $clang
$env:AR_aarch64_linux_android = $ar
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $clang

$perfil = if ($Depuracion) { 'debug' } else { 'release' }
$argumentos = @('build', '--target', $objetivo, '-p', 'multidrive-android')
if (-not $Depuracion) { $argumentos += '--release' }

Write-Host "==> NDK     $Ndk"
Write-Host "==> perfil  $perfil"
Write-Host "==> cargo $($argumentos -join ' ')"

Push-Location $raiz
try {
    # cargo escribe su progreso por la salida de error, que no es un fallo. Con
    # `ErrorActionPreference = Stop` eso bastaria para abortar.
    $antes = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & cargo @argumentos
    $codigo = $LASTEXITCODE
    $ErrorActionPreference = $antes
    if ($codigo -ne 0) { exit $codigo }
} finally {
    Pop-Location
}

$origen = Join-Path $raiz "target/$objetivo/$perfil/libmultidrive.so"
if (-not (Test-Path $origen)) { Write-Error "cargo dijo que si, pero no esta '$origen'." }

$destino = Join-Path $raiz "android/app/src/main/jniLibs/$abi"
New-Item -ItemType Directory -Force $destino | Out-Null
Copy-Item $origen (Join-Path $destino 'libmultidrive.so') -Force

$mib = [math]::Round((Get-Item $origen).Length / 1MB, 2)
Write-Host ''
Write-Host "libmultidrive.so ($mib MiB) -> android/app/src/main/jniLibs/$abi/"

# El componente Kotlin del verificador de certificados, que viene dentro del propio crate.
#
# Se busca aqui y no desde Gradle a proposito. La receta oficial hace que Gradle ejecute
# `cargo metadata` para localizarlo, lo cual ata la compilacion de Android a tener cargo
# delante. Prefiero que el lado de cargo lo resuelva el guion de cargo, y que Gradle solo
# empaquete lo que encuentre, igual que con el `.so`.
$antes = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
$meta = & cargo metadata --format-version 1 --filter-platform aarch64-linux-android --manifest-path (Join-Path $raiz 'Cargo.toml') 2>$null | ConvertFrom-Json
$ErrorActionPreference = $antes

$paquete = $meta.packages | Where-Object { $_.name -eq 'rustls-platform-verifier-android' } | Select-Object -First 1
if (-not $paquete) {
    Write-Error @'
No encuentro `rustls-platform-verifier-android` entre las dependencias de Android.

Sin su componente, la primera peticion HTTPS **entra en panico** en vez de fallar. Llega por
debajo de reqwest, asi que si no esta, es que algo cambio en el arbol de dependencias.
'@
}

$aar = Get-ChildItem (Split-Path -Parent $paquete.manifest_path) -Recurse -Filter '*.aar' |
    Select-Object -First 1
if (-not $aar) { Write-Error "El crate esta, pero no trae ningun .aar dentro." }

$libs = Join-Path $raiz 'android/app/libs'
New-Item -ItemType Directory -Force $libs | Out-Null
Copy-Item $aar.FullName (Join-Path $libs 'rustls-platform-verifier.aar') -Force
Write-Host "$($aar.Name) -> android/app/libs/rustls-platform-verifier.aar"

# Que los simbolos esten es lo unico que de verdad importa de este guion: sin ellos la
# aplicacion compila, instala, arranca y **falla al llamar**, que es el peor momento para
# enterarse.
$nm = Join-Path $bin 'llvm-nm.exe'
if (-not (Test-Path $nm)) { $nm = Join-Path $bin 'llvm-nm' }
if (Test-Path $nm) {
    $antes = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $simbolos = & $nm --defined-only --extern-only $origen 2>$null | Select-String 'Java_'
    $ErrorActionPreference = $antes
    if ($simbolos) {
        Write-Host 'simbolos JNI:'
        foreach ($s in $simbolos) { Write-Host ("  " + ($s.ToString() -replace '^\S+\s+\S+\s+', '')) }
    } else {
        Write-Warning 'el .so no exporta ningun simbolo Java_: la aplicacion fallara al llamar'
    }
}
