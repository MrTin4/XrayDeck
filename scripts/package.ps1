. "$PSScriptRoot\env.ps1"
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    cargo build --locked --release
    if ($LASTEXITCODE) { throw 'release build failed' }
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE) { throw 'cargo metadata failed' }
    $version = $metadata.packages[0].version
    $packageName = "XrayDeck-$version-windows-x64"
    $folder = Join-Path $PWD "dist\$packageName"
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    Copy-Item -LiteralPath 'target\release\xray-deck.exe' -Destination $folder
    Copy-Item -LiteralPath 'README.md', 'LICENSE', 'CHANGELOG.md' -Destination $folder
    if (Test-Path -LiteralPath 'docs') { Copy-Item -LiteralPath 'docs' -Destination $folder -Recurse -Force }
    # App-local redistributable runtime: no administrator installation is required.
    $runtime = Get-ChildItem -Path "$vsPath\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT\vcruntime140.dll" -ErrorAction SilentlyContinue | Sort-Object FullName -Descending | Select-Object -First 1
    if (!$runtime) { throw 'MSVC x64 redistributable runtime not found; cannot produce a portable package' }
    Copy-Item -LiteralPath $runtime.FullName -Destination $folder
    $runtimeExtra = Join-Path $runtime.DirectoryName 'vcruntime140_1.dll'
    if (Test-Path -LiteralPath $runtimeExtra) { Copy-Item -LiteralPath $runtimeExtra -Destination $folder }
    Compress-Archive -Path "$folder\*" -DestinationPath "dist\$packageName.zip" -Force
    $archive = Join-Path $PWD "dist\$packageName.zip"
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $packageName.zip" | Set-Content -LiteralPath "$archive.sha256" -Encoding ascii
} finally { Pop-Location }
