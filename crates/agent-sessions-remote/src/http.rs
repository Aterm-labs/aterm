//! GitLab and GitHub through their REST APIs, without cloning anything.
//!
//! Both providers reduce to the same four operations with different endpoints,
//! so the shape lives in [`HttpRepoRemote`] and each provider only supplies
//! URLs, headers and the two asymmetries that don't cancel out:
//!
//! - **GitLab has no upsert**: creating a file that already exists is a 400,
//!   so a write retries as `PUT`. Without that, republishing would always fail.
//! - **GitHub demands the blob's `sha`** to overwrite, which forces a `GET`
//!   before every write of an existing file.
//!
//! One commit per file, not one per session. GitLab's multi-action commit
//! endpoint would make a publication atomic, but GitHub has no direct
//! equivalent and keeping two different paths through the most delicate part
//! of the feature is worse than a noisier history — the invariant that
//! actually matters (a half-finished publication is invisible, not broken) is
//! preserved either way by writing the manifest last.
//!
//! Transport is `curl`, the same choice `aterm`'s service_status made: a real
//! HTTP client crate would pull a TLS stack into a binary whose whole point is
//! being small, and every machine that can reach GitLab already has curl.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::store::RepoBackend;

const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavour {
    GitLab,
    GitHub,
}

pub struct HttpRepoRemote {
    flavour: Flavour,
    /// API root, e.g. `https://gitlab.com` or `https://api.github.com`.
    host: String,
    /// `group/repo`.
    repo: String,
    branch: String,
    token: String,
}

pub type GitLabRemote = HttpRepoRemote;
pub type GitHubRemote = HttpRepoRemote;

impl HttpRepoRemote {
    pub fn new_gitlab(host: String, repo: String, branch: String, token: String) -> Self {
        Self {
            flavour: Flavour::GitLab,
            host,
            repo,
            branch,
            token,
        }
    }

    pub fn new_github(host: String, repo: String, branch: String, token: String) -> Self {
        Self {
            flavour: Flavour::GitHub,
            host,
            repo,
            branch,
            token,
        }
    }

    fn headers(&self) -> Vec<String> {
        match self.flavour {
            Flavour::GitLab => vec![format!("PRIVATE-TOKEN: {}", self.token)],
            Flavour::GitHub => vec![
                format!("Authorization: Bearer {}", self.token),
                // Pinning the API version keeps a server-side default change
                // from silently altering responses.
                "X-GitHub-Api-Version: 2022-11-28".to_string(),
                "Accept: application/vnd.github+json".to_string(),
            ],
        }
    }

    fn project_id(&self) -> String {
        urlencode(self.repo.trim_matches('/'))
    }

    fn files_url(&self, path: &str) -> String {
        match self.flavour {
            Flavour::GitLab => format!(
                "{}/api/v4/projects/{}/repository/files/{}",
                self.host,
                self.project_id(),
                urlencode(path)
            ),
            Flavour::GitHub => format!(
                "{}/repos/{}/contents/{}",
                self.host,
                self.repo.trim_matches('/'),
                path
            ),
        }
    }

