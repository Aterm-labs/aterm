//! `remote-*` subcommands: shared sessions, exposed to whichever UI asks.
//!
//! The engine lives in `agent-sessions-remote`; this layer is argv → JSON, the
//! same contract as the rest of the sidecar. Both frontends (the VS Code
//! extension and the native app) drive the feature through these commands, so
//! a repository published from one is readable from the other without either
//! of them knowing how a manifest is laid out.
//!
//! Everything is keyed by a **project cwd**, because that is what decides both
//! which repositories apply (links are indexed by the project's git origin)
//! and where a hydrated session lands. The frontend always knows it; inferring
//! it here from the session's recorded cwd would put a colleague's session
//! into a directory that doesn't exist on this machine.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use agent_sessions::provider::AgentProvider;
use agent_sessions::types::AgentSession;
use agent_sessions::{all_providers, metadata::MetadataStore};
use agent_sessions_remote::links::{
    self, backend_for, project_key, RemoteLink, RemoteServer, RemotesConfig,
};
use agent_sessions_remote::manifest::{timestamp_now, RemoteManifest, FORMAT, VERSION};
use agent_sessions_remote::payload::{self, Layouts, PendingArtifact};
use agent_sessions_remote::{store, LocalState};
use serde_json::json;

use crate::{emit, fail, home_dir, metadata_path};

fn config_path() -> PathBuf {
    home_dir().join(".config/aterm/remotes.json")
}

fn token_dir() -> PathBuf {
    home_dir().join(".config/aterm/remote-tokens")
}

fn load_config() -> RemotesConfig {
    RemotesConfig::load(&config_path())
}

fn save_config(config: &RemotesConfig) {
    if let Err(e) = config.save(&config_path()) {
        fail(&format!(
            "no se pudo guardar la configuración de remotos: {e}"
        ));
    }
}

fn stdin_json() -> serde_json::Value {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        fail("no se pudo leer stdin");
    }
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(&raw).unwrap_or_else(|e| fail(&format!("JSON inválido en stdin: {e}")))
}

/// Servers (with a flag for "has a token", never the token itself) plus the
/// global fallback links.
pub fn config_cmd() {
    let config = load_config();
    let dir = token_dir();
    let servers: Vec<serde_json::Value> = config
        .servers
        .iter()
        .map(|s| {
            let mut value = serde_json::to_value(s).unwrap_or(json!({}));
            value["hasToken"] = json!(links::read_token(&dir, &s.name).is_some());
            value
        })
        .collect();
    emit(&json!({
        "servers": servers,
        "global": config.global,
        // Surfaced so the UI can explain why nothing it configured applies.
        "dirOverride": std::env::var(links::DIR_OVERRIDE_ENV).ok().filter(|d| !d.trim().is_empty()),
    }));
}

