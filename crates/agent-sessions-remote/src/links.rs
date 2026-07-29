//! Servers configured once, repositories linked per project.
//!
//! The two are separate because they change at different rates: a company has
//! one or two servers and one sessions repository per client, so typing the
//! host and pasting a token into every repository was repeated work that left
//! the same credential in several places. Links name their server, so fixing a
//! URL or rotating a token fixes every repository pointing at it.
//!
//! A link whose server doesn't exist resolves to an error, not to some other
//! destination: inert, and visibly inert, beats publishing somewhere else
//! without saying so.
//!
//! The key of a project's links is its **normalised git origin**, so every
//! worktree and every sibling checkout of a repository shares one set of
//! links, and `git@host:g/r.git` and `https://host/g/r.git` are the same key.
//! A project without an origin falls back to its absolute path: still works on
//! one machine, doesn't travel between checkouts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::directory::DirectoryRemote;
use crate::git::GitRemote;
use crate::http::{GitHubRemote, GitLabRemote};
use crate::store::RepoBackend;

/// Total override to a plain directory, for trying the feature out (and for
/// tests) without touching shared state. Wins over everything else.
pub const DIR_OVERRIDE_ENV: &str = "ATERM_REMOTE_DIR";
/// Token for the server being used, when you'd rather not persist one.
pub const TOKEN_ENV: &str = "ATERM_REMOTE_TOKEN";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    /// A directory on this machine (or a mounted one). No server, no auth.
    Directory,
    /// GitLab, self-hosted or gitlab.com.
    Gitlab,
    /// GitHub, self-hosted (Enterprise) or github.com.
    Github,
    /// Any git host, reached with the `git` binary over SSH.
    Git,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// REST API with a personal access token.
    Token,
    /// The SSH keys already deployed on this machine.
    Ssh,
}

/// Where a repository lives and how we authenticate to it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteServer {
    /// Unique, human-chosen; this is what links reference.
    pub name: String,
    pub kind: ServerKind,
    /// Empty for gitlab.com / github.com, which have known defaults, and for
    /// directory servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub auth: AuthKind,
    /// SSH port. It cannot be derived: the web URL answers on 443 whether SSH
    /// listens on 22 or 2211, and the familiar `git@host:group/repo.git` form
    /// can't express a port at all (what follows the colon is the path), so a
    /// non-standard port forces the explicit `ssh://` form.
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
}

fn default_ssh_port() -> u16 {
    22
}

impl RemoteServer {
    /// The API host, falling back to the provider's public one so nobody has
    /// to type a URL for gitlab.com or github.com.
    pub fn api_host(&self) -> String {
        match (
            self.host
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty()),
            self.kind,
        ) {
            (Some(h), _) => h.trim_end_matches('/').to_string(),
            (None, ServerKind::Gitlab) => "https://gitlab.com".to_string(),
            (None, ServerKind::Github) => "https://api.github.com".to_string(),
            (None, _) => String::new(),
        }
    }

    /// Bare host for SSH (`git@<here>:group/repo.git`).
    pub fn ssh_host(&self) -> String {
        let host = self.api_host();
        let host = host
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        // api.github.com is the API endpoint, not the SSH one.
        if host == "api.github.com" {
            return "github.com".to_string();
        }
        host.to_string()
    }
}

/// A repository on a server, shown as one tab in the session list.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteLink {
    /// Tab label. Free text: "sesiones-equipo" reads better than a path.
    pub label: String,
    /// Name of the server this repository lives on.
    pub server: String,
    /// `group/repo` for hosted servers; an absolute path for directory ones.
    pub repo: String,
    #[serde(default = "default_branch")]
    pub branch: String,
}

fn default_branch() -> String {
    "main".to_string()
}

/// Servers plus every project's links, in one inspectable file.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemotesConfig {
    #[serde(default)]
    pub servers: Vec<RemoteServer>,
    /// project key -> links. BTreeMap so the file has a stable order and
    /// diffs stay readable.
    #[serde(default)]
    pub links: BTreeMap<String, Vec<RemoteLink>>,
    /// Fallback for projects with no links of their own.
    #[serde(default)]
    pub global: Vec<RemoteLink>,
}

