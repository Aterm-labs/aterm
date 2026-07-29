//! The GitLab and GitHub drivers against a real HTTP server on localhost.
//!
//! Recorded responses would pass while the drivers spoke nonsense over the
//! wire; a socket catches the two asymmetries that actually break publishing:
//! GitLab rejecting a `POST` over an existing file (so republishing must retry
//! as `PUT`), and GitHub demanding the blob `sha` to overwrite one.
//!
//! Skipped when curl isn't installed — the transport is curl by design.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use agent_sessions_remote::http::{self, HttpRepoRemote};
use agent_sessions_remote::manifest::{timestamp_from_unix, RemoteManifest, FORMAT, VERSION};
use agent_sessions_remote::payload::{PendingArtifact, Source};
use agent_sessions_remote::store;

type Files = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

fn curl_available() -> bool {
    std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_ok()
}

fn manifest(id: &str) -> RemoteManifest {
    RemoteManifest {
        format: FORMAT.to_string(),
        version: VERSION,
        id: id.to_string(),
        provider: "claude".to_string(),
        published_at: timestamp_from_unix(1_760_000_000),
        published_by: Some("ana@example.com".to_string()),
        cwd: Some("/w".to_string()),
        branch: None,
        git_remote: None,
        git_head: None,
        display_name: None,
        tags: Vec::new(),
        first_prompt: None,
        message_count: None,
        size_bytes: 0,
        resumable: true,
        origin_filename: None,
        artifacts: Vec::new(),
        forked_from: None,
    }
}

fn artifacts(body: &[u8]) -> Vec<PendingArtifact> {
    vec![
        PendingArtifact {
            path: "session.jsonl".to_string(),
            source: Source::Bytes(body.to_vec()),
            gzip: true,
        },
        PendingArtifact {
            path: "sub/subagents/a.meta.json".to_string(),
            source: Source::Bytes(b"{}".to_vec()),
            gzip: false,
        },
    ]
}

#[test]
fn gitlab_publishes_lists_fetches_and_republishes_over_an_existing_file() {
    if !curl_available() {
        return;
    }
    let (host, files) = serve(Flavour::GitLab);
    let remote = HttpRepoRemote::new_gitlab(
        host,
        "equipo/sesiones".to_string(),
        "main".to_string(),
        "token-de-prueba".to_string(),
    );

    let published = store::publish(&remote, &manifest("s-1"), &artifacts(b"primera")).unwrap();
    {
        let stored = files.lock().unwrap();
        assert!(stored.contains_key("manifest/s-1.json"));
        assert!(stored.contains_key("blobs/s-1/session.jsonl.gz"));
        // The small sidecar travels uncompressed, and intact.
        assert_eq!(
            stored.get("blobs/s-1/sub/subagents/a.meta.json").unwrap(),
            b"{}"
        );
    }

    let listed = store::list(&remote).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "s-1");
    let main = listed[0]
        .artifacts
        .iter()
        .find(|a| a.path == "session.jsonl")
        .unwrap();
    assert_eq!(store::fetch(&remote, "s-1", main).unwrap(), b"primera");

    // Republishing hits files that already exist: GitLab answers 400 to the
    // POST and the driver has to retry as PUT.
    store::publish(&remote, &manifest("s-1"), &artifacts(b"segunda, mas larga")).unwrap();
    let listed = store::list(&remote).unwrap();
    let main = listed[0]
        .artifacts
        .iter()
        .find(|a| a.path == "session.jsonl")
        .unwrap();
    assert_eq!(
        store::fetch(&remote, "s-1", main).unwrap(),
        b"segunda, mas larga"
    );
    assert!(published.size_bytes < listed[0].size_bytes);

    // And unpublishing removes manifest and blobs alike.
    store::unpublish(&remote, "s-1").unwrap();
    assert!(store::list(&remote).unwrap().is_empty());
    assert!(files.lock().unwrap().is_empty());
}

#[test]
fn github_publishes_and_overwrites_with_the_blob_sha() {
    if !curl_available() {
        return;
    }
    let (host, files) = serve(Flavour::GitHub);
    let remote = HttpRepoRemote::new_github(
        host,
        "equipo/sesiones".to_string(),
        "main".to_string(),
        "token-de-prueba".to_string(),
    );

    store::publish(&remote, &manifest("s-2"), &artifacts(b"primera")).unwrap();
    // Overwriting without the sha is a 409 in the fake server, exactly as it
    // is in GitHub: if the driver forgot the pre-read, this fails.
    store::publish(&remote, &manifest("s-2"), &artifacts(b"segunda")).unwrap();

    let listed = store::list(&remote).unwrap();
    assert_eq!(listed.len(), 1);
    let main = listed[0]
        .artifacts
        .iter()
        .find(|a| a.path == "session.jsonl")
        .unwrap();
    assert_eq!(store::fetch(&remote, "s-2", main).unwrap(), b"segunda");

    store::unpublish(&remote, "s-2").unwrap();
    assert!(files.lock().unwrap().is_empty());
}