/// Add or update a server. The token, when present, goes to its own file with
/// owner-only permissions — never into the JSON everybody screenshots.
pub fn server_set_cmd() {
    let body = stdin_json();
    let token = body
        .get("token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let server: RemoteServer =
        serde_json::from_value(body).unwrap_or_else(|e| fail(&format!("servidor inválido: {e}")));
    if server.name.trim().is_empty() {
        fail("el servidor necesita un nombre");
    }
    let mut config = load_config();
    config.set_server(server.clone());
    save_config(&config);
    if let Some(token) = token {
        if let Err(e) = links::write_token(&token_dir(), &server.name, &token) {
            fail(&format!("no se pudo guardar el token: {e}"));
        }
    }
    emit(&json!({ "ok": true, "name": server.name }));
}

pub fn server_delete_cmd(name: Option<&String>) {
    let Some(name) = name else {
        fail("uso: remote-server-delete <nombre>");
    };
    let mut config = load_config();
    config.remove_server(name);
    save_config(&config);
    let _ = links::write_token(&token_dir(), name, "");
    emit(&json!({ "ok": true }));
}

/// The repositories that apply to a project, and where they came from — the
/// UI says "heredado del global" rather than pretending they're the project's.
pub fn links_cmd(cwd: Option<&String>) {
    let Some(cwd) = cwd else {
        fail("uso: remote-links <cwd>");
    };
    let config = load_config();
    let key = project_key(Path::new(cwd));
    let own = config.links.get(&key).cloned().unwrap_or_default();
    let source = if !own.is_empty() {
        "project"
    } else if !config.global.is_empty() {
        "global"
    } else {
        "none"
    };
    let links = resolved_links(&config, &key);
    let source = if own.is_empty() && !links.is_empty() && source == "none" {
        "override"
    } else {
        source
    };
    emit(&json!({
        "key": key,
        "links": links,
        "source": source,
    }));
}

pub fn links_set_cmd(cwd: Option<&String>) {
    let Some(cwd) = cwd else {
        fail("uso: remote-links-set <cwd>   (JSON [links] por stdin)");
    };
    let value = stdin_json();
    let list: Vec<RemoteLink> =
        serde_json::from_value(value).unwrap_or_else(|e| fail(&format!("enlaces inválidos: {e}")));
    let key = project_key(Path::new(cwd));
    let mut config = load_config();
    config.set_links(&key, list);
    save_config(&config);
    emit(&json!({ "ok": true, "key": key }));
}

pub fn global_set_cmd() {
    let value = stdin_json();
    let list: Vec<RemoteLink> =
        serde_json::from_value(value).unwrap_or_else(|e| fail(&format!("enlaces inválidos: {e}")));
    let mut config = load_config();
    config.global = list;
    save_config(&config);
    emit(&json!({ "ok": true }));
}

/// Check a server before anything depends on it. SSH servers answer `ssh -T`;
/// token servers are asked for the repository, which also confirms the name
/// was typed right.
pub fn probe_cmd(server: Option<&String>, repo: Option<&String>) {
    let Some(name) = server else {
        fail("uso: remote-probe <servidor> [grupo/repo]");
    };
    let config = load_config();
    let Some(server) = config.server(name) else {
        fail(&format!("no hay ningún servidor llamado «{name}»"));
    };
    let message = match (server.kind, server.auth) {
        (links::ServerKind::Directory, _) => {
            let Some(repo) = repo else {
                fail("uso: remote-probe <servidor> <ruta>");
            };
            let path = Path::new(repo);
            if path.is_dir() {
                format!("la carpeta {repo} existe y es accesible")
            } else {
                fail(&format!("{repo} no existe o no es una carpeta"))
            }
        }
        (_, links::AuthKind::Ssh) | (links::ServerKind::Git, _) => {
            agent_sessions_remote::git::probe_ssh(&server.ssh_host(), server.ssh_port)
                .unwrap_or_else(|e| fail(&e))
        }
        (kind, links::AuthKind::Token) => {
            let Some(repo) = repo else {
                fail("uso: remote-probe <servidor> <grupo/repo>");
            };
            let token = links::read_token(&token_dir(), &server.name)
                .unwrap_or_else(|| fail("ese servidor no tiene token guardado"));
            let remote = match kind {
                links::ServerKind::Github => {
                    agent_sessions_remote::http::HttpRepoRemote::new_github(
                        server.api_host(),
                        repo.clone(),
                        "main".to_string(),
                        token,
                    )
                }
                _ => agent_sessions_remote::http::HttpRepoRemote::new_gitlab(
                    server.api_host(),
                    repo.clone(),
                    "main".to_string(),
                    token,
                ),
            };
            agent_sessions_remote::http::probe_http(&remote).unwrap_or_else(|e| fail(&e))
        }
    };
    emit(&json!({ "ok": true, "message": message }));
}

/// Label of the tab the directory override implies.
const OVERRIDE_LABEL: &str = "carpeta";

fn dir_override() -> Option<String> {
    std::env::var(links::DIR_OVERRIDE_ENV)
        .ok()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

/// The repositories that apply to a project, including the one the directory
/// override implies — that override exists so the whole feature can be tried
/// with nothing configured, which only works if it produces a tab.
fn resolved_links(config: &RemotesConfig, key: &str) -> Vec<RemoteLink> {
    let links = config.resolve(key);
    if !links.is_empty() {
        return links;
    }
    dir_override()
        .map(|dir| {
            vec![RemoteLink {
                label: OVERRIDE_LABEL.to_string(),
                server: String::new(),
                repo: dir,
                branch: "main".to_string(),
            }]
        })
        .unwrap_or_default()
}

/// Resolve a repository by tab label for this project.
fn link_for(cwd: &str, label: &str) -> (RemotesConfig, RemoteLink) {
    let config = load_config();
    let key = project_key(Path::new(cwd));
    let links = resolved_links(&config, &key);
    // Under the override there is exactly one destination, so honour whatever
    // the caller called it rather than failing on a name mismatch.
    if let (true, Some(dir)) = (config.resolve(&key).is_empty(), dir_override()) {
        return (
            config,
            RemoteLink {
                label: label.to_string(),
                server: String::new(),
                repo: dir,
                branch: "main".to_string(),
            },
        );
    }
    let link = links
        .into_iter()
        .find(|l| l.label == label)
        .unwrap_or_else(|| {
            fail(&format!(
                "este proyecto no está enlazado a ningún repositorio llamado «{label}»"
            ))
        });
    (config, link)
}

fn backend(cwd: &str, label: &str) -> Box<dyn store::RepoBackend> {
    let (config, link) = link_for(cwd, label);
    backend_for(&link, &config, &token_dir()).unwrap_or_else(|e| fail(&e))
}

/// Everything published in a repository, each row carrying how the local copy
/// compares. The tab lists **all** of it, including your own sessions: seeing
/// yours there is the confirmation that publishing worked.
pub fn list_cmd(cwd: Option<&String>, label: Option<&String>) {
    let (Some(cwd), Some(label)) = (cwd, label) else {
        fail("uso: remote-list <cwd> <repositorio>");
    };
    let backend = backend(cwd, label);
    let manifests = store::list(backend.as_ref()).unwrap_or_else(|e| fail(&e));
    let providers = all_providers();
    let rows: Vec<serde_json::Value> = manifests
        .into_iter()
        .map(|m| {
            let local = providers
                .iter()
                .find(|p| p.id() == m.provider)
                .and_then(|p| p.locate(&m.id));
            let local_bytes = local
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|meta| meta.len());
            let published_bytes = m
                .artifacts
                .iter()
                .find(|a| a.path == payload::MAIN)
                .map(|a| a.bytes)
                .unwrap_or(m.size_bytes);
            let mut value = row(&m);
            value["localState"] = json!(LocalState::compare(local_bytes, published_bytes));
            value["localPath"] = json!(local.map(|p| p.display().to_string()));
            value["repo"] = json!(label);
            value
        })
        .collect();
    emit(&json!(rows));
}

/// Exactly which files a publication would upload.
///
/// Shown before confirming, and the only thing standing between a session that
/// printed a `.env` into a tool result and a repository the whole team reads:
/// there is no secret scanner, so the list has to be reviewable.
pub fn plan_cmd(provider: Option<&String>, id: Option<&String>) {
    let (session, provider) = find_session(provider, id);
    let (artifacts, resumable) = gather(provider.as_ref(), &session.id);
    let files: Vec<serde_json::Value> = artifacts
        .iter()
        .map(|a| {
            let bytes = a.read().map(|b| b.len() as u64).unwrap_or(0);
            json!({ "path": a.path, "bytes": bytes, "gzip": a.gzip })
        })
        .collect();
    let total: u64 = files.iter().filter_map(|f| f["bytes"].as_u64()).sum();
    emit(&json!({
        "provider": provider.id(),
        "id": session.id,
        "files": files,
        "totalBytes": total,
        "resumable": resumable,
    }));
}

/// Publish a session to one of the project's repositories.
pub fn publish_cmd(
    cwd: Option<&String>,
    label: Option<&String>,
    provider: Option<&String>,
    id: Option<&String>,
) {
    let (Some(cwd), Some(label)) = (cwd, label) else {
        fail("uso: remote-publish <cwd> <repositorio> <provider> <session-id>");
    };
    let (session, provider) = find_session(provider, id);
    let (artifacts, resumable) = gather(provider.as_ref(), &session.id);
    if artifacts.is_empty() {
        fail("esta sesión no tiene nada que publicar");
    }
    let body = stdin_json();
    let manifest = build_manifest(&session, provider.as_ref(), resumable, &body, cwd);
    let backend = backend(cwd, label);
    let published =
        store::publish(backend.as_ref(), &manifest, &artifacts).unwrap_or_else(|e| fail(&e));
    emit(&row(&published));
}

/// Bring a published session to disk and report what it takes to resume it.
///
/// The destination project is `cwd` — the one the user is in — never the cwd
/// recorded in the manifest, which belongs to whoever published and may not
/// exist here. A session already on disk is returned untouched: same id means
/// same session, and the local copy may hold turns the published one doesn't.
pub fn fetch_cmd(cwd: Option<&String>, label: Option<&String>, id: Option<&String>) {
    let (Some(cwd), Some(label), Some(id)) = (cwd, label, id) else {
        fail("uso: remote-fetch <cwd> <repositorio> <session-id>");
    };
    let backend = backend(cwd, label);
    let manifest = store::read_manifest(backend.as_ref(), id).unwrap_or_else(|e| fail(&e));
    let outcome = payload::hydrate(
        &manifest,
        cwd,
        |artifact| store::fetch(backend.as_ref(), id, artifact),
        &Layouts::default(),
    )
    .unwrap_or_else(|e| fail(&e));

    let resume_argv = all_providers()
        .iter()
        .find(|p| p.id() == manifest.provider)
        .map(|p| p.resume_argv(id))
        .unwrap_or_default();
    emit(&json!({
        "path": outcome.path.display().to_string(),
        "alreadyPresent": outcome.already_present,
        "provider": manifest.provider,
        "resumeArgv": resume_argv,
        "divergence": divergence(&manifest, Path::new(cwd)),
    }));
}

pub fn unpublish_cmd(cwd: Option<&String>, label: Option<&String>, id: Option<&String>) {
    let (Some(cwd), Some(label), Some(id)) = (cwd, label, id) else {
        fail("uso: remote-unpublish <cwd> <repositorio> <session-id>");
    };
    let backend = backend(cwd, label);
    store::unpublish(backend.as_ref(), id).unwrap_or_else(|e| fail(&e));
    emit(&json!({ "ok": true }));
}

/// Which of this project's sessions are already shared, and how the local copy
/// compares — so the *local* list can carry the same marks as a repository tab.
///
/// Best-effort by design: a repository that fails to answer costs a missing
/// mark, never a broken listing.
pub fn shared_cmd(cwd: Option<&String>) {
    let Some(cwd) = cwd else {
        fail("uso: remote-shared <cwd>");
    };
    let config = load_config();
    let key = project_key(Path::new(cwd));
    let providers = all_providers();
    let mut index: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut failures: Vec<serde_json::Value> = Vec::new();

    for link in resolved_links(&config, &key) {
        let backend = match backend_for(&link, &config, &token_dir()) {
            Ok(backend) => backend,
            Err(e) => {
                failures.push(json!({ "repo": link.label, "error": e }));
                continue;
            }
        };
        let manifests = match store::list(backend.as_ref()) {
            Ok(manifests) => manifests,
            Err(e) => {
                failures.push(json!({ "repo": link.label, "error": e }));
                continue;
            }
        };
        for m in manifests {
            let local_bytes = providers
                .iter()
                .find(|p| p.id() == m.provider)
                .and_then(|p| p.locate(&m.id))
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|meta| meta.len());
            let published_bytes = m
                .artifacts
                .iter()
                .find(|a| a.path == payload::MAIN)
                .map(|a| a.bytes)
                .unwrap_or(m.size_bytes);
            index.insert(
                format!("{}:{}", m.provider, m.id),
                json!({
                    "repo": link.label,
                    "publishedBy": m.published_by,
                    "publishedAt": m.published_at,
                    "state": LocalState::compare(local_bytes, published_bytes),
                }),
            );
        }
    }
    emit(&json!({ "shared": index, "failures": failures }));
}

