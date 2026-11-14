param([string]$Output = (Join-Path $PSScriptRoot '../../target/ventoy-runtime'))

$ErrorActionPreference = 'Stop'
$version = '1.1.18'
$hash = '082C478E8C432E1AC653899FCF50E3A4077A2851BC6112B5DDD30545962164A9'
$uri = "https://github.com/ventoy/Ventoy/releases/download/v$version/ventoy-$version-windows.zip"
$Output = [IO.Path]::GetFullPath($Output)
if (Test-Path -LiteralPath $Output) {
    throw "Import destination already exists: $Output"
}
$null = New-Item -ItemType Directory -Path $Output
$archive = Join-Path $Output "ventoy-$version-windows.zip"
Invoke-WebRequest -Uri $uri -OutFile $archive
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash -ne $hash) {
    throw 'Ventoy archive SHA-256 mismatch. The archive was not extracted.'
}
Expand-Archive -LiteralPath $archive -DestinationPath $Output
Join-Path $Output "ventoy-$version"
