. "$PSScriptRoot\env.ps1"
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    cargo build --locked --release --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE) { throw 'release build failed' }
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE) { throw 'cargo metadata failed' }
    $version = ($metadata.packages | Where-Object name -EQ 'xray-deck').version
    $packageName = "XrayDeck-$version-windows-x64"
    $destination = Join-Path $PWD 'dist'
    $folder = Join-Path $destination $packageName
    # Clear only this version's package directory to prevent stale files in rebuilds.
    if (Test-Path -LiteralPath $folder) {
        $resolvedPackage = (Resolve-Path -LiteralPath $folder).Path
        if ((Split-Path $resolvedPackage -Parent) -ne $destination) { throw 'Invalid package destination' }
        Remove-Item -LiteralPath $resolvedPackage -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    $executable = Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc\release\xray-deck.exe'
    Copy-Item -LiteralPath $executable -Destination $folder
    Copy-Item -LiteralPath 'README.md', 'LICENSE', 'CHANGELOG.md' -Destination $folder
    if (Test-Path -LiteralPath 'docs') { Copy-Item -LiteralPath 'docs' -Destination $folder -Recurse -Force }
    # App-local redistributable runtime: no administrator installation is required.
    $runtime = Get-ChildItem -Path "$vsPath\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT\vcruntime140.dll" -ErrorAction SilentlyContinue | Sort-Object FullName -Descending | Select-Object -First 1
    if (!$runtime) { throw 'MSVC x64 redistributable runtime not found; cannot produce a portable package' }
    Copy-Item -LiteralPath $runtime.FullName -Destination $folder
    $runtimeExtra = Join-Path $runtime.DirectoryName 'vcruntime140_1.dll'
    if (Test-Path -LiteralPath $runtimeExtra) { Copy-Item -LiteralPath $runtimeExtra -Destination $folder }
    $archive = Join-Path $destination "$packageName.zip"
    Compress-Archive -Path "$folder\*" -DestinationPath $archive -Force
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $packageName.zip" | Set-Content -LiteralPath "$archive.sha256" -Encoding ascii
    Write-Host "Release package: $archive"
} finally { Pop-Location }