// ── helpers ───────────────────────────────────────────────────────────────

/// A manifest as the frontends want it: camelCase, like every other command
/// here. The file in the repository stays snake_case — it's an interchange
/// format meant to be read by hand and byte-identical whichever backend wrote
/// it, so the translation belongs at this boundary, not in the format.
fn row(m: &RemoteManifest) -> serde_json::Value {
    json!({
        "id": m.id,
        "provider": m.provider,
        "publishedAt": m.published_at,
        "publishedBy": m.published_by,
        "cwd": m.cwd,
        "branch": m.branch,
        "gitRemote": m.git_remote,
        "gitHead": m.git_head,
        "displayName": m.display_name,
        "tags": m.tags,
        "firstPrompt": m.first_prompt,
        "messageCount": m.message_count,
        "sizeBytes": m.size_bytes,
        "resumable": m.resumable,
        "originFilename": m.origin_filename,
        "forkedFrom": m.forked_from,
        "artifacts": m.artifacts.iter().map(|a| json!({
            "path": a.path,
            "bytes": a.bytes,
            "gzip": a.gzip,
        })).collect::<Vec<_>>(),
    })
}

fn find_session(
    provider: Option<&String>,
    id: Option<&String>,
) -> (AgentSession, Box<dyn AgentProvider>) {
    let (Some(provider_id), Some(id)) = (provider, id) else {
        fail("faltan <provider> y <session-id>");
    };
    let provider = all_providers()
        .into_iter()
        .find(|p| p.id() == provider_id)
        .unwrap_or_else(|| fail(&format!("proveedor desconocido: {provider_id}")));
    let session = provider
        .list_sessions()
        .unwrap_or_else(|e| fail(&format!("scan falló: {e}")))
        .into_iter()
        .find(|s| &s.id == id)
        .unwrap_or_else(|| fail("sesión no encontrada"));
    (session, provider)
}