#[test]
fn an_expired_token_produces_a_sentence_not_a_status_code() {
    if !curl_available() {
        return;
    }
    let (host, _files) = serve(Flavour::GitLab);
    let remote = HttpRepoRemote::new_gitlab(
        host,
        "equipo/sesiones".to_string(),
        "main".to_string(),
        String::new(), // No token: the fake server answers 401.
    );
    let err = store::list(&remote).unwrap_err();
    assert!(
        err.contains("no es válido") || err.contains("caducado"),
        "{err}"
    );
}

#[test]
fn probe_names_the_repository_it_reached() {
    if !curl_available() {
        return;
    }
    let (host, _files) = serve(Flavour::GitLab);
    let remote = HttpRepoRemote::new_gitlab(
        host,
        "equipo/sesiones".to_string(),
        "main".to_string(),
        "token-de-prueba".to_string(),
    );
    let said = http::probe_http(&remote).unwrap();
    assert!(said.contains("equipo/sesiones"), "{said}");
}

// ---------------------------------------------------------------------------
// A minimal stand-in for each provider's contents API.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Flavour {
    GitLab,
    GitHub,
}

/// Start the fake server on an ephemeral port; returns its base URL and the
/// file map so tests can assert on what really landed.
fn serve(flavour: Flavour) -> (String, Files) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("puerto libre");
    let port = listener.local_addr().unwrap().port();
    let files: Files = Arc::new(Mutex::new(BTreeMap::new()));
    let shared = Arc::clone(&files);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            handle(stream, flavour, &shared);
        }
    });
    (format!("http://127.0.0.1:{port}"), files)
}

struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    accept: String,
    token: bool,
    body: Vec<u8>,
}

