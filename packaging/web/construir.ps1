# Construye la version web y la deja en site/app/, lista para servir o para subir.
#
#   .\packaging\web\construir.ps1
#
# El codigo de la pagina vive en el repositorio (crates/multidrive-web/pagina/), porque es
# software. site/ no se versiona, asi que esto copia ahi la pagina y le pone al lado el
# WebAssembly recien compilado.
#
# Hace falta wasm-bindgen en **la misma version** que la biblioteca del Cargo.lock: si no
# coinciden, el JavaScript generado no casa con el binario y falla al cargar, no al compilar.
#
#   cargo install wasm-bindgen-cli --version 0.2.128 --locked

$ErrorActionPreference = "Stop"
$raiz = Resolve-Path "$PSScriptRoot\..\.."

# Lo mismo que en packaging/windows/construir-msi.ps1, y por lo mismo: con 'Stop', PowerShell
# 5.1 toma cada linea que cargo escribe en stderr --su progreso-- por un error y aborta. El
# codigo de salida es lo unico que dice si fue bien.
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
$destino = Join-Path $raiz "site\app"
$pagina = Join-Path $raiz "crates\multidrive-web\pagina"

$esperada = (Select-String -Path (Join-Path $raiz "Cargo.lock") -Pattern '^name = "wasm-bindgen"$' -Context 0,1).Context.PostContext[0] -replace '^version = "(.*)"$', '$1'
$tenemos = (wasm-bindgen --version) -replace '^wasm-bindgen ', ''
if ($tenemos -ne $esperada) {
    throw "wasm-bindgen $tenemos no casa con la biblioteca $esperada. cargo install wasm-bindgen-cli --version $esperada --locked"
}

# En release: Argon2 sin optimizar tarda minutos en derivar la clave en un navegador.
Ejecutar cargo @("build", "--release", "--target", "wasm32-unknown-unknown", "-p", "multidrive-web")

if (Test-Path $destino) { Remove-Item -Recurse -Force $destino }
New-Item -ItemType Directory -Force (Join-Path $destino "pkg") | Out-Null
Ejecutar wasm-bindgen @("--target", "web", "--no-typescript", "--out-dir", (Join-Path $destino "pkg"),
    (Join-Path $raiz "target\wasm32-unknown-unknown\release\multidrive_web.wasm"))
Copy-Item (Join-Path $pagina "*") $destino

$wasm = Get-Item (Join-Path $destino "pkg\multidrive_web_bg.wasm")
"Lista en $destino ($([math]::Round($wasm.Length / 1KB)) KB de WebAssembly)"
"SHA-256 del WebAssembly: $((Get-FileHash $wasm -Algorithm SHA256).Hash.ToLower())"
