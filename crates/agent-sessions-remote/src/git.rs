//! A repository reached with the `git` binary over SSH.
//!
//! This is the backend to prefer when two people publish at once: git already
//! knows how to resolve that, so a rejected push becomes rebase-and-retry and
//! both publications survive. The API drivers can't do that — there the second
//! write overwrites the first.
//!
//! Three details that are not obvious and were all learned the hard way:
//!
//! - **`LC_ALL=C` is mandatory.** git translates its errors, so on a Spanish
//!   system "repository does not exist" arrives as "el repositorio no existe"
//!   and any reading of stderr silently stops working.
//! - **`GIT_TERMINAL_PROMPT=0` and `BatchMode=yes`.** A credential prompt
//!   blocked inside a background worker is invisible and looks like a hang;
//!   failing with a message is strictly better.
//! - **`git add -A`, not `git add -- manifest blobs`.** Once a deletion has
//!   removed both directories, naming them fails with "pathspec did not match
//!   any files" — and `-A` records deletions as readily as additions.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::store::RepoBackend;

/// Interactive checks fail fast on purpose: being wrong quickly beats being
/// right after the user has given up.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Clones and pushes of a sessions repository can be slow but not endless.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

pub struct GitRemote {
    host: String,
    port: u16,
    /// `group/repo`.
    repo: String,
    branch: String,
    checkout: PathBuf,
    /// Whether this instance already refreshed its working copy. The sidecar
    /// is one-shot, so once per instance is once per command.
    synced: Mutex<bool>,
}

impl GitRemote {
    pub fn new(host: String, port: u16, repo: String, branch: String) -> Self {
        let checkout = cache_root().join(slug(&format!("{host}-{repo}-{branch}")));
        Self {
            host,
            port,
            repo,
            branch,
            checkout,
            synced: Mutex::new(false),
        }
    }

    /// Where to clone. Overridable so tests never write to a real cache.
    pub fn with_checkout(mut self, dir: impl Into<PathBuf>) -> Self {
        self.checkout = dir.into();
        self
    }

    /// The familiar `git@host:group/repo.git` form can't express a port —
    /// what follows the colon is the path — so a non-standard one forces the
    /// explicit `ssh://` form.
    pub fn url(&self) -> String {
        let repo = self.repo.trim_matches('/');
        let repo = if repo.ends_with(".git") {
            repo.to_string()
        } else {
            format!("{repo}.git")
        };
        if self.port == 22 {
            format!("git@{}:{repo}", self.host)
        } else {
            format!("ssh://git@{}:{}/{repo}", self.host, self.port)
        }
    }

    fn git(&self, args: &[&str]) -> Result<Output, String> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(if self.checkout.is_dir() {
                self.checkout.clone()
            } else {
                std::env::temp_dir()
            })
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(
                "GIT_SSH_COMMAND",
                "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new",
            );
        run(cmd, GIT_TIMEOUT)
    }

    fn git_ok(&self, args: &[&str]) -> Result<(), String> {
        let out = self.git(args)?;
        if out.status.success() {
            return Ok(());
        }
        Err(git_error(&out))
    }

    /// Clone or refresh the working copy. An empty repository (no branch yet)
    /// is a normal starting state, not a failure: the first publication is
    /// what creates the branch.
    fn ensure_synced(&self) -> Result<(), String> {
        let mut synced = self
            .synced
            .lock()
            .map_err(|_| "estado inconsistente".to_string())?;
        if *synced {
            return Ok(());
        }
        if self.checkout.join(".git").is_dir() {
            self.refresh()?;
        } else {
            self.clone_fresh()?;
        }
        *synced = true;
        Ok(())
    }

    fn clone_fresh(&self) -> Result<(), String> {
        if let Some(parent) = self.checkout.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let url = self.url();
        let dest = self.checkout.display().to_string();
        let cloned = self.git(&["clone", "--branch", &self.branch, &url, &dest])?;
        if cloned.status.success() {
            return Ok(());
        }
        // Either the repository is empty or the branch doesn't exist yet.
        // Both are fine: start a local history that pushes cleanly later.
        let plain = self.git(&["clone", &url, &dest])?;
        if !plain.status.success() {
            return Err(git_error(&plain));
        }
        self.git_ok(&["checkout", "-B", &self.branch])
    }

    fn refresh(&self) -> Result<(), String> {
        // Discard whatever a previous interrupted run left: every publication
        // pushes immediately, so a dirty checkout is debris, not work.
        let _ = self.git(&["reset", "--hard"]);
        let _ = self.git(&["clean", "-fd"]);
        let fetched = self.git(&["fetch", "origin", &self.branch])?;
        if fetched.status.success() {
            self.git_ok(&["reset", "--hard", &format!("origin/{}", self.branch)])?;
        }
        Ok(())
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        if path.split('/').any(|c| c == ".." || c.is_empty()) {
            return Err(format!("ruta sospechosa en el repositorio: {path}"));
        }
        Ok(self.checkout.join(path))
    }

    /// Push, and if somebody else got there first, rebase onto their work and
    /// try again. This is the whole reason to prefer SSH over the API drivers.
    fn push_with_rebase(&self) -> Result<(), String> {
        let refspec = format!("HEAD:{}", self.branch);
        for attempt in 0..3 {
            let pushed = self.git(&["push", "origin", &refspec])?;
            if pushed.status.success() {
                return Ok(());
            }
            if attempt == 2 {
                return Err(git_error(&pushed));
            }
            let rebased = self.git(&["pull", "--rebase", "origin", &self.branch])?;
            if !rebased.status.success() {
                return Err(git_error(&rebased));
            }
        }
        Ok(())
    }
}

