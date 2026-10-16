# Version builds always produce the matching portable release in dist/.
$ErrorActionPreference = 'Stop'
& "$PSScriptRoot\package.ps1"
