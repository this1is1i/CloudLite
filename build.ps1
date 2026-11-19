param([switch]$Offline)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path -LiteralPath $PSScriptRoot).Path
$manifestPath = Join-Path $projectRoot 'Cargo.toml'
$cacheRoot = Join-Path $projectRoot '.build-cache'
$savedEnvironment = @{}
foreach ($name in @('CARGO_HOME', 'CARGO_TARGET_DIR', 'CARGO_INCREMENTAL', 'CARGO_TERM_COLOR')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
try {
    Get-Command cargo.exe -ErrorAction Stop | Out-Null
    $env:CARGO_HOME = Join-Path $cacheRoot 'cargo-home'
    $env:CARGO_TARGET_DIR = Join-Path $cacheRoot 'target'
    $env:CARGO_INCREMENTAL = '0'
    $env:CARGO_TERM_COLOR = 'never'
    $dependencyOptions = @('--locked')
    if ($Offline) { $dependencyOptions += '--offline' }
    & cargo.exe fmt --check --manifest-path $manifestPath --package cloudlite
    if ($LASTEXITCODE -ne 0) { throw 'Formatting check failed.' }
    & cargo.exe test @dependencyOptions --manifest-path $manifestPath --tests
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
    & cargo.exe build --release @dependencyOptions --manifest-path $manifestPath
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
    Write-Output (Join-Path $env:CARGO_TARGET_DIR 'release\cloudlite.exe')
}
finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
    }
}