impl RemotesConfig {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn server(&self, name: &str) -> Option<&RemoteServer> {
        self.servers.iter().find(|s| s.name == name)
    }

    /// Add or replace a server by name.
    pub fn set_server(&mut self, server: RemoteServer) {
        match self.servers.iter_mut().find(|s| s.name == server.name) {
            Some(existing) => *existing = server,
            None => self.servers.push(server),
        }
    }

    /// Remove a server. Links naming it are left alone on purpose: deleting
    /// them silently would lose the user's repository list, and a link to a
    /// missing server already reports itself clearly.
    pub fn remove_server(&mut self, name: &str) {
        self.servers.retain(|s| s.name != name);
    }

    /// The links that apply to a project, and where they came from.
    ///
    /// Own links win over the global fallback **entirely**, they don't add up:
    /// a project linked to a client's repository must not also publish to the
    /// default one.
    pub fn resolve(&self, project_key: &str) -> Vec<RemoteLink> {
        match self.links.get(project_key) {
            Some(links) if !links.is_empty() => links.clone(),
            _ => self.global.clone(),
        }
    }

    pub fn set_links(&mut self, project_key: &str, links: Vec<RemoteLink>) {
        if links.is_empty() {
            self.links.remove(project_key);
        } else {
            self.links.insert(project_key.to_string(), links);
        }
    }
}

/// `~/.config/aterm/remotes.json`, alongside the metadata both UIs share.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_default()
        .join("aterm")
        .join("remotes.json")
}

/// Directory holding one file per server's token, never the config file: a
/// JSON blob full of settings gets copied, pasted and screenshotted.
pub fn token_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_default()
        .join("aterm")
        .join("remote-tokens")
}

