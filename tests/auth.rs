//! GitHub device login and the local config store.
use carla_pack_registry::Error;
use carla_pack_registry::auth::{DeviceCode, DeviceFlow, Poll};
use carla_pack_registry::store::Store;
use httpmock::prelude::*;
use serde_json::json;

#[test]
fn device_flow_polls_until_token() {
    let github = MockServer::start();
    github.mock(|when, then| {
        when.method(POST).path("/login/device/code").body_includes("client_id=cid").body_includes("scope=read%3Aorg");
        then.status(200).json_body(json!({ "device_code": "dc", "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device", "expires_in": 900, "interval": 5 }));
    });
    let flow = DeviceFlow::new(&github.base_url(), "cid").unwrap();
    let code = flow.start("read:org").unwrap();
    assert_eq!((code.user_code.as_str(), code.interval), ("ABCD-1234", 5));

    let mut pending = github.mock(|when, then| {
        when.method(POST).path("/login/oauth/access_token").body_includes("device_code=dc");
        then.status(200).json_body(json!({ "error": "authorization_pending" }));
    });
    assert_eq!(flow.poll("dc").unwrap(), Poll::Pending);
    pending.delete();

    let mut slow = github.mock(|when, then| {
        when.method(POST).path("/login/oauth/access_token");
        then.status(200).json_body(json!({ "error": "slow_down", "interval": 10 }));
    });
    assert_eq!(flow.poll("dc").unwrap(), Poll::SlowDown(10));
    slow.delete();

    github.mock(|when, then| {
        when.method(POST)
            .path("/login/oauth/access_token")
            .body_includes("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code");
        then.status(200).json_body(json!({ "access_token": "gho_abc", "token_type": "bearer", "scope": "read:org" }));
    });
    assert_eq!(flow.poll("dc").unwrap(), Poll::Token("gho_abc".into()));
}

#[test]
fn device_flow_reports_denied_expired_and_disabled() {
    let github = MockServer::start();
    let flow = DeviceFlow::new(&github.base_url(), "cid").unwrap();
    for (error, needle) in [("access_denied", "cancelled"), ("expired_token", "expired")] {
        let mut m = github.mock(|when, then| {
            when.method(POST).path("/login/oauth/access_token");
            then.status(200).json_body(json!({ "error": error }));
        });
        assert!(flow.poll("dc").unwrap_err().to_string().contains(needle));
        m.delete();
    }
    github.mock(|when, then| {
        when.method(POST).path("/login/device/code");
        then.status(200).json_body(json!({ "error": "device_flow_disabled" }));
    });
    assert!(flow.start("read:org").unwrap_err().to_string().contains("enable Device Flow"));
}

#[test]
fn device_flow_wait_handles_slow_down_and_interrupts() {
    let github = MockServer::start();
    let mut slow = github.mock(|when, then| {
        when.method(POST).path("/login/oauth/access_token");
        then.status(200).json_body(json!({ "error": "slow_down", "interval": 0 }));
    });
    let flow = DeviceFlow::new(&github.base_url(), "cid").unwrap();
    let code = DeviceCode {
        device_code: "dc".into(),
        user_code: "U".into(),
        verification_uri: "v".into(),
        expires_in: 60,
        interval: 0,
    };
    // keep_going returning false aborts before the next poll.
    let mut ticks = 0;
    assert!(matches!(
        flow.wait(&code, || {
            ticks += 1;
            ticks < 3
        }),
        Err(Error::Interrupted)
    ));
    assert_eq!(slow.calls(), 2);
    slow.delete();
    github.mock(|when, then| {
        when.method(POST).path("/login/oauth/access_token");
        then.status(200).json_body(json!({ "access_token": "gho_x" }));
    });
    assert_eq!(flow.wait(&code, || true).unwrap(), "gho_x");
}

#[test]
fn repositories_are_validated_and_deduplicated() {
    let mut s = Store::default();
    assert!(s.add_repo("tier4/carla-packs").unwrap());
    assert!(!s.add_repo("Tier4/Carla-Packs").unwrap());
    assert!(s.add_repo("not-a-repo").is_err());
    assert!(s.add_repo("a/..").is_err());
    assert!(s.remove_repo("TIER4/carla-packs"));
    assert!(s.repos.is_empty());
}