/// What travels for this session, and whether it can be resumed once brought
/// to another machine.
///
/// Providers that keep a per-session file publish it whole. The two that don't
/// (goose in SQLite, opencode behind its CLI) publish their rendered turns
/// instead: readable and searchable, explicitly not resumable.
fn gather(provider: &dyn AgentProvider, id: &str) -> (Vec<PendingArtifact>, bool) {
    if payload::provider_is_resumable(provider.id()) {
        if let Some(main) = provider.locate(id) {
            let artifacts = payload::collect_from_file(&main).unwrap_or_else(|e| fail(&e));
            return (artifacts, true);
        }
    }
    match provider.transcript(id) {
        Ok(turns) if !turns.is_empty() => (payload::collect_transcript(&turns), false),
        _ => fail(&format!(
            "no puedo leer el contenido de esta sesión de {}: no hay nada que publicar",
            provider.display_name()
        )),
    }
}

fn build_manifest(
    session: &AgentSession,
    provider: &dyn AgentProvider,
    resumable: bool,
    body: &serde_json::Value,
    cwd: &str,
) -> RemoteManifest {
    let metadata = MetadataStore::load(&metadata_path());
    let entry = metadata.get(provider.id(), &session.id);
    // Prefer the session's own cwd for the git snapshot: that's the code the
    // conversation is about. Fall back to the project the user is in.
    let source_cwd = session.cwd.clone().unwrap_or_else(|| cwd.to_string());
    let source = Path::new(&source_cwd);
    let source = if source.is_dir() {
        source
    } else {
        Path::new(cwd)
    };
    let (git_head, branch) = links::git_head(source);
    RemoteManifest {
        format: FORMAT.to_string(),
        version: VERSION,
        id: session.id.clone(),
        provider: provider.id().to_string(),
        published_at: timestamp_now(),
        published_by: body
            .get("publishedBy")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(publisher_identity),
        cwd: session.cwd.clone(),
        branch: session.branch.clone().or(branch),
        git_remote: links::git_origin(source)
            .as_deref()
            .map(links::normalize_git_remote),
        git_head,
        display_name: entry.and_then(|m| m.name.clone()),
        tags: entry.map(|m| m.tags.clone()).unwrap_or_default(),
        first_prompt: session.title.clone(),
        message_count: session.message_count,
        size_bytes: 0, // Filled by `store::publish` with what really went up.
        resumable,
        origin_filename: provider
            .locate(&session.id)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
        artifacts: Vec::new(),
        forked_from: None,
    }
}

