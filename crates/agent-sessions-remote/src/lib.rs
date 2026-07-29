//! Shared sessions: publish what an agent recorded here, resume what a
//! colleague recorded there.
//!
//! Local-first. The repository is a store, never the working directory — the
//! obvious alternative (mounting `~/.claude` over the network) fails on
//! identity, not throughput: each agent derives its project folder from the
//! local cwd, so two people with different `$HOME`s write to different folders
//! of the same mount and never see each other. And when the paths *do* line
//! up, two processes appending to one transcript over NFS corrupt it at the
//! byte level. So the session travels as a payload and gets written back into
//! whatever shape the local provider expects.
//!
//! Module map:
//!
//! - [`manifest`] — the document that makes a session visible.
//! - [`payload`] — what travels, and where it lands per provider.
//! - [`store`] — repository layout, gzip, and the four operations a backend
//!   must provide.
//! - [`directory`] / [`git`] / [`http`] — the backends.
//! - [`links`] — servers configured once, repositories linked per project.

pub mod directory;
pub mod git;
pub mod http;
pub mod links;
pub mod manifest;
pub mod payload;
pub mod store;

pub use manifest::{Artifact, RemoteManifest};
pub use store::RepoBackend;

/// How the local copy of a published session compares to the published one.
///
/// Comparing sizes is enough because transcripts are append-only: any
/// difference is real content, never a rewrite. The cost is one `stat` per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalState {
    /// Not on this machine.
    Absent,
    /// Same size as the manifest declares.
    Current,
    /// The manifest declares more: somebody continued it and republished.
    Stale,
    /// The local copy is bigger: you kept working without publishing.
    Ahead,
}

impl LocalState {
    pub fn compare(local_bytes: Option<u64>, published_bytes: u64) -> Self {
        match local_bytes {
            None => Self::Absent,
            Some(local) if local == published_bytes => Self::Current,
            Some(local) if local < published_bytes => Self::Stale,
            Some(_) => Self::Ahead,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_state_reads_the_four_cases() {
        assert_eq!(LocalState::compare(None, 10), LocalState::Absent);
        assert_eq!(LocalState::compare(Some(10), 10), LocalState::Current);
        assert_eq!(LocalState::compare(Some(4), 10), LocalState::Stale);
        assert_eq!(LocalState::compare(Some(20), 10), LocalState::Ahead);
    }
}
