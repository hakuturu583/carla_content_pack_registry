//! The whole flow against mock GitHub and S3 servers: the GitHub read is the access check,
//! the credentials come from a release asset, the pack comes straight from the bucket.
mod common;

use std::fs;
use std::path::Path;

use carla_pack_registry::client::ClientOptions;
use carla_pack_registry::manifest::{Filter, PackRef};
use carla_pack_registry::pack::InstallOptions;
use carla_pack_registry::sigv4::Credentials;
use carla_pack_registry::store::Token;
use carla_pack_registry::{Client, Error, PublishOptions, publish, time};
use common::*;
use httpmock::prelude::*;
use serde_json::json;

const REPO: &str = "tier4/carla-packs";
const TOKEN: &str = "gho_user_token";
const BUCKET: &str = "carla-packs";
const KEY: &str = "MiniPack-1.0.0-carla-0.10.0-Linux.tar.gz";

struct World {
    github: MockServer,
    storage: MockServer,
    /// Serves the release asset GitHub redirects to (another origin).
    assets: MockServer,
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        Self {
            github: MockServer::start(),
            storage: MockServer::start(),
            assets: MockServer::start(),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn client(&self, token: Option<&str>, repos: &[&str]) -> Client {
        Client::with_options(ClientOptions {
            token: token.map(|t| Token { value: t.into(), source: "test" }),
            repos: repos.iter().map(|r| r.to_string()).collect(),
            github_api: self.github.base_url(),
            cache_dir: self.dir.path().join("cache"),
        })
        .unwrap()
    }

    fn packs(&self) -> std::path::PathBuf {
        self.dir.path().join("Packs")
    }

    fn manifest_text(&self, credentials: &str, packs: &[(&str, &[u8])]) -> String {
        let mut text = format!(
            "schema = 1\n\n[storage]\nendpoint = \"{}\"\nbucket = \"{BUCKET}\"\nprefix = \"packs/\"\n\
             credentials = \"{credentials}\"\n",
            self.storage.base_url()
        );
        for (version, body) in packs {
            text.push_str(&format!(
                "\n[[pack]]\nname = \"MiniPack\"\nversion = \"{version}\"\nbase_release = \"carla-0.10.0-Linux\"\n\
                 platform = \"Linux\"\nkey = \"MiniPack-{version}-carla-0.10.0-Linux.tar.gz\"\nsha256 = \"{}\"\n\
                 size = {}\nmaps = [\"MiniTown\"]\n",
                hex_sha256(body),
                body.len()
            ));
        }
        text
    }

    /// GitHub serves `carla-packs.toml` of `repo` only to `TOKEN`; everyone else gets 404.
    fn serve_manifest(&self, repo: &str, text: &str) {
        let path = format!("/repos/{repo}/contents/carla-packs.toml");
        self.github.mock(|when, then| {
            when.method(GET).path(&path).header("authorization", format!("Bearer {TOKEN}"));
            then.status(200).body(text);
        });
        self.github.mock(|when, then| {
            when.method(GET).path(&path);
            then.status(404).json_body(json!({ "message": "Not Found" }));
        });
    }

    /// The credential release: the asset URL redirects to another origin, like GitHub's.
    fn serve_credentials(&self, body: serde_json::Value) {
        let asset_url = self.github.url(format!("/repos/{REPO}/releases/assets/42"));
        self.github.mock(|when, then| {
            when.method(GET)
                .path(format!("/repos/{REPO}/releases/tags/carla-pack-credentials"))
                .header("authorization", format!("Bearer {TOKEN}"));
            then.status(200).json_body(json!({ "assets": [{ "name": "credentials.json", "url": asset_url }] }));
        });
        self.github.mock(|when, then| {
            when.method(GET)
                .path(format!("/repos/{REPO}/releases/assets/42"))
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("accept", "application/octet-stream");
            then.status(302).header("location", self.assets.url("/signed/credentials.json"));
        });
        self.assets.mock(|when, then| {
            when.method(GET).path("/signed/credentials.json").header_missing("authorization");
            then.status(200).body(body.to_string());
        });
    }
}

fn creds_file(expires_in: i64) -> serde_json::Value {
    json!({ "version": 1, "access_key_id": "AKIDREAD", "secret_access_key": "s3cr3t", "session_token": "sess",
            "expires_at": time::rfc3339(time::now() + expires_in) })
}

fn reference(s: &str) -> PackRef {
    PackRef::parse(s).unwrap()
}

#[test]
fn github_read_access_unlocks_the_bucket() {
    let w = World::new();
    let body = mini_pack(&[]);
    w.serve_manifest(REPO, &w.manifest_text("github-release", &[("1.0.0", &body)]));
    w.serve_credentials(creds_file(3600));
    let object = w.storage.mock(|when, then| {
        when.method(GET)
            .path(format!("/{BUCKET}/packs/{KEY}"))
            .header("x-amz-security-token", "sess")
            .header("x-amz-content-sha256", "UNSIGNED-PAYLOAD")
            .header_prefix("authorization", "AWS4-HMAC-SHA256 Credential=AKIDREAD/")
            .header_includes("authorization", "/auto/s3/aws4_request")
            .header_excludes("authorization", TOKEN);
        then.status(200).body(body.clone());
    });

    let c = w.client(Some(TOKEN), &[REPO]);
    let release = c.resolve(&reference("tier4/carla-packs/MiniPack"), &Filter::default()).unwrap();
    assert_eq!(release.reference(), "tier4/carla-packs/MiniPack@1.0.0");
    let pack = c.install(&release, &w.packs(), &InstallOptions::default()).unwrap();
    assert_eq!((pack.name.as_str(), pack.maps.as_slice()), ("MiniPack", ["MiniTown".to_string()].as_slice()));
    assert!(w.packs().join("MiniPack/Content/Paks/Linux/MiniPack-Linux.pak").is_file());
    assert!(c.cached_path(&release.pack.sha256).is_file());

    // Installed already: nothing is downloaded again. Another packs dir: served from the cache.
    assert!(c.install(&release, &w.packs(), &InstallOptions::default()).unwrap().already_installed);
    c.install(&release, &w.dir.path().join("Other"), &InstallOptions::default()).unwrap();
    object.assert_calls(1);

    let status = c.credentials_status(REPO).unwrap();
    assert_eq!(status.access_key_id.as_deref(), Some("AKIDREAD"));
    assert!(status.expires_at.is_some());
}

#[test]
fn no_read_access_means_no_pack() {
    let w = World::new();
    w.serve_manifest(REPO, &w.manifest_text("github-release", &[]));
    let r = reference("tier4/carla-packs/MiniPack");

    match w.client(Some("someone_else"), &[]).resolve(&r, &Filter::default()) {
        Err(e @ Error::NoAccess(_)) => {
            assert_eq!(e.code(), "no_access");
            assert!(e.to_string().contains("tier4/carla-packs"));
        }
        other => panic!("unexpected {other:?}"),
    }
    match w.client(None, &[]).resolve(&r, &Filter::default()) {
        Err(Error::Unauthenticated(msg)) => assert!(msg.contains("no GitHub token"), "{msg}"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn expired_or_missing_credentials_are_reported() {
    let w = World::new();
    let body = mini_pack(&[]);
    w.serve_manifest(REPO, &w.manifest_text("github-release", &[("1.0.0", &body)]));
    let c = w.client(Some(TOKEN), &[]);
    let release = c.resolve(&reference("tier4/carla-packs/MiniPack@1.0.0"), &Filter::default()).unwrap();

    // No credential release at all.
    match c.fetch(&release) {
        Err(Error::Credentials(msg)) => assert!(msg.contains("credential workflow"), "{msg}"),
        other => panic!("unexpected {other:?}"),
    }

    w.serve_credentials(creds_file(-60));
    match c.fetch(&release) {
        Err(e @ Error::Credentials(_)) => {
            assert_eq!(e.code(), "credentials_unavailable");
            assert!(e.to_string().contains("expired"), "{e}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(entries(&w.packs()).is_empty());
}

#[test]
fn tampered_downloads_are_discarded() {
    let w = World::new();
    let body = mini_pack(&[]);
    w.serve_manifest(REPO, &w.manifest_text("github-release", &[("1.0.0", &body)]));
    w.serve_credentials(creds_file(3600));
    w.storage.mock(|when, then| {
        when.method(GET).path(format!("/{BUCKET}/packs/{KEY}"));
        then.status(200).body(mini_pack(&[("MiniPack/evil", Member::File(b"x"))]));
    });
    let c = w.client(Some(TOKEN), &[]);
    let release = c.resolve(&reference("tier4/carla-packs/MiniPack"), &Filter::default()).unwrap();
    match c.install(&release, &w.packs(), &InstallOptions::default()) {
        Err(e @ Error::Integrity(_)) => assert_eq!(e.code(), "checksum_mismatch"),
        other => panic!("unexpected {other:?}"),
    }
    assert!(entries(&w.packs()).is_empty());
    assert!(entries(&w.dir.path().join("cache/blobs")).is_empty());
}

#[test]
fn storage_refusals_explain_themselves() {
    let w = World::new();
    let body = mini_pack(&[]);
    w.serve_manifest(REPO, &w.manifest_text("github-release", &[("1.0.0", &body)]));
    w.serve_credentials(creds_file(3600));
    w.storage.mock(|when, then| {
        when.method(GET);
        then.status(403).body("<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>");
    });
    let c = w.client(Some(TOKEN), &[]);
    let release = c.resolve(&reference("tier4/carla-packs/MiniPack"), &Filter::default()).unwrap();
    match c.fetch(&release) {
        Err(Error::Storage { status: 403, message }) => {
            assert!(message.contains("AccessDenied") && message.contains("rotated"), "{message}")
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn anonymous_buckets_get_unsigned_requests() {
    let w = World::new();
    let body = mini_pack(&[]);
    w.serve_manifest(REPO, &w.manifest_text("anonymous", &[("1.0.0", &body)]));
    let object = w.storage.mock(|when, then| {
        when.method(GET).path(format!("/{BUCKET}/packs/{KEY}")).header_missing("authorization");
        then.status(200).body(body.clone());
    });
    let c = w.client(Some(TOKEN), &[]);
    let release = c.resolve(&reference("tier4/carla-packs/MiniPack"), &Filter::default()).unwrap();
    c.fetch(&release).unwrap();
    object.assert();
}

#[test]
fn finds_maps_across_repositories() {
    let w = World::new();
    let v1 = mini_pack(&[]);
    let v2 = mini_pack_version("2.0.0", &[]);
    w.serve_manifest(REPO, &w.manifest_text("github-release", &[("1.0.0", &v1), ("2.0.0", &v2)]));
    w.serve_manifest("other/secret", &w.manifest_text("github-release", &[]));
    let c = w.client(Some(TOKEN), &["someone/unreadable", REPO]);

    // someone/unreadable answers 404 and is skipped.
    let r = c.find_map("/Game/MiniPack/Maps/MiniTown", None, &Filter::default()).unwrap();
    assert_eq!(r.reference(), "tier4/carla-packs/MiniPack@2.0.0");
    let pinned = Filter { sha256: Some(hex_sha256(&v1)), ..Default::default() };
    assert_eq!(c.find_map("MiniTown", None, &pinned).unwrap().pack.version, "1.0.0");

    match c.find_map("Town99", None, &Filter::default()) {
        Err(Error::NotFound(msg)) => assert!(msg.contains("someone/unreadable (no_access)"), "{msg}"),
        other => panic!("unexpected {other:?}"),
    }
    let listed: Vec<String> = c.list_packs(REPO).unwrap().iter().map(|r| r.reference()).collect();
    assert_eq!(listed, ["tier4/carla-packs/MiniPack@2.0.0", "tier4/carla-packs/MiniPack@1.0.0"]);
}

#[test]
fn publish_uploads_and_appends_an_immutable_entry() {
    let w = World::new();
    let manifest = w.dir.path().join("carla-packs.toml");
    fs::write(&manifest, format!("# our packs\n{}", w.manifest_text("github-release", &[]))).unwrap();
    let tarball = w.dir.path().join(KEY);
    let body = mini_pack(&[]);
    fs::write(&tarball, &body).unwrap();

    let head = w.storage.mock(|when, then| {
        when.method("HEAD").path(format!("/{BUCKET}/packs/{KEY}"));
        then.status(404);
    });
    let put = w.storage.mock(|when, then| {
        when.method(PUT)
            .path(format!("/{BUCKET}/packs/{KEY}"))
            .header_exists("authorization")
            .header("content-length", body.len().to_string());
        then.status(200);
    });
    let creds = Credentials { access_key_id: "AKIDWRITE".into(), secret_access_key: "w".into(), session_token: None };
    let opts = PublishOptions { upload: true, ..Default::default() };
    let entry = publish(&manifest, &tarball, &opts, Some(&creds)).unwrap();
    head.assert();
    put.assert();
    assert_eq!((entry.name.as_str(), entry.version.as_str(), entry.key.as_str()), ("MiniPack", "1.0.0", KEY));
    assert_eq!((entry.sha256.clone(), entry.size), (hex_sha256(&body), body.len() as u64));

    let text = fs::read_to_string(&manifest).unwrap();
    assert!(text.starts_with("# our packs\n"));
    let parsed = carla_pack_registry::manifest::Manifest::parse(&text, "t").unwrap();
    assert_eq!(parsed.packs, [entry]);

    match publish(&manifest, &tarball, &PublishOptions::default(), None) {
        Err(Error::Usage(msg)) => assert!(msg.contains("already published"), "{msg}"),
        other => panic!("unexpected {other:?}"),
    }
    let missing = publish(
        &manifest,
        &tarball,
        &PublishOptions { key: Some("x.tar.gz".into()), upload: true, ..Default::default() },
        None,
    );
    assert!(missing.is_err());
}

#[test]
fn the_pack_repository_template_is_a_valid_manifest() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/pack-repo/carla-packs.toml");
    let text = fs::read_to_string(path).unwrap().replace("<ACCOUNT_ID>", "0123456789abcdef0123456789abcdef");
    let m = carla_pack_registry::manifest::Manifest::parse(&text, "template").unwrap();
    assert!(m.packs.is_empty() && m.storage.prefix() == "packs/");
}

#[test]
fn public_packs_come_from_hugging_face_without_any_token() {
    let w = World::new(); // `storage` plays the Hub, `assets` its CDN
    let body = mini_pack(&[]);
    let manifest = format!(
        "schema = 1\n[storage]\ntype = \"huggingface\"\nrepo = \"AutowareFoundation/carla-packs\"\n\
         revision = \"v1\"\nprefix = \"packs/\"\nendpoint = \"{}\"\n\n[[pack]]\nname = \"MiniPack\"\nversion = \"1.0.0\"\n\
         base_release = \"carla-0.10.0-Linux\"\nplatform = \"Linux\"\nkey = \"{KEY}\"\nsha256 = \"{}\"\nsize = {}\n\
         maps = [\"MiniTown\"]\n",
        w.storage.base_url(),
        hex_sha256(&body),
        body.len()
    );
    w.serve_manifest(REPO, &manifest);
    let resolve = w.storage.mock(|when, then| {
        when.method(GET)
            .path(format!("/datasets/AutowareFoundation/carla-packs/resolve/v1/packs/{KEY}"))
            .header_missing("authorization");
        then.status(302).header("location", w.assets.url("/cdn/blob"));
    });
    let cdn = w.assets.mock(|when, then| {
        when.method(GET).path("/cdn/blob").header_missing("authorization");
        then.status(200).body(body.clone());
    });

    let c = w.client(Some(TOKEN), &[REPO]);
    let release = c.find_map("MiniTown", None, &Filter::default()).unwrap();
    let pack = c.install(&release, &w.packs(), &InstallOptions::default()).unwrap();
    assert_eq!(pack.maps, ["MiniTown"]);
    resolve.assert();
    cdn.assert();

    let status = c.credentials_status(REPO).unwrap(); // no credential release is looked up
    assert_eq!(
        (status.storage.as_str(), status.access_key_id),
        ("Hugging Face dataset AutowareFoundation/carla-packs", None)
    );

    // publish never uploads there, but says how; --no-upload appends the entry.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("carla-packs.toml");
    fs::write(&file, manifest.split("\n[[pack]]").next().unwrap()).unwrap();
    let tarball = dir.path().join("MiniPack-2.0.0.tar.gz");
    fs::write(&tarball, mini_pack_version("2.0.0", &[])).unwrap();
    match publish(&file, &tarball, &PublishOptions { upload: true, ..Default::default() }, None) {
        Err(Error::Usage(msg)) => assert!(
            msg.contains("hf upload AutowareFoundation/carla-packs") && msg.contains("packs/MiniPack-2.0.0.tar.gz"),
            "{msg}"
        ),
        other => panic!("unexpected {other:?}"),
    }
    let entry = publish(&file, &tarball, &PublishOptions::default(), None).unwrap();
    assert_eq!(entry.version, "2.0.0");
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("type = \"huggingface\"") && text.contains("version = \"2.0.0\""), "{text}");
}

#[test]
fn the_hugging_face_template_is_a_valid_manifest() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/pack-repo/carla-packs.huggingface.toml");
    let m = carla_pack_registry::manifest::Manifest::parse(&fs::read_to_string(path).unwrap(), "template").unwrap();
    assert!(matches!(m.storage, carla_pack_registry::manifest::Storage::HuggingFace(_)));
}
