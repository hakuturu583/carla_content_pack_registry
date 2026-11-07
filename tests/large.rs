//! Packs of several gigabytes: multipart uploads and resumed downloads.
mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use carla_pack_registry::client::ClientOptions;
use carla_pack_registry::manifest::{Filter, PackRef};
use carla_pack_registry::sigv4::Credentials;
use carla_pack_registry::store::Token;
use carla_pack_registry::{Client, Error, PublishOptions, publish, time};
use common::*;
use httpmock::prelude::*;
use serde_json::json;

const MIB: u64 = 1024 * 1024;
const KEY: &str = "MiniPack-1.0.0-carla-0.10.0-Linux.tar.gz";

fn manifest_text(endpoint: &str, packs: &str) -> String {
    format!(
        "schema = 1\n[storage]\nendpoint = \"{endpoint}\"\nbucket = \"carla-packs\"\nprefix = \"packs/\"\n\
         credentials = \"github-release\"\n{packs}"
    )
}

fn writer() -> Credentials {
    Credentials { access_key_id: "AKIDWRITE".into(), secret_access_key: "w".into(), session_token: None }
}

#[test]
fn large_files_go_up_in_signed_parts() {
    let storage = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("carla-packs.toml");
    fs::write(&manifest, manifest_text(&storage.base_url(), "")).unwrap();
    let body = big_pack(11 * MIB as usize);
    let tarball = dir.path().join(KEY);
    fs::write(&tarball, &body).unwrap();
    let path = format!("/carla-packs/packs/{KEY}");

    storage.mock(|when, then| {
        when.method("HEAD").path(&path);
        then.status(404);
    });
    let create = storage.mock(|when, then| {
        when.method(POST)
            .path(&path)
            .query_param("uploads", "")
            .header_includes("authorization", "Credential=AKIDWRITE/");
        then.status(200)
            .body("<InitiateMultipartUploadResult><UploadId>up/1+x</UploadId></InitiateMultipartUploadResult>");
    });
    let mut parts = Vec::new();
    for (n, len) in [(1, 5 * MIB), (2, 5 * MIB), (3, body.len() as u64 - 10 * MIB)] {
        parts.push(storage.mock(|when, then| {
            when.method(PUT)
                .path(&path)
                .query_param("partNumber", n.to_string())
                .query_param("uploadId", "up/1+x")
                .header("content-length", len.to_string())
                .header_exists("authorization");
            then.status(200).header("etag", format!("\"etag-{n}\""));
        }));
    }
    let complete = storage.mock(|when, then| {
        when.method(POST).path(&path).query_param("uploadId", "up/1+x").body(
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"etag-1\"</ETag></Part>\
                   <Part><PartNumber>2</PartNumber><ETag>\"etag-2\"</ETag></Part>\
                   <Part><PartNumber>3</PartNumber><ETag>\"etag-3\"</ETag></Part></CompleteMultipartUpload>",
        );
        then.status(200).body("<CompleteMultipartUploadResult><ETag>\"x-3\"</ETag></CompleteMultipartUploadResult>");
    });

    let opts = PublishOptions { upload: true, part_size: Some(5 * MIB), ..Default::default() };
    let entry = publish(&manifest, &tarball, &opts, Some(&writer())).unwrap();
    create.assert();
    for p in &parts {
        p.assert();
    }
    complete.assert();
    assert_eq!(entry.size, body.len() as u64);
}

#[test]
fn a_failed_multipart_upload_is_aborted_and_publishes_nothing() {
    let storage = MockServer::start();
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("carla-packs.toml");
    let original = manifest_text(&storage.base_url(), "");
    fs::write(&manifest, &original).unwrap();
    let tarball = dir.path().join(KEY);
    fs::write(&tarball, big_pack(6 * MIB as usize)).unwrap();
    let path = format!("/carla-packs/packs/{KEY}");

    storage.mock(|when, then| {
        when.method("HEAD").path(&path);
        then.status(404);
    });
    storage.mock(|when, then| {
        when.method(POST).path(&path).query_param("uploads", "");
        then.status(200).body("<InitiateMultipartUploadResult><UploadId>u2</UploadId></InitiateMultipartUploadResult>");
    });
    storage.mock(|when, then| {
        when.method(PUT).path(&path).query_param("partNumber", "2");
        then.status(400).body("<Error><Code>EntityTooLarge</Code><Message>no</Message></Error>");
    });
    storage.mock(|when, then| {
        when.method(PUT).path(&path).query_param("partNumber", "1");
        then.status(200).header("etag", "\"e1\"");
    });
    let abort = storage.mock(|when, then| {
        when.method(DELETE).path(&path).query_param("uploadId", "u2");
        then.status(204);
    });

    let opts = PublishOptions { upload: true, part_size: Some(5 * MIB), ..Default::default() };
    match publish(&manifest, &tarball, &opts, Some(&writer())) {
        Err(Error::Storage { status: 400, message }) => assert!(message.contains("EntityTooLarge"), "{message}"),
        other => panic!("unexpected {other:?}"),
    }
    abort.assert();
    assert_eq!(fs::read_to_string(&manifest).unwrap(), original); // no entry for a file that is not there
}

