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

.PARAMETER Credenciales
    Fichero CLAVE=VALOR con las credenciales de la aplicacion. Por defecto,
    `packaging\windows\credenciales.conf`, que es el mismo que usa el instalador: dice
    «windows» por como nacio, pero lo que lleva dentro no es de ningun sistema.

    Sin esto, el `.so` sale sin credenciales compiladas y la aplicacion se niega a usar una
    cuenta ya enlazada diciendo que no trae aplicacion registrada. Pasa de verdad: costo una
    prueba en el telefono.

.PARAMETER Depuracion
    Compilar sin optimizar. Va mucho mas rapido y sirve para probar el cableado, pero **no
    para medir nada**: en el escritorio, Argon2id pasa de 22 ms a 424 entre release y debug.
#>
[CmdletBinding()]
param(
    [string]$Ndk = $env:ANDROID_NDK_HOME,
    [int]$Api = 26,
    [string]$Credenciales,
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

# Las credenciales de la aplicacion, que van **dentro** del binario por `option_env!`. Es la
# misma lista y el mismo fichero que usa el instalador de Windows: tener dos seria tener uno
# que se queda viejo.
if (-not $Credenciales) {
    $Credenciales = Join-Path $raiz 'packaging/windows/credenciales.conf'
}
$claves = @(
    'MULTIDRIVE_APP_GDRIVE_CLIENT_ID',
    'MULTIDRIVE_APP_GDRIVE_CLIENT_SECRET',
    'MULTIDRIVE_APP_ONEDRIVE_CLIENT_ID',
    'MULTIDRIVE_APP_ONEDRIVE_CLIENT_SECRET'
)
if (Test-Path $Credenciales) {
    foreach ($linea in Get-Content $Credenciales) {
        $l = $linea.Trim()
        if (-not $l -or $l.StartsWith('#')) { continue }
        $i = $l.IndexOf('=')
        if ($i -lt 1) { continue }
        $clave = $l.Substring(0, $i).Trim() -replace '^export\s+', ''
        $valor = $l.Substring($i + 1).Trim().Trim('"').Trim("'")
        if ($claves -contains $clave -and $valor) { Set-Item -Path "Env:$clave" -Value $valor }
    }
}
# Solo los nombres, nunca los valores: esta salida acaba pegada en informes.
$puestas = @($claves | Where-Object { (Get-Item "Env:$_" -ErrorAction SilentlyContinue).Value })
if ($puestas.Count -eq 0) {
    Write-Warning 'sin credenciales de aplicacion: la aplicacion se negara a usar una cuenta'
    Write-Warning 'ya enlazada diciendo que no trae aplicacion registrada. Ver docs/APLICACION-OAUTH.md'
} else {
    Write-Host "==> credenciales de aplicacion: $($puestas -join ', ')"
}

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
$enviado = Join-Path $destino 'libmultidrive.so'
Copy-Item $origen $enviado -Force

$antesMib = [math]::Round((Get-Item $origen).Length / 1MB, 2)

# Los simbolos salen de la biblioteca que se reparte y se guardan aparte.
#
# # Por que se estripa
#
# Un `.so` de release de Rust llega con toda su tabla de simbolos: son seis megas de los
# veinte, y van en la descarga de cada persona que instale la aplicacion sin servirle de
# nada. Lo que el sistema necesita para encontrar las funciones JNI es la tabla **dinamica**,
# y esa se queda: por eso se comprueba abajo, sobre el fichero ya estripado, que siguen ahi.
#
# # Y por que se guardan en vez de tirarlos
#
# Porque son lo que convierte un fallo nativo en algo que se puede leer. Sin ellos, la caida
# que reporte alguien llega como una lista de direcciones en memoria, y averiguar a que
# funcion corresponde cada una despues de haber publicado es trabajo perdido. El zip se sube
# a Play Console junto al bundle --Play lo pide, y tiene razon-- y sirve para **esa** version
# y no para otra: un `.so` recompilado tiene otras direcciones.
if (-not $Depuracion) {
    $simbolosDir = Join-Path $raiz "target/simbolos-nativos/$abi"
    New-Item -ItemType Directory -Force $simbolosDir | Out-Null
    Copy-Item $origen (Join-Path $simbolosDir 'libmultidrive.so') -Force

    $strip = Join-Path $bin 'llvm-strip.exe'
    if (-not (Test-Path $strip)) { $strip = Join-Path $bin 'llvm-strip' }
    if (Test-Path $strip) {
        & $strip --strip-all $enviado
        if ($LASTEXITCODE -ne 0) { Write-Error "llvm-strip fallo con codigo $LASTEXITCODE" }

        $zip = Join-Path $PSScriptRoot 'simbolos-nativos.zip'
        if (Test-Path $zip) { Remove-Item $zip -Force }

        # La estructura que espera Play es `<abi>/<biblioteca>.so` en la raiz del zip, con
        # **barras normales**. Se escribe entrada por entrada y no con `Compress-Archive`
        # porque el de Windows PowerShell pone `\` en los nombres, que la especificacion ZIP
        # no admite: el fichero se abre igual en el Explorador y luego no lo reconoce quien
        # espera la ruta exacta. Un zip que parece bueno y no sirve es peor que uno que falla.
        Add-Type -AssemblyName System.IO.Compression
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archivo = [System.IO.Compression.ZipFile]::Open($zip, 'Create')
        try {
            [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                $archivo, (Join-Path $simbolosDir 'libmultidrive.so'), "$abi/libmultidrive.so") | Out-Null
        } finally {
            $archivo.Dispose()
        }
        Write-Host "simbolos nativos -> packaging/android/simbolos-nativos.zip"
    } else {
        Write-Warning "no hay llvm-strip en '$bin': el .so va entero y sin simbolos aparte."
    }
}

$mib = [math]::Round((Get-Item $enviado).Length / 1MB, 2)
Write-Host ''
if ($mib -lt $antesMib) {
    Write-Host "libmultidrive.so ($antesMib MiB -> $mib MiB) -> android/app/src/main/jniLibs/$abi/"
} else {
    Write-Host "libmultidrive.so ($mib MiB) -> android/app/src/main/jniLibs/$abi/"
}

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
#
# Se mira la biblioteca **que se reparte**, no la que saco cargo, y por la tabla dinamica
# (`--dynamic`): despues de estripar es la unica que queda, y es la que usa el sistema para
# resolver las llamadas. Comprobar la otra seria comprobar un fichero que no viaja.
$nm = Join-Path $bin 'llvm-nm.exe'
if (-not (Test-Path $nm)) { $nm = Join-Path $bin 'llvm-nm' }
if (Test-Path $nm) {
    $antes = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $simbolos = & $nm --dynamic --defined-only --extern-only $enviado 2>$null | Select-String 'Java_'
    $ErrorActionPreference = $antes
    if ($simbolos) {
        Write-Host 'simbolos JNI:'
        foreach ($s in $simbolos) { Write-Host ("  " + ($s.ToString() -replace '^\S+\s+\S+\s+', '')) }
    } else {
        Write-Warning 'el .so no exporta ningun simbolo Java_: la aplicacion fallara al llamar'
    }
}