impl RepoBackend for GitRemote {
    fn describe(&self) -> String {
        format!("{} ({})", self.repo, self.host)
    }

    fn list_files(&self, prefix: &str) -> Result<Vec<String>, String> {
        self.ensure_synced()?;
        let dir = self.resolve(prefix)?;
        let mut out = Vec::new();
        walk(&dir, prefix, &mut out);
        out.sort();
        Ok(out)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        self.ensure_synced()?;
        let full = self.resolve(path)?;
        std::fs::read(&full).map_err(|e| format!("{path}: {e}"))
    }

    fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        self.ensure_synced()?;
        let full = self.resolve(path)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&full, bytes).map_err(|e| format!("{path}: {e}"))
    }

    fn delete_file(&self, path: &str) -> Result<(), String> {
        self.ensure_synced()?;
        let full = self.resolve(path)?;
        match std::fs::remove_file(&full) {
            Ok(()) | Err(_) => Ok(()),
        }
    }

    fn begin(&self) -> Result<(), String> {
        self.ensure_synced()
    }

    /// One commit per publication, which is what makes this backend's
    /// interrupted-transfer story better than the API ones: a commit either
    /// lands whole or not at all.
    fn commit(&self, message: &str) -> Result<(), String> {
        self.git_ok(&["add", "-A"])?;
        let staged = self.git(&["diff", "--cached", "--quiet"])?;
        if staged.status.success() {
            return Ok(()); // Nothing changed: republishing identical content.
        }
        self.git_ok(&[
            "-c",
            "user.name=aterm",
            "-c",
            "user.email=aterm@localhost",
            "commit",
            "-m",
            message,
        ])?;
        self.push_with_rebase()
    }
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
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

/// git's own words, trimmed. Kept raw rather than reinterpreted: guessing at
/// the cause of a git failure produces worse messages than quoting it.
fn git_error(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = stderr.trim();
    if text.is_empty() {
        format!("git falló con código {}", out.status.code().unwrap_or(-1))
    } else {
        text.to_string()
    }
}

fn cache_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("aterm")
        .join("remote-repos")
}