/// Who published, taken from git's identity — the one email a developer's
/// machine reliably has configured.
fn publisher_identity() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "--get", "user.email"])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let email = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !email.is_empty() {
        return Some(email);
    }
    std::env::var("USER").ok().filter(|u| !u.is_empty())
}

/// The transcript travels; the code doesn't. If the session was recorded
/// against another commit, the conversation describes files that no longer
/// look like that — so say it before launching, rather than letting the user
/// discover it mid-conversation.
fn divergence(manifest: &RemoteManifest, cwd: &Path) -> Option<serde_json::Value> {
    let (head, _) = links::git_head(cwd);
    let local_origin = links::git_origin(cwd)
        .as_deref()
        .map(links::normalize_git_remote);
    let recorded_head = manifest.git_head.as_deref()?;
    let same_repo = match (&local_origin, &manifest.git_remote) {
        (Some(local), Some(recorded)) => local == recorded,
        _ => true, // Can't tell: don't claim a mismatch that may not exist.
    };
    let head = head?;
    if same_repo && head == recorded_head {
        return None;
    }
    Some(json!({
        "recordedHead": recorded_head,
        "localHead": head,
        "recordedRemote": manifest.git_remote,
        "localRemote": local_origin,
        "sameRepo": same_repo,
    }))
}
