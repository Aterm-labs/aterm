//! What travels, and where it lands.
//!
//! A session is not one file. For Claude it's the transcript plus a sibling
//! directory holding subagent logs and the tool outputs too big to inline —
//! without those the conversation has holes. What deliberately does *not*
//! travel is the project's `memory/` (personal notes) and `session-env`
//! (machine-local, may hold secrets; the CLI recreates it).
//!
//! Hydration is the mirror image, and it's the multi-provider part: each agent
//! keeps its sessions in its own shape, so the manifest's `provider` decides
//! which path we write back into. Two providers can be published and read but
//! not hydrated — goose keeps everything in SQLite and opencode only answers
//! through its CLI — and they say so in the manifest (`resumable: false`)
//! instead of failing at the last step.

use std::path::{Component, Path, PathBuf};

use agent_sessions::encode_cwd;

use crate::manifest::{Artifact, RemoteManifest};

/// Name of the main transcript inside a published session, whatever the
/// provider called it on disk. The original name survives in the manifest's
/// `origin_filename` when it carries meaning (codex encodes the timestamp).
pub const MAIN: &str = "session.jsonl";
/// Prefix for everything under the session's sibling directory.
pub const SUB: &str = "sub/";
/// Rendered turns, for providers with no readable per-session file.
pub const TRANSCRIPT: &str = "transcript.json";

/// Directory entries never worth publishing, checked by name at every level.
const NEVER_PUBLISH: [&str; 2] = ["memory", "session-env"];

#[derive(Debug)]
pub enum Source {
    File(PathBuf),
    Bytes(Vec<u8>),
}

/// An artefact resolved but not yet read: publishing streams these so a
/// multi-megabyte session never sits in memory twice.
#[derive(Debug)]
pub struct PendingArtifact {
    pub path: String,
    pub source: Source,
    pub gzip: bool,
}

impl PendingArtifact {
    pub fn read(&self) -> Result<Vec<u8>, String> {
        match &self.source {
            Source::File(p) => std::fs::read(p).map_err(|e| format!("{}: {e}", p.display())),
            Source::Bytes(b) => Ok(b.clone()),
        }
    }
}

/// Everything that makes up `main` on disk: the transcript itself plus its
/// sibling directory (`<id>/`) when the provider keeps one.
///
/// Symlinks are skipped rather than followed: a link inside the session
/// directory would otherwise publish whatever it points at, which is exactly
/// the kind of accident this feature must not make easy.
pub fn collect_from_file(main: &Path) -> Result<Vec<PendingArtifact>, String> {
    if !main.is_file() {
        return Err(format!(
            "no encuentro el fichero de la sesión: {}",
            main.display()
        ));
    }
    let mut out = vec![PendingArtifact {
        path: MAIN.to_string(),
        source: Source::File(main.to_path_buf()),
        gzip: true,
    }];
    let subdir = main.with_extension("");
    if subdir.is_dir() {
        collect_dir(&subdir, SUB.trim_end_matches('/'), &mut out)?;
    }
    Ok(out)
}

fn collect_dir(dir: &Path, prefix: &str, out: &mut Vec<PendingArtifact>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if NEVER_PUBLISH.contains(&name.as_str()) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let rel = format!("{prefix}/{name}");
        if file_type.is_dir() {
            collect_dir(&path, &rel, out)?;
        } else {
            // `.meta.json` sidecars are a few hundred bytes: gzipping them
            // costs a round trip and saves nothing.
            let gzip = !name.ends_with(".meta.json");
            out.push(PendingArtifact {
                path: rel,
                source: Source::File(path),
                gzip,
            });
        }
    }
    Ok(())
}

/// Fallback payload for providers with no per-session file: the conversation
/// as rendered turns. Readable and searchable, but not resumable — hydration
/// refuses it rather than writing something the CLI would ignore.
pub fn collect_transcript(turns: &[agent_sessions::types::PreviewTurn]) -> Vec<PendingArtifact> {
    let body = serde_json::to_vec_pretty(
        &turns
            .iter()
            .map(|t| serde_json::json!({"role": t.role, "text": t.text}))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| b"[]".to_vec());
    vec![PendingArtifact {
        path: TRANSCRIPT.to_string(),
        source: Source::Bytes(body),
        gzip: true,
    }]
}

/// True when a session of this provider can be written back into the local
/// layout and resumed by its CLI.
pub fn provider_is_resumable(provider: &str) -> bool {
    matches!(provider, "claude" | "codex" | "gemini" | "qwen")
}

/// Provider data directories, overridable so tests never touch a real `$HOME`.
#[derive(Clone, Debug)]
pub struct Layouts {
    pub claude_home: PathBuf,
    pub codex_home: PathBuf,
    pub gemini_home: PathBuf,
    pub qwen_home: PathBuf,
}

