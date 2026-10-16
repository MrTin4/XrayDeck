param([switch]$Live, [switch]$Reflection, [switch]$Tray, [string]$ServiceSettings)
. "$PSScriptRoot\env.ps1"
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    cargo fmt --check
    if ($LASTEXITCODE) { throw 'rustfmt failed' }
    cargo test --locked --no-default-features
    if ($LASTEXITCODE) { throw 'unit tests failed' }
    cargo test --locked --all-features --bin xray-deck
    if ($LASTEXITCODE) { throw 'desktop unit tests failed' }
    cargo clippy --locked --all-targets --all-features -- -D warnings
    if ($LASTEXITCODE) { throw 'clippy failed' }
    if ($Tray) {
        cargo test --locked --all-features --bin xray-deck tray::tests::native_tray_hides_restores_and_recovers_after_icon_removal -- --ignored
        if ($LASTEXITCODE) { throw 'native tray test failed' }
    }
    if ($Live) {
        cargo test --locked --no-default-features --test api_live running_core_returns_nodes_without_credentials -- --ignored
        if ($LASTEXITCODE) { throw 'live API tests failed' }
    }
    if ($ServiceSettings) {
        $env:XRAY_DECK_TEST_SETTINGS = (Resolve-Path -LiteralPath $ServiceSettings).Path
        cargo test --locked --no-default-features --test api_live local_service_association_matches_listener -- --ignored
        if ($LASTEXITCODE) { throw 'live service association test failed' }
    }
    if ($Reflection) {
        cargo test --locked --no-default-features --test api_live reflection_lists_services_when_enabled -- --ignored
        if ($LASTEXITCODE) { throw 'live Reflection test failed' }
    }
} finally { Pop-Location }
