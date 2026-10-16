//! Opt-in, read-only checks against the user's running core. No service mutations.
#[test]
#[ignore = "requires Reflection enabled on the local Xray service"]
fn reflection_lists_services_when_enabled() {
    let address = std::env::var("XRAY_DECK_TEST_API").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let snapshot =
        xray_deck::api::inspect_blocking(&address).expect("local gRPC API should be reachable");
    assert!(
        snapshot.discovery_note.is_none(),
        "{:?}",
        snapshot.discovery_note
    );
    assert!(
        snapshot
            .services
            .iter()
            .any(|name| name == "grpc.reflection.v1.ServerReflection"
                || name == "grpc.reflection.v1alpha.ServerReflection")
    );
    assert!(
        snapshot
            .services
            .iter()
            .any(|name| name == "xray.app.proxyman.command.HandlerService")
    );
}

#[test]
#[ignore = "requires the local Xray service; run with --ignored"]
fn running_core_returns_nodes_without_credentials() {
    let address = std::env::var("XRAY_DECK_TEST_API").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let snapshot =
        xray_deck::api::inspect_blocking(&address).expect("local gRPC API should be reachable");
    assert!(
        snapshot.nodes_note.is_none(),
        "ListOutbounds: {:?}",
        snapshot.nodes_note
    );
    assert!(
        !snapshot.nodes.is_empty(),
        "expected at least one configured outbound"
    );
    assert!(snapshot.nodes.iter().all(|node| !node.tag.is_empty()));
    let json = serde_json::to_value(&snapshot).unwrap();
    for node in json["nodes"].as_array().unwrap() {
        for forbidden in ["password", "uuid", "user", "proxy_settings", "value"] {
            assert!(
                node.get(forbidden).is_none(),
                "credentials must not enter the display model"
            );
        }
    }
}

#[test]
#[ignore = "requires explicit service settings path; read-only"]
fn local_service_association_matches_listener() {
    let path = std::env::var("XRAY_DECK_TEST_SETTINGS")
        .expect("set XRAY_DECK_TEST_SETTINGS to a local settings file");
    let settings = xray_deck::settings::Settings::load(std::path::Path::new(&path)).unwrap();
    let info = xray_deck::service::validate(&settings).expect("service/config/API association");
    assert!(info.process_id > 0);
    assert_eq!(info.state, "Running");
    assert!(!info.file_version.is_empty());
    if let Some(version) = &info.runtime_version {
        assert!(xray_deck::version::parse_version(version).is_ok());
        assert!(info.runtime_version_source.is_some());
        assert!(
            info.runtime_version_source
                .as_ref()
                .unwrap()
                .contains("--version")
        );
        if !info.image_verified {
            assert!(info.runtime_version_note.is_some());
        }
    } else {
        assert!(info.runtime_version_note.is_some());
    }
}