fn slug(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Check that SSH access to a host works, and say something useful when it
/// doesn't.
///
/// `ssh -T` is what GitHub and GitLab expect for this: it needs no repository
/// and both answer with the account name, which is what you actually want
/// confirmed. The earlier approach — asking for a repository that doesn't
/// exist and treating the 404 as success — ended up showing users a fabricated
/// URL that belongs to nobody.
pub fn probe_ssh(host: &str, port: u16) -> Result<String, String> {
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-T",
        "-p",
        &port.to_string(),
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ConnectTimeout=10",
        &format!("git@{host}"),
    ])
    .env("LC_ALL", "C");
    let out = run(cmd, PROBE_TIMEOUT)?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let said = said.trim().to_string();
    // GitHub exits non-zero *on success* ("does not provide shell access"),
    // so the exit code says nothing and only the greeting counts.
    if said.contains("successfully authenticated") || said.contains("Welcome to GitLab") {
        return Ok(said);
    }
    if said.contains("Permission denied") {
        return Err(format!(
            "{host} rechazó tu clave SSH — el usuario SSH es siempre «git», no tu cuenta"
        ));
    }
    if said.is_empty() {
        // The worst symptom there is: ssh against the wrong port returns
        // nothing at all, so the check used to report an empty failure.
        return Err(if port == 22 {
            format!("{host} no respondió por SSH en el puerto 22 — algunos GitLab autoalojados escuchan en otro (2211 es habitual); revísalo en Ajustes › Servidores")
        } else {
            format!("{host}:{port} no respondió por SSH")
        });
    }
    Err(said)
}

/// Run a command with a wall-clock limit, killing it if it overruns.
///
/// Nothing that touches the network may block forever: inside a UI worker a
/// hung process is invisible and reads as a frozen application.
fn run(mut cmd: Command, timeout: Duration) -> Result<Output, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("no puedo ejecutar {:?}: {e}", cmd.get_program()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{:?} no respondió en {} s",
                        cmd.get_program(),
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    child.wait_with_output().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    #[test]
    fn url_only_uses_the_ssh_form_when_the_port_is_not_standard() {
        let plain = GitRemote::new(
            "github.com".to_string(),
            22,
            "equipo/sesiones".to_string(),
            "main".to_string(),
        );
        assert_eq!(plain.url(), "git@github.com:equipo/sesiones.git");

        let ported = GitRemote::new(
            "git.empresa.com".to_string(),
            2211,
            "equipo/sesiones.git".to_string(),
            "main".to_string(),
        );
        assert_eq!(
            ported.url(),
            "ssh://git@git.empresa.com:2211/equipo/sesiones.git"
        );
    }

    /// A bare repository on disk is a real git remote: no network, no server,
    /// but the same clone / commit / push path the SSH backend uses.
    fn local_remote(dir: &Path) -> GitRemote {
        let bare = dir.join("origin.git");
        assert!(Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .arg(&bare)
            .env("LC_ALL", "C")
            .output()
            .expect("git disponible")
            .status
            .success());
        GitRemote::new(
            "localhost".to_string(),
            22,
            bare.display().to_string(),
            "main".to_string(),
        )
        .with_checkout(dir.join("work"))
    }

    #[test]
    fn publishes_and_unpublishes_through_a_real_git_repository() {
        if Command::new("git").arg("--version").output().is_err() {
            return; // No git on this machine: nothing to exercise.
        }
        let tmp = tempfile::tempdir().unwrap();
        let remote = local_remote(tmp.path());
        // `url()` would wrap a local path in git@…: point straight at it.
        let url = remote.repo.clone();
        assert!(Command::new("git")
            .args(["clone", &url])
            .arg(tmp.path().join("work"))
            .env("LC_ALL", "C")
            .output()
            .unwrap()
            .status
            .success());

        remote.begin().unwrap();
        remote.write_file("manifest/a.json", b"{}").unwrap();
        remote
            .write_file("blobs/a/session.jsonl.gz", b"contenido")
            .unwrap();
        remote.commit("publica a").unwrap();

        // A second instance sees it only if the push really happened.
        let reader = GitRemote::new("localhost".to_string(), 22, url.clone(), "main".to_string())
            .with_checkout(tmp.path().join("otro"));
        assert!(Command::new("git")
            .args(["clone", &url])
            .arg(tmp.path().join("otro"))
            .env("LC_ALL", "C")
            .output()
            .unwrap()
            .status
            .success());
        assert_eq!(reader.read_file("manifest/a.json").unwrap(), b"{}");

        // Deleting both directories must not trip `git add`.
        store::unpublish(&remote, "a").unwrap();
        assert!(remote.list_files("manifest").unwrap().is_empty());
        assert!(remote.list_files("blobs/a").unwrap().is_empty());
    }
}
