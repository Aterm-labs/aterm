//! The whole cycle over a directory repository: publish → list → fetch →
//! hydrate → unpublish, with the bytes compared at the end.
//!
//! This is the test that would catch a layout or gzip change breaking
//! compatibility between backends, since every backend shares this path.

use std::path::Path;

use agent_sessions_remote::directory::DirectoryRemote;
use agent_sessions_remote::manifest::{timestamp_from_unix, RemoteManifest, FORMAT, VERSION};
use agent_sessions_remote::payload::{self, Layouts};
use agent_sessions_remote::{store, LocalState};

const TRANSCRIPT: &[u8] =
    b"{\"type\":\"user\",\"cwd\":\"/home/ana/WS/repo\"}\n{\"type\":\"assistant\"}\n";
const SUBAGENT: &[u8] = b"{\"agent\":1}\n";
const TOOL_RESULT: &[u8] = b"salida muy larga de una herramienta\n";

/// A Claude project as it looks on disk, including the two things that must
/// never travel.
fn claude_session(home: &Path, cwd: &str, id: &str) -> std::path::PathBuf {
    let project = home
        .join(".claude/projects")
        .join(agent_sessions::encode_cwd(cwd));
    std::fs::create_dir_all(project.join(id).join("subagents")).unwrap();
    std::fs::create_dir_all(project.join(id).join("tool-results")).unwrap();
    std::fs::create_dir_all(project.join("memory")).unwrap();
    std::fs::create_dir_all(home.join(".claude/session-env")).unwrap();
    let jsonl = project.join(format!("{id}.jsonl"));
    std::fs::write(&jsonl, TRANSCRIPT).unwrap();
    std::fs::write(project.join(id).join("subagents/agent-1.jsonl"), SUBAGENT).unwrap();
    std::fs::write(project.join(id).join("subagents/agent-1.meta.json"), b"{}").unwrap();
    std::fs::write(project.join(id).join("tool-results/out.txt"), TOOL_RESULT).unwrap();
    std::fs::write(project.join("memory/notas.md"), b"mis notas personales").unwrap();
    std::fs::write(home.join(".claude/session-env").join(id), b"TOKEN=secreto").unwrap();
    jsonl
}

fn manifest(id: &str) -> RemoteManifest {
    RemoteManifest {
        format: FORMAT.to_string(),
        version: VERSION,
        id: id.to_string(),
        provider: "claude".to_string(),
        published_at: timestamp_from_unix(1_760_000_000),
        published_by: Some("ana@example.com".to_string()),
        cwd: Some("/home/ana/WS/repo".to_string()),
        branch: Some("main".to_string()),
        git_remote: Some("git.empresa.com/odoo-16/fl-v16".to_string()),
        git_head: Some("abc1234".to_string()),
        display_name: Some("la sesión del bug".to_string()),
        tags: vec!["equipo".to_string()],
        first_prompt: Some("¿por qué falla el pago?".to_string()),
        message_count: Some(12),
        size_bytes: 0,
        resumable: true,
        origin_filename: None,
        artifacts: Vec::new(),
        forked_from: None,
    }
}