fn handle(mut stream: TcpStream, flavour: Flavour, files: &Files) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    if !request.token {
        return respond(
            &mut stream,
            401,
            b"{\"message\":\"401 Unauthorized\"}".to_vec(),
        );
    }
    let (status, body) = match flavour {
        Flavour::GitLab => gitlab(&request, files),
        Flavour::GitHub => github(&request, files),
    };
    respond(&mut stream, status, body);
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut start = String::new();
    reader.read_line(&mut start).ok()?;
    let mut parts = start.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let (path, query_raw) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let mut query = BTreeMap::new();
    for pair in query_raw.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(k.to_string(), urldecode(v));
    }
    let (mut length, mut accept, mut token) = (0usize, String::new(), false);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 || line.trim().is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = lower.strip_prefix("accept:") {
            accept = value.trim().to_string();
        } else if lower.starts_with("private-token:") || lower.starts_with("authorization:") {
            token = line
                .trim()
                .split(':')
                .nth(1)
                .is_some_and(|v| !v.trim().is_empty());
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(Request {
        method,
        path: urldecode(path),
        query,
        accept,
        token,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u32, body: Vec<u8>) {
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn json(value: serde_json::Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

/// Content sent by either driver: base64 under `content`.
fn decoded_body(request: &Request) -> Option<Vec<u8>> {
    let parsed: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
    http::base64_decode(parsed.get("content")?.as_str()?)
}

fn tree(files: &Files, prefix: &str) -> Vec<String> {
    files
        .lock()
        .unwrap()
        .keys()
        .filter(|p| prefix.is_empty() || p.starts_with(prefix))
        .cloned()
        .collect()
}

fn gitlab(request: &Request, files: &Files) -> (u32, Vec<u8>) {
    let prefix = "/api/v4/projects/equipo/sesiones";
    let Some(rest) = request.path.strip_prefix(prefix) else {
        return (
            404,
            json(serde_json::json!({"message": "404 Project Not Found"})),
        );
    };
    if rest.is_empty() {
        return (
            200,
            json(serde_json::json!({"path_with_namespace": "equipo/sesiones"})),
        );
    }
    if let Some(page) = request.query.get("page") {
        if rest.starts_with("/repository/tree") && page != "1" {
            return (200, json(serde_json::json!([])));
        }
    }
    if rest.starts_with("/repository/tree") {
        let empty = String::new();
        let path = request.query.get("path").unwrap_or(&empty);
        let entries: Vec<serde_json::Value> = tree(files, path)
            .into_iter()
            .map(|p| serde_json::json!({"path": p, "type": "blob"}))
            .collect();
        return (200, json(serde_json::Value::Array(entries)));
    }
    let Some(file_part) = rest.strip_prefix("/repository/files/") else {
        return (404, Vec::new());
    };
    let path = file_part.trim_end_matches("/raw").to_string();
    let mut stored = files.lock().unwrap();
    match request.method.as_str() {
        "GET" => match stored.get(&path) {
            Some(bytes) => (200, bytes.clone()),
            None => (
                404,
                json(serde_json::json!({"message": "404 File Not Found"})),
            ),
        },
        // No upsert: creating an existing file is a 400, which is what forces
        // the driver's POST→PUT retry.
        "POST" if stored.contains_key(&path) => (
            400,
            json(serde_json::json!({"message": "A file with this name already exists"})),
        ),
        "POST" | "PUT" => {
            if request.method == "PUT" && !stored.contains_key(&path) {
                return (
                    400,
                    json(serde_json::json!({"message": "File does not exist"})),
                );
            }
            match decoded_body(request) {
                Some(bytes) => {
                    stored.insert(path, bytes);
                    (if request.method == "POST" { 201 } else { 200 }, Vec::new())
                }
                None => (400, json(serde_json::json!({"message": "bad content"}))),
            }
        }
        "DELETE" => {
            stored.remove(&path);
            (204, Vec::new())
        }
        _ => (405, Vec::new()),
    }
}

fn github(request: &Request, files: &Files) -> (u32, Vec<u8>) {
    let prefix = "/repos/equipo/sesiones";
    let Some(rest) = request.path.strip_prefix(prefix) else {
        return (404, json(serde_json::json!({"message": "Not Found"})));
    };
    if rest.is_empty() {
        return (
            200,
            json(serde_json::json!({"full_name": "equipo/sesiones"})),
        );
    }
    let Some(path) = rest.strip_prefix("/contents/") else {
        return (404, Vec::new());
    };
    let path = path.to_string();
    let mut stored = files.lock().unwrap();
    match request.method.as_str() {
        "GET" => {
            if let Some(bytes) = stored.get(&path) {
                return if request.accept.contains("raw") {
                    (200, bytes.clone())
                } else {
                    (
                        200,
                        json(
                            serde_json::json!({"path": path, "type": "file", "sha": sha_of(bytes)}),
                        ),
                    )
                };
            }
            // Directory listing: one level only, as the real contents API does.
            let children: Vec<serde_json::Value> = stored
                .keys()
                .filter(|p| p.starts_with(&format!("{path}/")))
                .map(|p| {
                    let rest = &p[path.len() + 1..];
                    match rest.split_once('/') {
                        Some((dir, _)) => {
                            serde_json::json!({"path": format!("{path}/{dir}"), "type": "dir"})
                        }
                        None => serde_json::json!({"path": p, "type": "file"}),
                    }
                })
                .collect();
            if children.is_empty() {
                return (404, json(serde_json::json!({"message": "Not Found"})));
            }
            let mut unique: Vec<serde_json::Value> = Vec::new();
            for child in children {
                if !unique.contains(&child) {
                    unique.push(child);
                }
            }
            (200, json(serde_json::Value::Array(unique)))
        }
        "PUT" => {
            let parsed: serde_json::Value =
                serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null);
            let given_sha = parsed.get("sha").and_then(serde_json::Value::as_str);
            if let Some(existing) = stored.get(&path) {
                // Overwriting without the sha is a conflict, same as GitHub.
                if given_sha != Some(sha_of(existing).as_str()) {
                    return (
                        409,
                        json(serde_json::json!({"message": "sha does not match"})),
                    );
                }
            }
            match decoded_body(request) {
                Some(bytes) => {
                    stored.insert(path, bytes);
                    (200, Vec::new())
                }
                None => (400, Vec::new()),
            }
        }
        "DELETE" => {
            stored.remove(&path);
            (200, Vec::new())
        }
        _ => (405, Vec::new()),
    }
}

/// Stand-in for git's blob hash: the drivers only ever compare it to itself.
fn sha_of(bytes: &[u8]) -> String {
    let mut hash: u64 = 1469598103934665603;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

fn urldecode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
