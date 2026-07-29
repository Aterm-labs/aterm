//! The repository layout, and the four operations a backend has to provide.
//!
//! Layout, gzip and ordering live *here*, not in the backends, so a session
//! published to a shared folder and the same session published to GitLab are
//! byte-identical and a repository can be read by either driver:
//!
//! ```text
//! manifest/<id>.json
//! blobs/<id>/session.jsonl.gz
//! blobs/<id>/sub/subagents/agent-1.jsonl.gz
//! blobs/<id>/sub/subagents/agent-1.meta.json
//! ```
//!
//! Two orderings carry the only atomicity guarantee there is. Publishing
//! writes blobs first and the manifest last; unpublishing deletes the manifest
//! first and the blobs after. Since the manifest is what makes a session
//! visible, an interrupted transfer in either direction leaves unreferenced
//! blobs — invisible and harmless — never an entry pointing at payload that
//! isn't there.

use std::io::{Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::manifest::{Artifact, RemoteManifest};
use crate::payload::PendingArtifact;

pub const MANIFEST_DIR: &str = "manifest";
pub const BLOBS_DIR: &str = "blobs";

/// A place that can hold files: a directory, a git checkout, a repository
/// reachable through an API. Everything above this trait is shared.
///
/// `begin`/`commit` bracket a publish or unpublish so backends that can group
/// writes (git: one commit) do, and the ones that can't ignore them.
pub trait RepoBackend: Send + Sync {
    /// Human-readable destination, for messages and error text.
    fn describe(&self) -> String;
    /// Every file path under `prefix`, recursively, repository-relative.
    /// A missing prefix is an empty list, not an error: a repository with
    /// nothing published yet is a normal state.
    fn list_files(&self, prefix: &str) -> Result<Vec<String>, String>;
    fn read_file(&self, path: &str) -> Result<Vec<u8>, String>;
    fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), String>;
    /// Deleting an absent file is success: unpublish has to be idempotent, or
    /// a half-finished one can never be finished.
    fn delete_file(&self, path: &str) -> Result<(), String>;
    fn begin(&self) -> Result<(), String> {
        Ok(())
    }
    fn commit(&self, _message: &str) -> Result<(), String> {
        Ok(())
    }
}

pub fn manifest_path(id: &str) -> String {
    format!("{MANIFEST_DIR}/{id}.json")
}

pub fn blob_path(id: &str, artifact_path: &str, gzip: bool) -> String {
    let suffix = if gzip { ".gz" } else { "" };
    format!("{BLOBS_DIR}/{id}/{artifact_path}{suffix}")
}

/// Publish one session: blobs first, manifest last.
///
/// The manifest is rewritten with the artefact list actually uploaded, so
/// `fetch` never has to list the remote — which matters for the GitHub driver,
/// whose contents API has no recursive listing.
pub fn publish(
    backend: &dyn RepoBackend,
    manifest: &RemoteManifest,
    artifacts: &[PendingArtifact],
) -> Result<RemoteManifest, String> {
    backend.begin()?;
    let mut written = Vec::with_capacity(artifacts.len());
    let mut total = 0u64;
    for artifact in artifacts {
        let raw = artifact.read()?;
        let bytes = raw.len() as u64;
        total += bytes;
        let body = if artifact.gzip { gzip(&raw)? } else { raw };
        backend.write_file(
            &blob_path(&manifest.id, &artifact.path, artifact.gzip),
            &body,
        )?;
        written.push(Artifact {
            path: artifact.path.clone(),
            bytes,
            gzip: artifact.gzip,
        });
    }
    let mut final_manifest = manifest.clone();
    final_manifest.artifacts = written;
    final_manifest.size_bytes = total;
    backend.write_file(
        &manifest_path(&manifest.id),
        final_manifest.to_json().as_bytes(),
    )?;
    backend.commit(&format!("publica la sesión {}", manifest.id))?;
    Ok(final_manifest)
}

/// Every session published in this repository, newest first.
///
/// A manifest that fails to parse is skipped rather than fatal: one corrupt or
/// newer-format file must not hide everything else in a shared repository.
pub fn list(backend: &dyn RepoBackend) -> Result<Vec<RemoteManifest>, String> {
    let mut out = Vec::new();
    for path in backend.list_files(MANIFEST_DIR)? {
        if !path.ends_with(".json") {
            continue;
        }
        let Ok(raw) = backend.read_file(&path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(raw) else {
            continue;
        };
        if let Ok(manifest) = RemoteManifest::parse(&text) {
            out.push(manifest);
        }
    }
    out.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    Ok(out)
}

/// Read one published session's manifest, with a real error when it's absent
/// (unlike `list`, the caller asked for this one specifically).
pub fn read_manifest(backend: &dyn RepoBackend, id: &str) -> Result<RemoteManifest, String> {
    let raw = backend.read_file(&manifest_path(id))?;
    let text = String::from_utf8(raw).map_err(|_| "el manifest no es texto".to_string())?;
    RemoteManifest::parse(&text)
}

/// Uncompressed bytes of one artefact.
pub fn fetch(backend: &dyn RepoBackend, id: &str, artifact: &Artifact) -> Result<Vec<u8>, String> {
    let raw = backend.read_file(&blob_path(id, &artifact.path, artifact.gzip))?;
    if artifact.gzip {
        gunzip(&raw)
    } else {
        Ok(raw)
    }
}

/// Remove a session from the repository: manifest first, blobs after.
///
/// Never touches the local copy — nobody expects "stop sharing this" to delete
/// their own transcript.
pub fn unpublish(backend: &dyn RepoBackend, id: &str) -> Result<(), String> {
    backend.begin()?;
    backend.delete_file(&manifest_path(id))?;
    let prefix = format!("{BLOBS_DIR}/{id}");
    for path in backend.list_files(&prefix)? {
        backend.delete_file(&path)?;
    }
    backend.commit(&format!("despublica la sesión {id}"))?;
    Ok(())
}

pub fn gzip(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(raw).map_err(|e| e.to_string())?;
    encoder.finish().map_err(|e| e.to_string())
}

pub fn gunzip(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    GzDecoder::new(raw)
        .read_to_end(&mut out)
        .map_err(|_| "el blob publicado no es gzip válido".to_string())?;
    Ok(out)
}