impl Default for Layouts {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            claude_home: home.join(".claude"),
            codex_home: home.join(".codex"),
            gemini_home: home.join(".gemini"),
            qwen_home: home.join(".qwen"),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct HydrateOutcome {
    /// Absolute path of the transcript now on disk.
    pub path: PathBuf,
    /// True when the file was already there and we left it untouched.
    pub already_present: bool,
}

/// Write a published session into the local layout of its provider, under
/// `dest_cwd` (the project the user picked — never inferred from the embedded
/// cwd, which belongs to whoever recorded it and may not exist here).
///
/// `read` hands back the *uncompressed* bytes of an artefact path. An existing
/// session is never overwritten: same id means same session, and the local
/// copy may already have turns the published one doesn't.
pub fn hydrate(
    manifest: &RemoteManifest,
    dest_cwd: &str,
    read: impl Fn(&Artifact) -> Result<Vec<u8>, String>,
    layouts: &Layouts,
) -> Result<HydrateOutcome, String> {
    if !manifest.resumable {
        return Err(format!(
            "las sesiones de {} no se pueden traer a disco: {} no guarda un fichero por sesión que su CLI sepa reanudar",
            manifest.provider, manifest.provider
        ));
    }
    let main = manifest
        .artifacts
        .iter()
        .find(|a| a.path == MAIN)
        .ok_or_else(|| "la sesión publicada no incluye el transcript".to_string())?;

    let (main_path, subdir) = destination(manifest, dest_cwd, layouts)?;
    if main_path.exists() {
        return Ok(HydrateOutcome {
            path: main_path,
            already_present: true,
        });
    }
    let parent = main_path
        .parent()
        .ok_or_else(|| "destino inválido".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    std::fs::write(&main_path, read(main)?).map_err(|e| format!("{}: {e}", main_path.display()))?;

    for artifact in manifest
        .artifacts
        .iter()
        .filter(|a| a.path.starts_with(SUB))
    {
        let Some(subdir) = subdir.as_ref() else {
            continue; // Provider without a session directory: nothing to place.
        };
        let rel = &artifact.path[SUB.len()..];
        let target = safe_join(subdir, rel)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&target, read(artifact)?)
            .map_err(|e| format!("{}: {e}", target.display()))?;
    }
    Ok(HydrateOutcome {
        path: main_path,
        already_present: false,
    })
}

/// Where this provider expects the transcript (and its sibling directory, when
/// it has one) for a session recorded under `dest_cwd`.
fn destination(
    manifest: &RemoteManifest,
    dest_cwd: &str,
    layouts: &Layouts,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    let id = &manifest.id;
    match manifest.provider.as_str() {
        "claude" => {
            let dir = layouts
                .claude_home
                .join("projects")
                .join(encode_cwd(dest_cwd));
            Ok((dir.join(format!("{id}.jsonl")), Some(dir.join(id))))
        }
        "qwen" => {
            let dir = layouts
                .qwen_home
                .join("projects")
                .join(encode_cwd(dest_cwd))
                .join("chats");
            Ok((dir.join(format!("{id}.jsonl")), None))
        }
        "codex" => {
            // The rollout filename carries the recording date, and codex looks
            // for its sessions under that dated directory — so the name is
            // reconstructed, not invented.
            let name = manifest
                .origin_filename
                .clone()
                .unwrap_or_else(|| format!("rollout-{}-{id}.jsonl", date_prefix(manifest)));
            let (y, m, d) = rollout_date(&name).ok_or_else(|| {
                "no puedo deducir la fecha del rollout de codex a partir del manifest".to_string()
            })?;
            let dir = layouts.codex_home.join("sessions").join(y).join(m).join(d);
            Ok((dir.join(name), None))
        }
        "gemini" => {
            // Gemini addresses projects by a short id it mints itself and
            // records in projects.json. We can't reproduce that id, so we can
            // only place a session in a project gemini already knows.
            let registry = layouts.gemini_home.join("projects.json");
            let short = gemini_project_id(&registry, dest_cwd).ok_or_else(|| {
                format!(
                    "Gemini todavía no conoce {dest_cwd}: abre `gemini` una vez en ese directorio y vuelve a traerla"
                )
            })?;
            let dir = layouts.gemini_home.join("tmp").join(short).join("chats");
            let name = manifest
                .origin_filename
                .clone()
                .unwrap_or_else(|| format!("session-{id}.jsonl"));
            Ok((dir.join(name), None))
        }
        other => Err(format!(
            "proveedor no soportado para traer a disco: {other}"
        )),
    }
}

/// `2026-06-03T10-00-00` from the manifest's publish timestamp, as codex names
/// its rollouts. Only used when the original filename didn't travel.
fn date_prefix(manifest: &RemoteManifest) -> String {
    manifest
        .published_at
        .split('+')
        .next()
        .unwrap_or_default()
        .replacen(':', "-", 2)
        .replace(':', "-")
}

/// Pull `(YYYY, MM, DD)` out of `rollout-2026-06-03T10-00-00-<id>.jsonl`.
fn rollout_date(name: &str) -> Option<(String, String, String)> {
    let rest = name.strip_prefix("rollout-")?;
    let date = rest.split('T').next()?;
    let mut parts = date.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    Some((y.to_string(), m.to_string(), d.to_string()))
}

/// Gemini's `projects.json` maps real path → short id; we need the reverse for
/// exactly one path.
fn gemini_project_id(registry: &Path, cwd: &str) -> Option<String> {
    let raw = std::fs::read_to_string(registry).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    parsed
        .get("projects")?
        .as_object()?
        .iter()
        .find(|(path, _)| path.as_str() == cwd)
        .and_then(|(_, id)| id.as_str())
        .map(str::to_string)
}

/// Join a repository-relative path onto a directory, refusing anything that
/// could climb out of it. The remote is written by other people; its paths are
/// input, not data we produced.
fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("ruta sospechosa en la sesión publicada: {rel}"));
    }
    if rel_path.components().any(|c| {
        matches!(c, Component::Normal(name) if NEVER_PUBLISH.contains(&name.to_string_lossy().as_ref()))
    }) {
        return Err(format!("ruta no permitida en la sesión publicada: {rel}"));
    }
    Ok(base.join(rel_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{timestamp_from_unix, FORMAT, VERSION};
    use std::collections::HashMap;

    fn manifest(provider: &str, id: &str, artifacts: Vec<Artifact>) -> RemoteManifest {
        RemoteManifest {
            format: FORMAT.to_string(),
            version: VERSION,
            id: id.to_string(),
            provider: provider.to_string(),
            published_at: timestamp_from_unix(1_760_000_000),
            published_by: None,
            cwd: Some("/home/ana/WS/repo".to_string()),
            branch: None,
            git_remote: None,
            git_head: None,
            display_name: None,
            tags: Vec::new(),
            first_prompt: None,
            message_count: None,
            size_bytes: 0,
            resumable: provider_is_resumable(provider),
            origin_filename: None,
            artifacts,
            forked_from: None,
        }
    }

    fn art(path: &str) -> Artifact {
        Artifact {
            path: path.to_string(),
            bytes: 3,
            gzip: true,
        }
    }

    fn layouts(root: &Path) -> Layouts {
        Layouts {
            claude_home: root.join(".claude"),
            codex_home: root.join(".codex"),
            gemini_home: root.join(".gemini"),
            qwen_home: root.join(".qwen"),
        }
    }

    fn contents(map: HashMap<String, Vec<u8>>) -> impl Fn(&Artifact) -> Result<Vec<u8>, String> {
        move |a: &Artifact| {
            map.get(&a.path)
                .cloned()
                .ok_or_else(|| format!("falta {}", a.path))
        }
    }

    #[test]
    fn collects_the_transcript_and_its_directory_but_not_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(project.join("sid/subagents")).unwrap();
        std::fs::create_dir_all(project.join("sid/memory")).unwrap();
        std::fs::write(project.join("sid.jsonl"), b"{}\n").unwrap();
        std::fs::write(project.join("sid/subagents/agent-1.jsonl"), b"{}\n").unwrap();
        std::fs::write(project.join("sid/subagents/agent-1.meta.json"), b"{}").unwrap();
        std::fs::write(project.join("sid/memory/secreto.md"), b"nope").unwrap();

        let arts = collect_from_file(&project.join("sid.jsonl")).unwrap();
        let mut paths: Vec<&str> = arts.iter().map(|a| a.path.as_str()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "session.jsonl",
                "sub/subagents/agent-1.jsonl",
                "sub/subagents/agent-1.meta.json",
            ]
        );
        // Small sidecars travel uncompressed.
        let meta = arts
            .iter()
            .find(|a| a.path.ends_with(".meta.json"))
            .unwrap();
        assert!(!meta.gzip);
    }

    #[test]
    fn collect_reports_a_missing_session_instead_of_publishing_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let err = collect_from_file(&tmp.path().join("ausente.jsonl")).unwrap_err();
        assert!(err.contains("no encuentro"), "{err}");
    }

    #[test]
    fn hydrates_claude_into_the_destination_project_not_the_recorded_one() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest(
            "claude",
            "sid-1",
            vec![art(MAIN), art("sub/subagents/a.jsonl")],
        );
        let body = HashMap::from([
            (MAIN.to_string(), b"{\"a\":1}\n".to_vec()),
            ("sub/subagents/a.jsonl".to_string(), b"{}\n".to_vec()),
        ]);
        let out = hydrate(&m, "/home/bea/otro", contents(body), &layouts(tmp.path())).unwrap();
        let expected = tmp
            .path()
            .join(".claude/projects")
            .join(encode_cwd("/home/bea/otro"))
            .join("sid-1.jsonl");
        assert_eq!(out.path, expected);
        assert!(!out.already_present);
        assert_eq!(std::fs::read(&expected).unwrap(), b"{\"a\":1}\n");
        assert!(expected
            .with_extension("")
            .join("subagents/a.jsonl")
            .is_file());
    }

    #[test]
    fn hydrating_twice_keeps_the_local_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest("claude", "sid-2", vec![art(MAIN)]);
        let body = HashMap::from([(MAIN.to_string(), b"nuevo\n".to_vec())]);
        let l = layouts(tmp.path());
        hydrate(&m, "/w", contents(body.clone()), &l).unwrap();
        let path = tmp
            .path()
            .join(".claude/projects")
            .join(encode_cwd("/w"))
            .join("sid-2.jsonl");
        std::fs::write(&path, b"mi version con mas turnos\n").unwrap();

        let out = hydrate(&m, "/w", contents(body), &l).unwrap();
        assert!(out.already_present);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"mi version con mas turnos\n"
        );
    }

    #[test]
    fn codex_lands_under_the_dated_directory_of_its_rollout() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = manifest("codex", "0199-pppp", vec![art(MAIN)]);
        m.origin_filename = Some("rollout-2026-06-03T10-00-00-0199-pppp.jsonl".to_string());
        let body = HashMap::from([(MAIN.to_string(), b"{}\n".to_vec())]);
        let out = hydrate(&m, "/w", contents(body), &layouts(tmp.path())).unwrap();
        assert_eq!(
            out.path,
            tmp.path()
                .join(".codex/sessions/2026/06/03/rollout-2026-06-03T10-00-00-0199-pppp.jsonl")
        );
    }

    #[test]
    fn gemini_refuses_a_project_it_does_not_know_yet() {
        let tmp = tempfile::tempdir().unwrap();
        let l = layouts(tmp.path());
        std::fs::create_dir_all(&l.gemini_home).unwrap();
        std::fs::write(
            l.gemini_home.join("projects.json"),
            r#"{"projects":{"/home/bea/conocido":"proj-a"}}"#,
        )
        .unwrap();
        let m = manifest("gemini", "g-1", vec![art(MAIN)]);
        let body = HashMap::from([(MAIN.to_string(), b"{}\n".to_vec())]);

        let err = hydrate(&m, "/home/bea/nuevo", contents(body.clone()), &l).unwrap_err();
        assert!(err.contains("gemini"), "{err}");

        let out = hydrate(&m, "/home/bea/conocido", contents(body), &l).unwrap();
        assert_eq!(
            out.path,
            l.gemini_home.join("tmp/proj-a/chats/session-g-1.jsonl")
        );
    }

    #[test]
    fn qwen_lands_in_its_encoded_project_chats_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest("qwen", "q-1", vec![art(MAIN)]);
        let body = HashMap::from([(MAIN.to_string(), b"{}\n".to_vec())]);
        let out = hydrate(&m, "/work/proj", contents(body), &layouts(tmp.path())).unwrap();
        assert_eq!(
            out.path,
            tmp.path()
                .join(".qwen/projects")
                .join(encode_cwd("/work/proj"))
                .join("chats/q-1.jsonl")
        );
    }

    #[test]
    fn non_resumable_providers_say_so_before_touching_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest("goose", "g", vec![art(TRANSCRIPT)]);
        assert!(!m.resumable);
        let err = hydrate(&m, "/w", contents(HashMap::new()), &layouts(tmp.path())).unwrap_err();
        assert!(err.contains("goose"), "{err}");
    }

    #[test]
    fn a_traversing_artifact_path_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest("claude", "sid-3", vec![art(MAIN), art("sub/../../fuera")]);
        let body = HashMap::from([
            (MAIN.to_string(), b"{}\n".to_vec()),
            ("sub/../../fuera".to_string(), b"malo".to_vec()),
        ]);
        let err = hydrate(&m, "/w", contents(body), &layouts(tmp.path())).unwrap_err();
        assert!(err.contains("sospechosa"), "{err}");
        assert!(!tmp.path().join("fuera").exists());
    }
}