/// One raw HTTP exchange the storage stub saw.
#[derive(Debug, Clone)]
struct Seen {
    request_line: String,
    headers: Vec<(String, String)>,
}

/// A storage stub that cuts every response off after `cut` bytes and honours `Range: bytes=N-`.
fn flaky_storage(object: Vec<u8>, cut: usize) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                let (k, v) = line.split_once(':').unwrap();
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
            let start = headers
                .iter()
                .find(|(k, _)| k == "range")
                .map(|(_, v)| v.trim_start_matches("bytes=").trim_end_matches('-').parse::<usize>().unwrap());
            log.lock().unwrap().push(Seen { request_line: request_line.trim().into(), headers });
            let rest = &object[start.unwrap_or(0)..];
            let head = match start {
                None => format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", rest.len()),
                Some(s) => format!(
                    "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\ncontent-range: bytes {s}-{}/{}\r\n\
                     connection: close\r\n\r\n",
                    rest.len(),
                    object.len() - 1,
                    object.len()
                ),
            };
            stream.write_all(head.as_bytes()).unwrap();
            let _ = stream.write_all(&rest[..rest.len().min(cut)]);
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Both); // cut off
        }
    });
    (endpoint, seen)
}

#[test]
fn broken_downloads_resume_where_they_stopped() {
    let body = big_pack(3 * MIB as usize);
    let cut = MIB as usize + 12_345;
    let (endpoint, seen) = flaky_storage(body.clone(), cut);

    let github = MockServer::start();
    let entry = format!(
        "[[pack]]\nname = \"MiniPack\"\nversion = \"1.0.0\"\nbase_release = \"carla-0.10.0-Linux\"\nplatform = \"Linux\"\n\
         key = \"{KEY}\"\nsha256 = \"{}\"\nsize = {}\nmaps = [\"MiniTown\"]\n",
        hex_sha256(&body),
        body.len()
    );
    github.mock(|when, then| {
        when.method(GET).path("/repos/o/packs/contents/carla-packs.toml");
        then.status(200).body(manifest_text(&endpoint, &entry));
    });
    github.mock(|when, then| {
        when.method(GET).path("/repos/o/packs/releases/tags/carla-pack-credentials");
        then.status(200)
            .json_body(json!({ "assets": [{ "name": "credentials.json", "url": github.url("/repos/o/packs/releases/assets/1") }] }));
    });
    github.mock(|when, then| {
        when.method(GET).path("/repos/o/packs/releases/assets/1");
        then.status(200).json_body(json!({ "version": 1, "access_key_id": "AKIDREAD", "secret_access_key": "r",
                                           "expires_at": time::rfc3339(time::now() + 3600) }));
    });
    let dir = tempfile::tempdir().unwrap();
    let client = Client::with_options(ClientOptions {
        token: Some(Token { value: "t".into(), source: "test" }),
        repos: vec![],
        github_api: github.base_url(),
        cache_dir: dir.path().join("cache"),
    })
    .unwrap();
    let release = client.resolve(&PackRef::parse("o/packs/MiniPack").unwrap(), &Filter::default()).unwrap();
    let path = client.fetch(&release).unwrap();
    assert_eq!(fs::read(path).unwrap(), body);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), body.len().div_ceil(cut)); // one request per cut-off piece
    for (i, s) in seen.iter().enumerate() {
        let header = |k: &str| s.headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert!(s.request_line.starts_with(&format!("GET /carla-packs/packs/{KEY}")), "{}", s.request_line);
        let auth = header("authorization").unwrap();
        if i == 0 {
            assert_eq!(header("range"), None);
        } else {
            assert_eq!(header("range"), Some(format!("bytes={}-", i * cut).as_str()));
            assert!(auth.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"), "{auth}");
        }
    }
}