/// Read a server's token: the environment override first, then its file.
pub fn read_token(dir: &Path, server: &str) -> Option<String> {
    if let Ok(from_env) = std::env::var(TOKEN_ENV) {
        if !from_env.trim().is_empty() {
            return Some(from_env.trim().to_string());
        }
    }
    let raw = std::fs::read_to_string(dir.join(sanitize(server))).ok()?;
    let token = raw.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Write a server's token with owner-only permissions. An empty token clears
/// it, so the UI has a way to undo.
pub fn write_token(dir: &Path, server: &str, token: &str) -> Result<(), String> {
    let path = dir.join(sanitize(server));
    if token.trim().is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(&path, token.trim()).map_err(|e| e.to_string())?;
    set_owner_only(&path)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<(), String> {
    // Windows inherits the user profile's ACL, which is already owner-only.
    Ok(())
}

/// One path component, whatever the server was called.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The key under which a project's links are stored: its normalised origin,
/// falling back to the absolute path when there's no git remote.
pub fn project_key(cwd: &Path) -> String {
    git_origin(cwd)
        .as_deref()
        .map(normalize_git_remote)
        .unwrap_or_else(|| cwd.display().to_string())
}

/// `git@host:group/repo.git`, `https://host/group/repo.git` and
/// `ssh://git@host:2211/group/repo.git` are the same repository, so they must
/// produce the same key — nobody should have to link a repository twice.
pub fn normalize_git_remote(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let rest = if let Some(rest) = url.strip_prefix("ssh://") {
        rest
    } else if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else if let Some(rest) = url.strip_prefix("git://") {
        rest
    } else {
        url
    };
    // Drop any user@ prefix.
    let rest = rest.split_once('@').map_or(rest, |(_, after)| after);
    // An explicit port is transport, not identity — and it has to go before
    // the scp-like rewrite below, which would otherwise turn `:2211` into a
    // path segment.
    let rest = strip_port(rest);
    // scp-like syntax: host:group/repo → host/group/repo.
    let rest = rest.replacen(':', "/", 1);
    rest.trim_end_matches(".git")
        .trim_end_matches('/')
        .to_string()
}

fn strip_port(rest: &str) -> String {
    let Some((host, tail)) = rest.split_once(':') else {
        return rest.to_string();
    };
    match tail.split_once('/') {
        // `host:2211/group/repo` — a numeric segment before the path is a port.
        Some((port, path)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            format!("{host}/{path}")
        }
        // `host:group/repo` — scp-like, the colon is the path separator.
        _ => rest.to_string(),
    }
}

/// `origin` of the repository containing `cwd`, when there is one.
pub fn git_origin(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// Current commit and branch of the repository containing `cwd`, for the
/// divergence warning at hydration time.
pub fn git_head(cwd: &Path) -> (Option<String>, Option<String>) {
    let run = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!value.is_empty()).then_some(value)
    };
    (
        run(&["rev-parse", "--short", "HEAD"]),
        run(&["rev-parse", "--abbrev-ref", "HEAD"]),
    )
}

/// Build the backend a link points at.
///
/// `ATERM_REMOTE_DIR` short-circuits everything, which is how the feature can
/// be tried out with nothing configured.
pub fn backend_for(
    link: &RemoteLink,
    config: &RemotesConfig,
    token_dir: &Path,
) -> Result<Box<dyn RepoBackend>, String> {
    if let Ok(dir) = std::env::var(DIR_OVERRIDE_ENV) {
        if !dir.trim().is_empty() {
            return Ok(Box::new(DirectoryRemote::new(dir.trim())));
        }
    }
    let server = config.server(&link.server).ok_or_else(|| {
        format!(
            "el repositorio «{}» apunta al servidor «{}», que ya no existe: revísalo en los ajustes",
            link.label, link.server
        )
    })?;
    match (server.kind, server.auth) {
        (ServerKind::Directory, _) => Ok(Box::new(DirectoryRemote::new(&link.repo))),
        (ServerKind::Git, _) | (_, AuthKind::Ssh) => Ok(Box::new(GitRemote::new(
            server.ssh_host(),
            server.ssh_port,
            link.repo.clone(),
            link.branch.clone(),
        ))),
        (ServerKind::Gitlab, AuthKind::Token) => {
            let token = require_token(token_dir, server)?;
            Ok(Box::new(GitLabRemote::new_gitlab(
                server.api_host(),
                link.repo.clone(),
                link.branch.clone(),
                token,
            )))
        }
        (ServerKind::Github, AuthKind::Token) => {
            let token = require_token(token_dir, server)?;
            Ok(Box::new(GitHubRemote::new_github(
                server.api_host(),
                link.repo.clone(),
                link.branch.clone(),
                token,
            )))
        }
    }
}

fn require_token(dir: &Path, server: &RemoteServer) -> Result<String, String> {
    read_token(dir, &server.name).ok_or_else(|| {
        format!(
            "el servidor «{}» no tiene token guardado: añádelo en los ajustes o expórtalo en {TOKEN_ENV}",
            server.name
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `backend_for` reads process-wide environment, and cargo runs tests in
    /// threads: without this every test that touches it would be flaky.
    static ENV: Mutex<()> = Mutex::new(());

    fn server(name: &str, kind: ServerKind, auth: AuthKind) -> RemoteServer {
        RemoteServer {
            name: name.to_string(),
            kind,
            host: None,
            auth,
            ssh_port: 22,
        }
    }

    fn link(label: &str, server: &str) -> RemoteLink {
        RemoteLink {
            label: label.to_string(),
            server: server.to_string(),
            repo: "equipo/sesiones".to_string(),
            branch: "main".to_string(),
        }
    }

    #[test]
    fn the_same_repository_written_four_ways_gives_one_key() {
        let expected = "git.empresa.com/odoo-16/fl-v16";
        for url in [
            "git@git.empresa.com:odoo-16/fl-v16.git",
            "https://git.empresa.com/odoo-16/fl-v16.git",
            "https://git.empresa.com/odoo-16/fl-v16",
            "ssh://git@git.empresa.com:2211/odoo-16/fl-v16.git",
        ] {
            assert_eq!(normalize_git_remote(url), expected, "{url}");
        }
    }

    #[test]
    fn own_links_replace_the_global_fallback_instead_of_adding_to_it() {
        let mut config = RemotesConfig {
            global: vec![link("global", "s")],
            ..Default::default()
        };
        assert_eq!(config.resolve("proyecto-a"), vec![link("global", "s")]);

        config.set_links("proyecto-a", vec![link("cliente", "s")]);
        assert_eq!(config.resolve("proyecto-a"), vec![link("cliente", "s")]);
        assert_eq!(config.resolve("proyecto-b"), vec![link("global", "s")]);

        // Clearing a project's links brings the fallback back.
        config.set_links("proyecto-a", Vec::new());
        assert_eq!(config.resolve("proyecto-a"), vec![link("global", "s")]);
    }

    #[test]
    fn a_link_to_a_missing_server_is_an_error_not_another_destination() {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let config = RemotesConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        let err = backend_for(&link("equipo", "no-existe"), &config, tmp.path())
            .err()
            .expect("un servidor inexistente no puede resolver a un destino");
        assert!(err.contains("ya no existe"), "{err}");
    }

    #[test]
    fn a_token_server_without_a_token_says_where_to_put_one() {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let mut config = RemotesConfig::default();
        config.set_server(server("gl", ServerKind::Gitlab, AuthKind::Token));
        let tmp = tempfile::tempdir().unwrap();
        let err = backend_for(&link("equipo", "gl"), &config, tmp.path())
            .err()
            .expect("sin token no hay destino");
        assert!(err.contains("token"), "{err}");
    }

    #[test]
    fn tokens_live_outside_the_config_and_are_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        write_token(tmp.path(), "gl", "secreto").unwrap();
        assert_eq!(read_token(tmp.path(), "gl").as_deref(), Some("secreto"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.path().join("gl"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Saving the config never carries the token.
        let mut config = RemotesConfig::default();
        config.set_server(server("gl", ServerKind::Gitlab, AuthKind::Token));
        let path = tmp.path().join("remotes.json");
        config.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("secreto"));

        write_token(tmp.path(), "gl", "").unwrap();
        assert!(read_token(tmp.path(), "gl").is_none());
    }

    #[test]
    fn config_round_trips_and_older_files_without_new_keys_still_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("remotes.json");
        let mut config = RemotesConfig::default();
        config.set_server(server("gl", ServerKind::Gitlab, AuthKind::Token));
        config.set_links("clave", vec![link("equipo", "gl")]);
        config.save(&path).unwrap();
        assert_eq!(RemotesConfig::load(&path), config);

        std::fs::write(
            &path,
            r#"{"servers":[{"name":"x","kind":"git","auth":"ssh"}]}"#,
        )
        .unwrap();
        let loaded = RemotesConfig::load(&path);
        assert_eq!(loaded.servers[0].ssh_port, 22);
        // An unreadable file is an empty config, never a crash.
        std::fs::write(&path, "no soy json").unwrap();
        assert_eq!(RemotesConfig::load(&path), RemotesConfig::default());
    }

    #[test]
    fn api_and_ssh_hosts_fall_back_to_the_public_ones() {
        let gl = server("gl", ServerKind::Gitlab, AuthKind::Token);
        assert_eq!(gl.api_host(), "https://gitlab.com");
        assert_eq!(gl.ssh_host(), "gitlab.com");

        let gh = server("gh", ServerKind::Github, AuthKind::Token);
        assert_eq!(gh.api_host(), "https://api.github.com");
        assert_eq!(gh.ssh_host(), "github.com");

        let mut own = server("propio", ServerKind::Gitlab, AuthKind::Ssh);
        own.host = Some("https://git.empresa.com/".to_string());
        assert_eq!(own.api_host(), "https://git.empresa.com");
        assert_eq!(own.ssh_host(), "git.empresa.com");
    }

    #[test]
    fn the_directory_override_wins_over_any_configuration() {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let mut config = RemotesConfig::default();
        config.set_server(server("gl", ServerKind::Gitlab, AuthKind::Token));
        std::env::set_var(DIR_OVERRIDE_ENV, tmp.path());
        let backend = backend_for(&link("equipo", "gl"), &config, tmp.path())
            .unwrap_or_else(|e| panic!("el override siempre resuelve: {e}"));
        assert_eq!(backend.describe(), tmp.path().display().to_string());
        std::env::remove_var(DIR_OVERRIDE_ENV);
    }
}
