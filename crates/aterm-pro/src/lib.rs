//! Community placeholder for the private `aterm-pro` crate.
//!
//! **This is not the Pro implementation.** The real one (parallel worktree
//! compare, workspace profiles, Pro dashboard, HTML export, session porting,
//! memory graph, MCP setup) lives in the private `Aterm-labs/aterm-pro` repo and
//! is copied over this directory by its `build.sh` when producing the official
//! binary.
//!
//! Why a placeholder instead of nothing: Cargo requires the manifest of every
//! declared `path` dependency to exist on disk, even when the dependency is
//! optional and its feature is off. So `aterm` keeps declaring `aterm-pro`, this
//! crate keeps the build green in the public checkout, and the official build
//! swaps the directory. Either way `crates/aterm/src/pro.rs` owns the seam and
//! the chrome is identical across editions.
//!
//! Note that the Community stub the app actually uses when built *without*
//! `--features pro` is `CommunityPro` in `crates/aterm/src/pro.rs`; this crate is
//! only reached by `--features pro`, i.e. an official build.

use aterm_pro_api::{ProHost, ProModule};

/// Builds the placeholder module. An official build replaces this crate, so
/// reaching this code means the `pro` feature was enabled on a Community
/// checkout — say so instead of pretending the features are there.
pub fn module() -> Box<dyn ProModule> {
    Box::new(PlaceholderPro)
}

struct PlaceholderPro;

impl PlaceholderPro {
    fn decline(host: &mut dyn ProHost, feature: &str) {
        host.notify(format!(
            "«{feature}» no está en esta compilación: es una función Pro y su \
             código no se distribuye con la edición Community."
        ));
    }
}

impl ProModule for PlaceholderPro {
    fn open_parallel(&mut self, host: &mut dyn ProHost) {
        Self::decline(host, "Comparativa paralela");
    }
    fn run_compare(&mut self, host: &mut dyn ProHost) {
        Self::decline(host, "Comparar worktrees");
    }
    fn open_cleanup(&mut self, host: &mut dyn ProHost) {
        Self::decline(host, "Limpiar worktrees");
    }
    fn open_features(&mut self, host: &mut dyn ProHost) {
        Self::decline(host, "Funciones Pro");
    }
    fn ui(&mut self, _ctx: &egui::Context, _host: &mut dyn ProHost) {}
    fn edition(&self) -> &'static str {
        "Community"
    }
}
