//! A repository that is just a directory.
//!
//! Not only a test double: it's a real backend (a shared folder, Syncthing, a
//! read-only NFS mount) that needs no token, no server and no network, and
//! it's what makes the feature testable in CI. It's also the backend where the
//! layout is inspectable by hand, which is half the point of keeping the
//! on-remote format plain files.

use std::path::{Component, Path, PathBuf};

use crate::store::RepoBackend;

pub struct DirectoryRemote {
    root: PathBuf,
}

impl DirectoryRemote {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve a repository-relative path, refusing anything that climbs out.
    /// Paths come from manifests other people wrote.
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let rel = Path::new(path);
        if rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(format!("ruta sospechosa en el repositorio: {path}"));
        }
        Ok(self.root.join(rel))
    }

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
                Self::walk(&entry.path(), &rel, out);
            } else {
                out.push(rel);
            }
        }
    }
}

impl RepoBackend for DirectoryRemote {
    fn describe(&self) -> String {
        self.root.display().to_string()
    }

    fn list_files(&self, prefix: &str) -> Result<Vec<String>, String> {
        let dir = self.resolve(prefix)?;
        let mut out = Vec::new();
        Self::walk(&dir, prefix, &mut out);
        out.sort();
        Ok(out)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        let full = self.resolve(path)?;
        std::fs::read(&full).map_err(|e| format!("{path}: {e}"))
    }

    fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let full = self.resolve(path)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&full, bytes).map_err(|e| format!("{path}: {e}"))
    }

    fn delete_file(&self, path: &str) -> Result<(), String> {
        let full = self.resolve(path)?;
        match std::fs::remove_file(&full) {
            Ok(()) => {}
            // Idempotent: an already-absent file is a finished job.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{path}: {e}")),
        }
        // Leave no empty scaffolding behind, but never climb past the root.
        let mut dir = full.parent().map(Path::to_path_buf);
        while let Some(current) = dir {
            if current == self.root || !current.starts_with(&self.root) {
                break;
            }
            if std::fs::remove_dir(&current).is_err() {
                break;
            }
            dir = current.parent().map(Path::to_path_buf);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_reads_lists_and_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = DirectoryRemote::new(tmp.path());
        assert!(remote.list_files("manifest").unwrap().is_empty());

        remote.write_file("manifest/a.json", b"{}").unwrap();
        remote.write_file("blobs/a/session.jsonl.gz", b"x").unwrap();
        assert_eq!(remote.read_file("manifest/a.json").unwrap(), b"{}");
        assert_eq!(
            remote.list_files("blobs/a").unwrap(),
            vec!["blobs/a/session.jsonl.gz".to_string()]
        );

        remote.delete_file("blobs/a/session.jsonl.gz").unwrap();
        assert!(remote.list_files("blobs/a").unwrap().is_empty());
        // Empty scaffolding is cleaned up, the root survives.
        assert!(!tmp.path().join("blobs/a").exists());
        assert!(tmp.path().is_dir());
        // Deleting again is still success.
        remote.delete_file("blobs/a/session.jsonl.gz").unwrap();
    }

    #[test]
    fn refuses_paths_that_climb_out_of_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = DirectoryRemote::new(tmp.path().join("repo"));
        let err = remote.write_file("../fuera.json", b"x").unwrap_err();
        assert!(err.contains("sospechosa"), "{err}");
        assert!(!tmp.path().join("fuera.json").exists());
    }
}
