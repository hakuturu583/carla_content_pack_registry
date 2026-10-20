//! Core behaviour against mock HTTP servers (registry, storage and GitHub).
use carla_pack::auth::{DeviceFlow, Poll};
use carla_pack::store::{Login, Store};
use carla_pack::{Client, Error};
use httpmock::prelude::*;
use serde_json::json;

const BUCKET_ID: &str = "5d93316f-ced0-4419-836f-813b9d450715";

fn bucket(id: &str, owner: &str, name: &str) -> serde_json::Value {
    json!({ "id": id, "name": name, "owner": { "id": 1, "login": owner }, "visibility": "private",
            "provider": "r2", "verifyStatus": "verified" })
}

#[test]
fn sends_bearer_token_and_maps_api_errors() {
    let server = MockServer::start();
    let me = server.mock(|when, then| {
        when.method(GET).path("/v1/me").header("authorization", "Bearer tok");
        then.status(200).json_body(json!({ "user": { "id": 1, "login": "alice" } }));
    });
    let denied = server.mock(|when, then| {
        when.method(DELETE).path(format!("/v1/buckets/{BUCKET_ID}"));
        then.status(403)
            .json_body(json!({ "error": { "code": "forbidden", "message": "Only the bucket owner can do this" } }));
    });

    let c = Client::new(&server.base_url(), Some("tok".into())).unwrap();
    assert_eq!(c.me().unwrap()["user"]["login"], "alice");
    match c.delete_bucket(BUCKET_ID).unwrap_err() {
        Error::Api { status, code, message } => {
            assert_eq!((status, code.as_str()), (403, "forbidden"));
            assert!(message.contains("owner"));
        }
        e => panic!("unexpected {e:?}"),
    }
    me.assert();
    denied.assert();
}

#[test]
fn resolves_owner_name_references() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v1/buckets");
        then.status(200).json_body(json!({ "buckets": [
            bucket(BUCKET_ID, "alice", "maps"),
            bucket("6d93316f-ced0-4419-836f-813b9d450715", "bob", "maps"),
            bucket("7d93316f-ced0-4419-836f-813b9d450715", "bob", "props"),
        ]}));
    });
    let c = Client::new(&server.base_url(), Some("t".into())).unwrap();
    assert_eq!(c.resolve_bucket("alice/maps").unwrap().id, BUCKET_ID);
    assert_eq!(c.resolve_bucket("ALICE/maps").unwrap().id, BUCKET_ID);
    assert_eq!(c.resolve_bucket("props").unwrap().name, "props");
    let ambiguous = c.resolve_bucket("maps").unwrap_err().to_string();
    assert!(ambiguous.contains("alice/maps") && ambiguous.contains("bob/maps"), "{ambiguous}");
    assert!(matches!(c.resolve_bucket("carol/maps"), Err(Error::Usage(_))));
}

#[test]
fn follows_object_pagination() {
    let server = MockServer::start();
    let path = format!("/v1/buckets/{BUCKET_ID}/objects");
    server.mock(|when, then| {
        when.method(GET).path(&path).query_param("prefix", "maps/").query_param_missing("cursor");
        then.status(200)
            .json_body(json!({ "objects": [{ "key": "maps/a", "size": 1, "etag": "", "lastModified": "" }],
                                            "prefixes": [], "nextCursor": "c2" }));
    });
    server.mock(|when, then| {
        when.method(GET).path(&path).query_param("cursor", "c2");
        then.status(200)
            .json_body(json!({ "objects": [{ "key": "maps/b", "size": 2, "etag": "", "lastModified": "" }],
                                            "prefixes": [], "nextCursor": null }));
    });
    let c = Client::new(&server.base_url(), Some("t".into())).unwrap();
    let keys: Vec<_> = c.list_all_objects(BUCKET_ID, Some("maps/")).unwrap().into_iter().map(|o| o.key).collect();
    assert_eq!(keys, ["maps/a", "maps/b"]);
}

#[test]
fn downloads_from_presigned_url_without_registry_token() {
    let registry = MockServer::start();
    let storage = MockServer::start();
    let presign = registry.mock(|when, then| {
        // Keys are encoded per segment, keeping "/".
        when.method(GET).path(format!("/v1/buckets/{BUCKET_ID}/objects/maps/Town%2010.tar/download"));
        then.status(200).json_body(json!({ "url": storage.url("/packs/maps/Town%2010.tar?X-Amz-Signature=abc"),
                                            "method": "GET", "expiresAt": "2026-01-01T00:05:00Z" }));
    });
    let object = storage.mock(|when, then| {
        when.method(GET)
            .path("/packs/maps/Town%2010.tar")
            .query_param("X-Amz-Signature", "abc")
            .header_missing("authorization");
        then.status(200).body("pack-bytes");
    });

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out/maps/Town 10.tar");
    let c = Client::new(&registry.base_url(), Some("secret-token".into())).unwrap();
    assert_eq!(c.download(BUCKET_ID, "maps/Town 10.tar", &dest).unwrap(), 10);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "pack-bytes");
    assert!(!dir.path().join("out/maps/Town 10.tar.part").exists());
    presign.assert();
    object.assert();
}

#[test]
fn failed_download_leaves_no_partial_file() {
    let registry = MockServer::start();
    let storage = MockServer::start();
    registry.mock(|when, then| {
        when.method(GET).path(format!("/v1/buckets/{BUCKET_ID}/objects/x.bin/download"));
        then.status(200).json_body(json!({ "url": storage.url("/x.bin"), "method": "GET", "expiresAt": "" }));
    });
    storage.mock(|when, then| {
        when.method(GET).path("/x.bin");
        then.status(403).body("<Error><Code>AccessDenied</Code></Error>");
    });
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("x.bin");
    let c = Client::new(&registry.base_url(), Some("t".into())).unwrap();
    assert!(matches!(c.download(BUCKET_ID, "x.bin", &dest), Err(Error::Api { status: 403, .. })));
    assert!(!dest.exists());
}

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
fn store_round_trip_and_permissions() {
    let dir = tempfile::tempdir().unwrap();
    // The only test that touches the process environment.
    unsafe { std::env::set_var("CARLA_PACK_CONFIG_DIR", dir.path()) };

    let mut s = Store::load().unwrap();
    assert_eq!(s.registry(None), "http://localhost:8787");
    s.set_login("https://registry.example.org/", Login { token: "tok".into(), login: "alice".into() });
    s.save().unwrap();

    let s = Store::load().unwrap();
    assert_eq!(s.registry(None), "https://registry.example.org");
    assert_eq!(s.registry(Some("https://other.example/")), "https://other.example");
    assert_eq!(s.token("https://registry.example.org").as_deref(), Some("tok"));
    assert_eq!(s.token("https://other.example"), None);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("credentials.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
