use super::fence::{refused_host, settings};
use super::*;
use serde_json::json;

#[test]
fn containment_becomes_fence_settings() {
    let containment = Containment {
        writable: vec!["/work/tree".into(), "/home/u/.cargo".into()],
        read_only: vec!["/work/tree/.git".into()],
        domains: vec!["crates.io".into(), "*.crates.io".into()],
        loopback_ports: vec![4226],
        mach_services: vec!["com.apple.SecurityServer".into()],
        temp_dir: Some("/run/tmp/session".into()),
    };

    assert_eq!(
        settings(&containment),
        json!({
            "network": {
                "allowedDomains": ["crates.io", "*.crates.io"],
                "deniedDomains": [],
                "allowLocalOutbound": true,
                "allowLocalOutboundPorts": [4226],
            },
            "filesystem": {
                "allowWrite": ["/work/tree", "/home/u/.cargo", "/run/tmp/session"],
                "denyWrite": ["/work/tree/.git"],
            },
            "command": { "useDefaults": false },
            "macos": { "mach": { "lookup": ["com.apple.SecurityServer"] } },
        })
    );
}

#[test]
fn an_empty_containment_writes_nothing_and_reaches_nothing() {
    assert_eq!(
        settings(&Containment::default()),
        json!({
            "network": { "allowedDomains": [], "deniedDomains": [] },
            "filesystem": { "allowWrite": [], "denyWrite": [] },
            "command": { "useDefaults": false },
        })
    );
}

#[test]
fn refused_hosts_are_read_from_both_proxies_lines() {
    assert_eq!(
        refused_host(
            "[fence:http] 11:08:14 ✗ CONNECT 403 example.com https://example.com:443 (0s)"
        )
        .as_deref(),
        Some("example.com")
    );
    assert_eq!(
        refused_host("[fence:socks] 11:08:14 ✗ CONNECT example.org:443 BLOCKED").as_deref(),
        Some("example.org")
    );
    assert_eq!(
        refused_host("[fence:socks] 11:08:14 ✗ CONNECT [::1]:22 BLOCKED").as_deref(),
        Some("::1")
    );
    assert_eq!(refused_host("curl: (7) CONNECT tunnel failed"), None);
    assert_eq!(
        refused_host("[fence:http] 11:08:14 ✓ CONNECT 200 crates.io"),
        None
    );
}

#[test]
fn each_refused_host_is_listed_once() {
    let fence = FenceRuntime::new("fence");
    let stderr = "[fence:http] 1 ✗ CONNECT 403 a.example https://a.example:443 (0s)\n\
                  curl: (7) CONNECT tunnel failed\n\
                  [fence:socks] 1 ✗ CONNECT a.example:443 BLOCKED\n\
                  [fence:socks] 1 ✗ CONNECT b.example:22 BLOCKED\n";
    assert_eq!(fence.refused_hosts(stderr), ["a.example", "b.example"]);
}

#[test]
fn a_missing_binary_is_unavailable() {
    let fence = FenceRuntime::new("/nonexistent/fence");
    let error = fence.probe().unwrap_err();
    assert!(error.0.contains("/nonexistent/fence"), "{error}");
}