#[test]
fn publish_list_fetch_hydrate_and_unpublish() {
    let ana = tempfile::tempdir().unwrap();
    let bea = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let remote = DirectoryRemote::new(repo.path());

    let jsonl = claude_session(ana.path(), "/home/ana/WS/repo", "sesion-1");
    let artifacts = payload::collect_from_file(&jsonl).unwrap();
    let published = store::publish(&remote, &manifest("sesion-1"), &artifacts).unwrap();

    // The manifest records what actually went up, and the payload excludes
    // the personal notes and the machine-local env file.
    let mut paths: Vec<&str> = published
        .artifacts
        .iter()
        .map(|a| a.path.as_str())
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "session.jsonl",
            "sub/subagents/agent-1.jsonl",
            "sub/subagents/agent-1.meta.json",
            "sub/tool-results/out.txt",
        ]
    );
    let every_file = remote_files(repo.path());
    assert!(
        !every_file
            .iter()
            .any(|p| p.contains("memory") || p.contains("session-env")),
        "publicado de más: {every_file:?}"
    );
    assert_eq!(
        published.size_bytes,
        (TRANSCRIPT.len() + SUBAGENT.len() + 2 + TOOL_RESULT.len()) as u64
    );

    // A colleague lists the repository and sees it.
    let listed = store::list(&remote).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], published);
    assert_eq!(listed[0].display_name.as_deref(), Some("la sesión del bug"));

    // …and hydrates it into *their* project, not the recorded one.
    let layouts = Layouts {
        claude_home: bea.path().join(".claude"),
        codex_home: bea.path().join(".codex"),
        gemini_home: bea.path().join(".gemini"),
        qwen_home: bea.path().join(".qwen"),
    };
    let outcome = payload::hydrate(
        &listed[0],
        "/home/bea/proyectos/repo",
        |artifact| store::fetch(&remote, &listed[0].id, artifact),
        &layouts,
    )
    .unwrap();
    assert!(!outcome.already_present);
    assert_eq!(std::fs::read(&outcome.path).unwrap(), TRANSCRIPT);
    let subdir = outcome.path.with_extension("");
    assert_eq!(
        std::fs::read(subdir.join("subagents/agent-1.jsonl")).unwrap(),
        SUBAGENT
    );
    assert_eq!(
        std::fs::read(subdir.join("tool-results/out.txt")).unwrap(),
        TOOL_RESULT
    );
    // The id survives untouched: it's what the CLI resumes by.
    assert!(outcome.path.ends_with("sesion-1.jsonl"));

    // Bea's copy now matches; Ana's, after she keeps working, is ahead.
    let local = std::fs::metadata(&outcome.path).unwrap().len();
    let main = published
        .artifacts
        .iter()
        .find(|a| a.path == "session.jsonl")
        .unwrap();
    assert_eq!(
        LocalState::compare(Some(local), main.bytes),
        LocalState::Current
    );
    assert_eq!(
        LocalState::compare(Some(main.bytes + 10), main.bytes),
        LocalState::Ahead
    );
    assert_eq!(LocalState::compare(None, main.bytes), LocalState::Absent);

    // Unpublishing empties the repository and leaves both local copies alone.
    store::unpublish(&remote, "sesion-1").unwrap();
    assert!(store::list(&remote).unwrap().is_empty());
    assert!(remote_files(repo.path()).is_empty());
    assert!(jsonl.is_file());
    assert!(outcome.path.is_file());
}

#[test]
fn republishing_after_more_turns_reports_the_local_copy_as_stale() {
    let ana = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let remote = DirectoryRemote::new(repo.path());

    let jsonl = claude_session(ana.path(), "/w", "sesion-2");
    let first = store::publish(
        &remote,
        &manifest("sesion-2"),
        &payload::collect_from_file(&jsonl).unwrap(),
    )
    .unwrap();

    // Ana continues the conversation and publishes again: today that
    // overwrites rather than forking, which is the known v1 limitation.
    let mut grown = TRANSCRIPT.to_vec();
    grown.extend_from_slice(b"{\"type\":\"user\"}\n");
    std::fs::write(&jsonl, &grown).unwrap();
    let second = store::publish(
        &remote,
        &manifest("sesion-2"),
        &payload::collect_from_file(&jsonl).unwrap(),
    )
    .unwrap();

    assert_eq!(store::list(&remote).unwrap().len(), 1, "no debe duplicar");
    assert!(second.size_bytes > first.size_bytes);
    let published_main = second
        .artifacts
        .iter()
        .find(|a| a.path == "session.jsonl")
        .unwrap();
    // Somebody holding the previous version sees it as stale.
    assert_eq!(
        LocalState::compare(Some(TRANSCRIPT.len() as u64), published_main.bytes),
        LocalState::Stale
    );
    assert_eq!(
        store::fetch(&remote, "sesion-2", published_main).unwrap(),
        grown
    );
}

#[test]
fn a_corrupt_manifest_hides_only_itself() {
    let repo = tempfile::tempdir().unwrap();
    let remote = DirectoryRemote::new(repo.path());
    let ana = tempfile::tempdir().unwrap();
    let jsonl = claude_session(ana.path(), "/w", "buena");
    store::publish(
        &remote,
        &manifest("buena"),
        &payload::collect_from_file(&jsonl).unwrap(),
    )
    .unwrap();
    std::fs::write(repo.path().join("manifest/rota.json"), "no soy json").unwrap();
    std::fs::write(
        repo.path().join("manifest/futura.json"),
        r#"{"format":"aterm/remote-session","version":99}"#,
    )
    .unwrap();

    let listed = store::list(&remote).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "buena");
    // Asking for the broken one by name does report the problem.
    let err = store::read_manifest(&remote, "futura").unwrap_err();
    assert!(err.contains("no soportada"), "{err}");
}

