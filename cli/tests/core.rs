//! Core behaviour against mock HTTP servers (registry, storage and GitHub).
use carla_pack_registry::auth::{DeviceCode, DeviceFlow, Poll};
use carla_pack_registry::client::{AclChange, AclRef, PullEvent};
use carla_pack_registry::store::{Login, Store};
use carla_pack_registry::{Client, Error};
use httpmock::prelude::*;
use serde_json::json;
use std::sync::Mutex;

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
    assert_eq!(c.resolve_bucket("alice/maps").unwrap()["id"], BUCKET_ID);
    assert_eq!(c.resolve_bucket("ALICE/maps").unwrap()["id"], BUCKET_ID);
    assert_eq!(c.resolve_bucket("props").unwrap()["name"], "props");
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
    let keys: Vec<_> = c.list_objects(BUCKET_ID, Some("maps/")).unwrap().into_iter().map(|o| o.key).collect();
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
fn acl_add_and_remove_merge_with_the_stored_entries() {
    let server = MockServer::start();
    let acl = format!("/v1/buckets/{BUCKET_ID}/acl");
    server.mock(|when, then| {
        when.method(GET).path(&acl);
        then.status(200).json_body(json!({ "entries": [
            { "type": "org", "id": 1, "name": "carla-sim" },
            { "type": "team", "id": 2, "name": "carla-sim/maps" },
        ]}));
    });
    let added = server.mock(|when, then| {
        when.method(PUT).path(&acl).json_body(json!({ "entries": [
            { "type": "org", "login": "carla-sim" },
            { "type": "team", "org": "carla-sim", "slug": "maps" },
            { "type": "user", "login": "bob" },
        ]}));
        then.status(200).json_body(json!({ "entries": [] }));
    });
    let removed = server.mock(|when, then| {
        when.method(PUT).path(&acl).json_body(json!({ "entries": [{ "type": "org", "login": "carla-sim" }] }));
        then.status(200).json_body(json!({ "entries": [] }));
    });
    let c = Client::new(&server.base_url(), Some("t".into())).unwrap();
    c.update_acl(BUCKET_ID, AclChange::Add, vec![AclRef::parse("user", "bob").unwrap()]).unwrap();
    c.update_acl(BUCKET_ID, AclChange::Remove, vec![AclRef::parse("team", "Carla-Sim/MAPS").unwrap()]).unwrap();
    assert!(c.update_acl(BUCKET_ID, AclChange::Add, vec![]).is_err());
    added.assert();
    removed.assert();
}

#[test]
fn pull_expands_prefixes_skips_existing_and_refuses_escaping_keys() {
    let registry = MockServer::start();
    let storage = MockServer::start();
    registry.mock(|when, then| {
        when.method(GET).path(format!("/v1/buckets/{BUCKET_ID}/objects")).query_param("prefix", "maps/");
        then.status(200).json_body(json!({ "objects": [
            { "key": "maps/a.tar", "size": 1 }, { "key": "maps/b.tar", "size": 1 }], "nextCursor": null }));
    });
    for key in ["maps/a.tar", "maps/b.tar", "props/c.tar"] {
        registry.mock(|when, then| {
            when.method(GET).path(format!("/v1/buckets/{BUCKET_ID}/objects/{key}/download"));
            then.status(200).json_body(json!({ "url": storage.url(format!("/{key}")) }));
        });
        storage.mock(|when, then| {
            when.method(GET).path(format!("/{key}"));
            then.status(200).body(key);
        });
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("maps")).unwrap();
    std::fs::write(dir.path().join("maps/b.tar"), "old").unwrap();

    let c = Client::new(&registry.base_url(), Some("t".into())).unwrap();
    let events = Mutex::new(Vec::new());
    let targets = vec!["maps/".to_string(), "props/c.tar".to_string()];
    let total = c.pull(BUCKET_ID, &targets, dir.path(), false, 3, |e| events.lock().unwrap().push(e)).unwrap();
    assert_eq!(total, ("maps/a.tar".len() + "props/c.tar".len()) as u64);
    assert_eq!(std::fs::read_to_string(dir.path().join("props/c.tar")).unwrap(), "props/c.tar");
    assert_eq!(std::fs::read_to_string(dir.path().join("maps/b.tar")).unwrap(), "old");
    let events = events.into_inner().unwrap();
    assert_eq!(events.iter().filter(|e| matches!(e, PullEvent::Skipped { .. })).count(), 1);
    assert_eq!(events.iter().filter(|e| matches!(e, PullEvent::Downloaded { .. })).count(), 2);

    let escape = c.pull(BUCKET_ID, &["../etc/passwd".to_string()], dir.path(), true, 1, |_| {});
    assert!(matches!(escape, Err(Error::Usage(m)) if m.contains("outside")));

    // Overlapping targets download each object once.
    let events = Mutex::new(Vec::new());
    let overlap = vec!["maps/".to_string(), "maps/a.tar".to_string(), "/maps/a.tar".to_string()];
    c.pull(BUCKET_ID, &overlap, dir.path(), true, 4, |e| events.lock().unwrap().push(e)).unwrap();
    assert_eq!(events.into_inner().unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn pull_refuses_to_write_through_symlinked_directories() {
    let registry = MockServer::start();
    let outside = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let c = Client::new(&registry.base_url(), Some("t".into())).unwrap();
    let r = c.pull(BUCKET_ID, &["link/file.tar".to_string()], dir.path(), true, 1, |_| {});
    assert!(matches!(r, Err(Error::Usage(m)) if m.contains("symlinked")));
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}
