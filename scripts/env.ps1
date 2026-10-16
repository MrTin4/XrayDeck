$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$localCargo = Join-Path $projectRoot '.tools\cargo'
if (Test-Path (Join-Path $localCargo 'bin\cargo.exe')) {
    $env:CARGO_HOME = $localCargo
    $env:RUSTUP_HOME = Join-Path $projectRoot '.tools\rustup'
    $env:Path = "$localCargo\bin;$env:Path"
}
$vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
if (Test-Path $vswhere) {
    $vsPath = & $vswhere -all -products '*' -property installationPath | Select-Object -First 1
    $devCmd = Join-Path $vsPath 'Common7\Tools\VsDevCmd.bat'
    if (Test-Path $devCmd) {
        # Rust discovers MSVC itself, including instances pending a VS restart.
        # Put SDK executables on PATH for rc.exe and shader compilation tools.
        $sdkRoot = 'C:\Program Files (x86)\Windows Kits\10'
        $sdkVersion = Get-ChildItem (Join-Path $sdkRoot 'bin') -Directory | Sort-Object Name -Descending | Select-Object -First 1
        if ($sdkVersion) { $env:Path = "$($sdkVersion.FullName)\x64;$env:Path" }
    }
}