    /// The `sha` GitHub needs to overwrite a file, or None when it's new.
    fn github_sha(&self, path: &str) -> Option<String> {
        let url = format!("{}?ref={}", self.files_url(path), self.branch);
        let response = request("GET", &url, &self.headers(), None).ok()?;
        if !response.ok() {
            return None;
        }
        let parsed: serde_json::Value = serde_json::from_slice(&response.body).ok()?;
        parsed
            .get("sha")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    /// Recursive listing. GitLab does it server-side; GitHub's contents API
    /// only lists one level, so we walk it.
    fn list_github(&self, prefix: &str, out: &mut Vec<String>) -> Result<(), String> {
        let url = format!("{}?ref={}", self.files_url(prefix), self.branch);
        let response = request("GET", &url, &self.headers(), None)?;
        if response.status == 404 {
            return Ok(()); // Nothing published yet under this prefix.
        }
        if !response.ok() {
            return Err(response.explain(&self.repo));
        }
        let parsed: serde_json::Value =
            serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
        let Some(entries) = parsed.as_array() else {
            return Ok(()); // A single file, not a directory listing.
        };
        for entry in entries {
            let (Some(path), Some(kind)) = (
                entry.get("path").and_then(serde_json::Value::as_str),
                entry.get("type").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            match kind {
                "dir" => self.list_github(path, out)?,
                "file" => out.push(path.to_string()),
                _ => {}
            }
        }
        Ok(())
    }
}

impl RepoBackend for HttpRepoRemote {
    fn describe(&self) -> String {
        format!("{} ({})", self.repo, self.host)
    }

    fn list_files(&self, prefix: &str) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        match self.flavour {
            Flavour::GitHub => self.list_github(prefix, &mut out)?,
            Flavour::GitLab => {
                let mut page = 1;
                loop {
                    let url = format!(
                        "{}/api/v4/projects/{}/repository/tree?ref={}&path={}&recursive=true&per_page=100&page={page}",
                        self.host,
                        self.project_id(),
                        urlencode(&self.branch),
                        urlencode(prefix)
                    );
                    let response = request("GET", &url, &self.headers(), None)?;
                    if response.status == 404 {
                        break; // Empty repository or nothing under the prefix.
                    }
                    if !response.ok() {
                        return Err(response.explain(&self.repo));
                    }
                    let parsed: serde_json::Value =
                        serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
                    let entries = parsed.as_array().cloned().unwrap_or_default();
                    if entries.is_empty() {
                        break;
                    }
                    for entry in &entries {
                        if entry.get("type").and_then(serde_json::Value::as_str) == Some("blob") {
                            if let Some(path) =
                                entry.get("path").and_then(serde_json::Value::as_str)
                            {
                                out.push(path.to_string());
                            }
                        }
                    }
                    if entries.len() < 100 {
                        break;
                    }
                    page += 1;
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        let (url, mut headers) = match self.flavour {
            Flavour::GitLab => (
                format!(
                    "{}/raw?ref={}",
                    self.files_url(path),
                    urlencode(&self.branch)
                ),
                self.headers(),
            ),
            Flavour::GitHub => (
                format!("{}?ref={}", self.files_url(path), urlencode(&self.branch)),
                self.headers(),
            ),
        };
        if self.flavour == Flavour::GitHub {
            // Raw media type: the JSON form would base64 the body for nothing.
            headers.retain(|h| !h.starts_with("Accept:"));
            headers.push("Accept: application/vnd.github.raw".to_string());
        }
        let response = request("GET", &url, &headers, None)?;
        if !response.ok() {
            return Err(response.explain(&self.repo));
        }
        Ok(response.body)
    }

    fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let content = base64_encode(bytes);
        let message = format!("aterm: actualiza {path}");
        match self.flavour {
            Flavour::GitLab => {
                let body = serde_json::json!({
                    "branch": self.branch,
                    "content": content,
                    "encoding": "base64",
                    "commit_message": message,
                })
                .to_string();
                let url = self.files_url(path);
                let mut headers = self.headers();
                headers.push("Content-Type: application/json".to_string());
                let created = request("POST", &url, &headers, Some(body.as_bytes()))?;
                if created.ok() {
                    return Ok(());
                }
                // No upsert: an existing file rejects the POST, so update it.
                let updated = request("PUT", &url, &headers, Some(body.as_bytes()))?;
                if updated.ok() {
                    return Ok(());
                }
                Err(updated.explain(&self.repo))
            }
            Flavour::GitHub => {
                let mut payload = serde_json::json!({
                    "message": message,
                    "content": content,
                    "branch": self.branch,
                });
                if let Some(sha) = self.github_sha(path) {
                    payload["sha"] = serde_json::Value::String(sha);
                }
                let mut headers = self.headers();
                headers.push("Content-Type: application/json".to_string());
                let response = request(
                    "PUT",
                    &self.files_url(path),
                    &headers,
                    Some(payload.to_string().as_bytes()),
                )?;
                if response.ok() {
                    return Ok(());
                }
                Err(response.explain(&self.repo))
            }
        }
    }

    fn delete_file(&self, path: &str) -> Result<(), String> {
        let message = format!("aterm: elimina {path}");
        let mut headers = self.headers();
        headers.push("Content-Type: application/json".to_string());
        let response = match self.flavour {
            Flavour::GitLab => {
                let body = serde_json::json!({
                    "branch": self.branch,
                    "commit_message": message,
                })
                .to_string();
                request(
                    "DELETE",
                    &self.files_url(path),
                    &headers,
                    Some(body.as_bytes()),
                )?
            }
            Flavour::GitHub => {
                let Some(sha) = self.github_sha(path) else {
                    return Ok(()); // Already gone.
                };
                let body = serde_json::json!({
                    "message": message,
                    "sha": sha,
                    "branch": self.branch,
                })
                .to_string();
                request(
                    "DELETE",
                    &self.files_url(path),
                    &headers,
                    Some(body.as_bytes()),
                )?
            }
        };
        // Idempotent: an already-absent file is a finished job.
        if response.ok() || response.status == 404 {
            return Ok(());
        }
        Err(response.explain(&self.repo))
    }
}

/// Confirm the token can reach the repository, and say which repository it
/// resolved to — that string is what tells a user they typed the right one.
pub fn probe_http(remote: &HttpRepoRemote) -> Result<String, String> {
    let url = match remote.flavour {
        Flavour::GitLab => format!("{}/api/v4/projects/{}", remote.host, remote.project_id()),
        Flavour::GitHub => format!("{}/repos/{}", remote.host, remote.repo.trim_matches('/')),
    };
    let response = request("GET", &url, &remote.headers(), None)?;
    if !response.ok() {
        return Err(response.explain(&remote.repo));
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
    let name = parsed
        .get("path_with_namespace")
        .or_else(|| parsed.get("full_name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&remote.repo);
    Ok(format!("acceso confirmado a {name}"))
}

struct Response {
    status: u32,
    body: Vec<u8>,
}

impl Response {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Turn a status code into something the user can act on. The API's own
    /// `message` field is appended when there is one, because "insufficient
    /// scope" is more useful than any wording we could invent.
    fn explain(&self, repo: &str) -> String {
        let detail = serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .or_else(|| v.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let base = match self.status {
            401 => "el token no es válido o ha caducado".to_string(),
            403 => format!("el token no tiene permiso de escritura sobre {repo}"),
            404 => format!("no encuentro el repositorio {repo} (¿nombre o rama equivocados?)"),
            other => format!("el servidor respondió {other}"),
        };
        if detail.is_empty() {
            base
        } else {
            format!("{base} — {detail}")
        }
    }
}

/// One HTTP request through curl. The body goes to a temporary file rather
/// than stdout so binary payloads (a gzipped transcript) survive intact and
/// the status code can be read separately.
fn request(
    method: &str,
    url: &str,
    headers: &[String],
    body: Option<&[u8]>,
) -> Result<Response, String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = format!(
        "aterm-remote-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let out_path = std::env::temp_dir().join(format!("{stamp}.out"));
    let body_path = std::env::temp_dir().join(format!("{stamp}.in"));

    let mut cmd = Command::new("curl");
    cmd.arg("-sS")
        .args(["-X", method])
        .args(["-o", &out_path.display().to_string()])
        .args(["-w", "%{http_code}"])
        .args(["--max-time", &HTTP_TIMEOUT.as_secs().to_string()]);
    for header in headers {
        cmd.args(["-H", header]);
    }
    if let Some(body) = body {
        std::fs::write(&body_path, body).map_err(|e| e.to_string())?;
        cmd.args(["--data-binary", &format!("@{}", body_path.display())]);
    }
    cmd.arg(url);

    let result = run(cmd);
    let _ = std::fs::remove_file(&body_path);
    let output = match result {
        Ok(output) => output,
        Err(e) => {
            let _ = std::fs::remove_file(&out_path);
            return Err(e);
        }
    };
    let payload = std::fs::read(&out_path).unwrap_or_default();
    let _ = std::fs::remove_file(&out_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "curl no pudo completar la petición".to_string()
        } else {
            stderr
        });
    }
    let status = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or(0);
    Ok(Response {
        status,
        body: payload,
    })
}

fn run(mut cmd: Command) -> Result<std::process::Output, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("no puedo ejecutar curl: {e}"))?;
    let started = Instant::now();
    let limit = HTTP_TIMEOUT + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= limit {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("la petición no respondió a tiempo".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    child.wait_with_output().map_err(|e| e.to_string())
}

/// Percent-encoding for a path segment: repository ids and file paths both
/// travel inside the URL, and `/` must not survive as a separator.
fn urlencode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Both APIs take file content base64-encoded. Writing the 20 lines here beats
/// a dependency for it.
pub fn base64_encode(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len().div_ceil(3) * 4);
    for chunk in raw.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Only needed by the tests and by any caller reading GitHub's JSON form.
pub fn base64_decode(raw: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for c in raw.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break;
        }
        let value = B64.iter().position(|b| *b == c)? as u32;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_including_padding_cases() {
        for raw in [
            &b""[..],
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            &[0u8, 255, 128, 7, 90][..],
        ] {
            let encoded = base64_encode(raw);
            assert_eq!(base64_decode(&encoded).unwrap(), raw, "{encoded}");
        }
        assert_eq!(base64_encode(b"hola"), "aG9sYQ==");
    }

    #[test]
    fn urlencode_escapes_the_slash_that_would_split_the_path() {
        assert_eq!(urlencode("grupo/repo"), "grupo%2Frepo");
        assert_eq!(
            urlencode("manifest/abc-123.json"),
            "manifest%2Fabc-123.json"
        );
    }

    #[test]
    fn errors_are_sentences_with_the_api_detail_appended() {
        let unauthorised = Response {
            status: 401,
            body: br#"{"message":"401 Unauthorized"}"#.to_vec(),
        };
        let text = unauthorised.explain("equipo/sesiones");
        assert!(text.contains("no es válido"), "{text}");
        assert!(text.contains("401 Unauthorized"), "{text}");

        let missing = Response {
            status: 404,
            body: Vec::new(),
        };
        assert!(missing
            .explain("equipo/sesiones")
            .contains("equipo/sesiones"));
    }
}
