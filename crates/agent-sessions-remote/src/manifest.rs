//! The one document that makes a published session visible.
//!
//! Written last when publishing and deleted first when unpublishing, so an
//! interrupted transfer leaves unreferenced blobs (invisible, harmless) rather
//! than an entry pointing at payload that isn't there.
//!
//! Keys are snake_case: the format is meant to be read by hand (`jq` over a
//! shared folder is a supported way to inspect a repository) and to stay
//! byte-identical whichever backend wrote it.

use serde::{Deserialize, Serialize};

pub const FORMAT: &str = "aterm/remote-session";
pub const VERSION: u64 = 1;

/// One file inside a published session. Listed in the manifest rather than
/// discovered by listing the remote directory, because the GitHub contents API
/// has no recursive listing and we'd otherwise walk it a request at a time.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Artifact {
    /// Path relative to the session's blob root, always `/`-separated.
    pub path: String,
    /// Size of the *uncompressed* content, for progress and the local-copy
    /// comparison.
    pub bytes: u64,
    /// Whether the stored blob is gzipped (`<path>.gz`). Text is; the small
    /// `.meta.json` sidecars aren't worth the round trip.
    pub gzip: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteManifest {
    pub format: String,
    pub version: u64,
    /// The provider's own session id, preserved verbatim: it's what the CLI
    /// accepts in its resume command, and uuids don't collide between people.
    pub id: String,
    /// Which agent recorded it ("claude", "codex", …). Unlike the single-agent
    /// original this port is multi-provider, so hydration needs to know which
    /// on-disk layout to write back into.
    pub provider: String,
    pub published_at: String,
    pub published_by: Option<String>,
    /// Working directory where it was recorded. Historic: it belongs to
    /// whoever published, may not exist here, and is never rewritten.
    pub cwd: Option<String>,
    pub branch: Option<String>,
    /// Normalised origin of the *code* repository, with the commit the session
    /// was recorded against. Together they drive the divergence warning.
    pub git_remote: Option<String>,
    pub git_head: Option<String>,
    pub display_name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub first_prompt: Option<String>,
    pub message_count: Option<u32>,
    /// Total uncompressed bytes across every artefact. The local-copy state
    /// (`current` / `stale` / `ahead`) compares against this.
    pub size_bytes: u64,
    /// False when the provider keeps no per-session file we can write back
    /// (goose lives in SQLite, opencode only answers through its CLI): the
    /// session is publishable and readable, but not resumable from here.
    pub resumable: bool,
    /// Original filename of the main transcript, when the provider derives
    /// meaning from it — codex encodes the recording timestamp in the rollout
    /// name and gemini names its chats by date.
    pub origin_filename: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// Reserved for the explicit-fork flow: today republishing overwrites.
    pub forked_from: Option<String>,
}

impl RemoteManifest {
    /// Parse and validate, mapping every failure to a sentence a user can act
    /// on. A repository is a shared place: a manifest written by a newer
    /// version, or by something else entirely, must not surface as a panic or
    /// a raw serde error.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| "el manifest no es JSON válido".to_string())?;
        match value.get("format").and_then(serde_json::Value::as_str) {
            Some(FORMAT) => {}
            Some(other) => return Err(format!("esto no es una sesión publicada: {other}")),
            None => return Err("el manifest no declara formato".to_string()),
        }
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(VERSION) => {}
            Some(other) => {
                return Err(format!(
                    "versión de manifest no soportada: {other} (esta versión entiende la {VERSION})"
                ))
            }
            None => return Err("el manifest no declara versión".to_string()),
        }
        serde_json::from_value(value).map_err(|e| format!("manifest incompleto: {e}"))
    }

    pub fn to_json(&self) -> String {
        // Pretty on purpose: these files get read in a browser's repo view.
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// ISO-8601 UTC to the second, without pulling in chrono — same
/// days-to-civil conversion the core's `transfer.rs` uses.
pub fn timestamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    timestamp_from_unix(secs)
}

pub fn timestamp_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}+00:00")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RemoteManifest {
        RemoteManifest {
            format: FORMAT.to_string(),
            version: VERSION,
            id: "abc".to_string(),
            provider: "claude".to_string(),
            published_at: timestamp_from_unix(1_760_000_000),
            published_by: Some("ana@example.com".to_string()),
            cwd: Some("/home/ana/WS/repo".to_string()),
            branch: Some("main".to_string()),
            git_remote: Some("github.com/o/r".to_string()),
            git_head: Some("abc1234".to_string()),
            display_name: None,
            tags: vec!["equipo".to_string()],
            first_prompt: Some("hola".to_string()),
            message_count: Some(12),
            size_bytes: 400,
            resumable: true,
            origin_filename: None,
            artifacts: vec![Artifact {
                path: "session.jsonl".to_string(),
                bytes: 400,
                gzip: true,
            }],
            forked_from: None,
        }
    }

    #[test]
    fn round_trips_through_json() {
        let m = sample();
        assert_eq!(RemoteManifest::parse(&m.to_json()).unwrap(), m);
    }

    #[test]
    fn rejects_foreign_and_future_documents_with_a_sentence() {
        let err = RemoteManifest::parse("{}").unwrap_err();
        assert!(err.contains("formato"), "{err}");
        let err = RemoteManifest::parse(r#"{"format":"otra-cosa"}"#).unwrap_err();
        assert!(err.contains("no es una sesión publicada"), "{err}");
        let err =
            RemoteManifest::parse(&format!(r#"{{"format":"{FORMAT}","version":99}}"#)).unwrap_err();
        assert!(err.contains("no soportada"), "{err}");
        // Right format and version but missing required fields: still a
        // sentence, never a traceback.
        let err =
            RemoteManifest::parse(&format!(r#"{{"format":"{FORMAT}","version":1}}"#)).unwrap_err();
        assert!(err.contains("incompleto"), "{err}");
    }

    #[test]
    fn not_valid_json_is_reported_as_such() {
        assert!(RemoteManifest::parse("no soy json")
            .unwrap_err()
            .contains("JSON"));
    }

    #[test]
    fn timestamp_is_iso8601_utc() {
        assert_eq!(timestamp_from_unix(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(
            timestamp_from_unix(1_760_000_000),
            "2025-10-09T08:53:20+00:00"
        );
    }
}