#[test]
fn a_session_with_no_readable_file_travels_as_a_transcript_and_refuses_to_hydrate() {
    let repo = tempfile::tempdir().unwrap();
    let bea = tempfile::tempdir().unwrap();
    let remote = DirectoryRemote::new(repo.path());

    let turns = vec![
        agent_sessions::types::PreviewTurn {
            role: "user".to_string(),
            text: "¿qué hace este módulo?".to_string(),
        },
        agent_sessions::types::PreviewTurn {
            role: "assistant".to_string(),
            text: "calcula el IVA".to_string(),
        },
    ];
    let mut m = manifest("goose-1");
    m.provider = "goose".to_string();
    m.resumable = payload::provider_is_resumable("goose");
    let published = store::publish(&remote, &m, &payload::collect_transcript(&turns)).unwrap();
    assert!(!published.resumable);

    // Readable by anyone: that's the point of publishing it at all.
    let artifact = &published.artifacts[0];
    let body = store::fetch(&remote, "goose-1", artifact).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed[1]["text"], "calcula el IVA");

    // But hydration says so up front instead of writing something the CLI
    // would ignore.
    let layouts = Layouts {
        claude_home: bea.path().join(".claude"),
        codex_home: bea.path().join(".codex"),
        gemini_home: bea.path().join(".gemini"),
        qwen_home: bea.path().join(".qwen"),
    };
    let err = payload::hydrate(&published, "/w", |_| Ok(Vec::new()), &layouts).unwrap_err();
    assert!(err.contains("goose"), "{err}");
    assert!(!bea.path().join(".claude").exists());
}

#[test]
fn an_interrupted_publication_is_invisible_rather_than_broken() {
    let repo = tempfile::tempdir().unwrap();
    let remote = DirectoryRemote::new(repo.path());
    // Blobs uploaded, manifest never written: exactly what a crash mid-upload
    // leaves behind, since the manifest goes last.
    std::fs::create_dir_all(repo.path().join("blobs/a-medias")).unwrap();
    std::fs::write(
        repo.path().join("blobs/a-medias/session.jsonl.gz"),
        store::gzip(b"contenido").unwrap(),
    )
    .unwrap();
    assert!(store::list(&remote).unwrap().is_empty());
}

/// Every file in the repository, for the assertions about what travelled.
fn remote_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.path().is_dir() {
                walk(&entry.path(), &rel, out);
            } else {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out.sort();
    out
}

/// Reading a session published by the other UI must work: both write the same
/// bytes, so a manifest written by hand (or by the native app) parses here.
#[test]
fn a_manifest_written_by_hand_is_readable() {
    let repo = tempfile::tempdir().unwrap();
    let remote = DirectoryRemote::new(repo.path());
    std::fs::create_dir_all(repo.path().join("manifest")).unwrap();
    std::fs::write(
        repo.path().join("manifest/hecha-a-mano.json"),
        r#"{
          "format": "aterm/remote-session",
          "version": 1,
          "id": "hecha-a-mano",
          "provider": "codex",
          "published_at": "2026-07-01T09:00:00+00:00",
          "published_by": "otro@example.com",
          "cwd": "/srv/proyecto",
          "branch": null,
          "git_remote": null,
          "git_head": null,
          "display_name": null,
          "tags": [],
          "first_prompt": null,
          "message_count": null,
          "size_bytes": 10,
          "resumable": true,
          "origin_filename": "rollout-2026-07-01T09-00-00-hecha-a-mano.jsonl",
          "artifacts": [{"path": "session.jsonl", "bytes": 10, "gzip": true}],
          "forked_from": null
        }"#,
    )
    .unwrap();
    let listed = store::list(&remote).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].provider, "codex");

    // And it hydrates into codex's dated layout, derived from the filename
    // that travelled in the manifest.
    let bea = tempfile::tempdir().unwrap();
    let layouts = Layouts {
        claude_home: bea.path().join(".claude"),
        codex_home: bea.path().join(".codex"),
        gemini_home: bea.path().join(".gemini"),
        qwen_home: bea.path().join(".qwen"),
    };
    let outcome = payload::hydrate(&listed[0], "/w", |_| Ok(b"{}\n".to_vec()), &layouts).unwrap();
    assert_eq!(
        outcome.path,
        bea.path()
            .join(".codex/sessions/2026/07/01/rollout-2026-07-01T09-00-00-hecha-a-mano.jsonl")
    );
}
