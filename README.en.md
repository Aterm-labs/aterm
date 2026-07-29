<div align="center">

# aterm

**A native Rust terminal with a coding-agent session manager built in.**

List, preview and resume your Claude Code, Codex, OpenCode, Gemini CLI, Qwen
Code, Goose and Factory Droid conversations — without leaving the terminal and
without depending on an editor.

[![License](https://img.shields.io/badge/license-MIT-e0af68)](#licence)
[![Rust](https://img.shields.io/badge/rust-edition%202021-e0883b)](#building)
[![Tests](https://img.shields.io/badge/tests-passing-6bd089)](#tests)

[Website](https://atermlabs.jesuslorenzo.es) ·
[VS Code extension](https://github.com/Aterm-labs/agent-sessions) ·
[Español](./README.md)

<!-- SCREENSHOT PENDING — see CAPTURAS.md in the meta-repo. Once the file is in,
     drop these two comment lines and keep the image:
<img src="media/panel-nativo.png" alt="The session panel and a terminal in one window" width="900" />
-->

</div>

---

## What it is

Two things in one binary:

1. **A real terminal** — a full VT emulator on top of
   [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal), with
   tabs, resizable splits, scrollback, search, selection and mouse reporting.
   Native rendering with [egui](https://github.com/emilk/egui): ~40-80 MB of RAM,
   not the 300-500 of an Electron app.
2. **An agent manager** — a side panel that discovers the sessions your agents
   already store on disk and reopens them with one click, with the metadata none
   of them gives you: project, branch, model, cost, context %, quota and live
   status.

## Why it exists

The panel started as a fork of Warp's session manager, and before that Terax's.
Both are huge, very active repos: keeping the fork alive meant **perpetual rebase
debt**. `aterm` inverts the relationship — instead of forking a terminal, it
**embeds a terminal emulator as a library** and adds the panel on top. The result
is ours, minimal and 100 % editable.

## What works today

**Terminal**

- Real PTY + `Term` + `EventLoop` from `alacritty_terminal` 0.25 (16/256/truecolor
  ANSI palette, styles, cursor, scrollback).
- **Tabs** (create/close/reorder by drag & drop, `Ctrl+Tab`, `Alt+1..9`,
  `Ctrl+Shift+T` reopens the last one in its **live cwd**) and grid **splits**
  with draggable dividers.
- **Persistence across restarts**: on launch it restores the tabs you had (argv +
  live cwd) from `~/.config/aterm/session.json`.
- Mouse selection, copy/paste (`Ctrl+Shift+C/V`, middle click on X11),
  **context menu**, font zoom, **scrollback search** (`Ctrl+Shift+F`) with every
  match highlighted.
- VT fidelity: SGR/X10 mouse reporting, bracketed paste, alt-scroll — aggressive
  TUIs (claude, codex) behave.
- **11 switchable themes** (Nexus by default, Catppuccin Mocha/Latte, Tokyo
  Night, Dracula, Nord, Gruvbox, Solarized, One Dark, Rosé Pine, Monokai) plus an
  optional **HUD** layer (background lattice, scanlines) you can turn off in
  Settings.

**Session manager**

- Scans **7 providers** by reading their native formats; cards with model,
  branch, context %, message count and relative time.
- **Predicate filters** and **full-text search inside** the conversations.
- Grouping by **provider / project / cascade / your own groups**, with per-project
  alias and colour.
- Conversation **preview**; rename, tags, colour, notes, favourites and icons.
- **Launch**: new session in the directory you pick, across several projects at
  once, recommended agent for the cwd, **templates** and **project commands**
  (slash-commands from `.claude/commands/**` + scripts from
  `package.json`/`Makefile`/`justfile`/`Cargo`).
- **Compact context** (`/compact`, Claude), **move** a session to another project,
  **delete** (one, a selection, or by age).
- **Export/import** `.zip`, byte-compatible with the extension and multi-claude,
  plus **backup/restore** of the catalogue.
- Provider **quota**, **service status** (statuspage) and configurable
  notifications when a session finishes or goes idle.

Everything is local: it reads from your `$HOME`, no server involved. Metadata
lives in `~/.config/aterm/` and is **the same the VS Code extension uses**, so
both front-ends see each other's changes.

## Building

```bash
cargo run -p aterm            # start the app
cargo build --release         # optimised binary (lto thin)
cargo check                   # quick workspace validation
```

Requirements: stable Rust (edition 2021). The shell's live cwd is resolved via
`/proc/<pid>/cwd`, so directory restoration is a Linux-only nicety; everything
else is cross-platform.

## Tests

```bash
cargo test --workspace
```

**73** in the `agent-sessions` core (parsers for the 7 providers, metadata,
transfer) and **27** in the `aterm` crate — terminal e2e over a real PTY (child
output, keyboard echo, exit code), URL detection, SGR/X10 mouse, and session and
status helpers.

## Layout

```
crates/
├── agent-sessions/       # core: session discovery (read-only)
│   └── src/providers/    #   claude · codex · opencode · gemini · qwen · goose · factory
├── agent-sessions-cli/   # sidecar: the core as JSON on stdout, + MCP server
├── aterm/                # the app
│   ├── app.rs            #   chrome, tabs, splits, mouse/keyboard
│   ├── sessions.rs       #   the session panel
│   ├── term/             #   PTY + grid → egui (mod · render · input)
│   └── …                 #   theme · settings · persist · groups · templates · …
└── aterm-pro-api/        # open-core contract (ProHost / ProModule traits)
```

`agent-sessions` is **read-only by design** for sessions: every provider derives
its paths from `$HOME` and never accepts caller-supplied paths. It does write
metadata under `~/.config/aterm/**`, and into `~/.claude/projects/**` on import.

## The sidecar (`agent-sessions-cli`)

Wraps the core and emits JSON on stdout, so any front-end can use it without
linking Rust. This is what the VS Code extension consumes:

```bash
agent-sessions-cli scan               # every session, as JSON
agent-sessions-cli preview claude <id>
agent-sessions-cli resume-argv claude <id>
agent-sessions-cli search-content "rollout parser"
```

Commands: `scan`, `providers`, `preview`, `transcript`, `resume-argv`,
`new-argv`, `compact-argv`, `metadata-{get,set,clear}`,
`projects-{get,set,clear}`, `export`, `import`, `archive`, `unarchive`,
`archive-restore`, `delete`, `move`, `backup`, `restore`, `service-status`,
`live`, `search-content`, `templates-{get,set,delete}`, `serve`, plus the
shared-session `remote-*` family (`remote-config`, `remote-server-set`,
`remote-server-delete`, `remote-links`, `remote-links-set`, `remote-global-set`,
`remote-probe`, `remote-list`, `remote-plan`, `remote-publish`, `remote-fetch`,
`remote-unpublish`, `remote-shared`).

### Shared sessions

Publish a session to a repository your team reads, and pull someone else's in,
keeping its id. Local-first: the repository (a folder, git over SSH, or the
GitLab/GitHub API) is just a store, and the session is written back into the
layout its provider expects. The engine lives in the `agent-sessions-remote`
crate and both front-ends consume it; the design, known limits and how to try it
without configuring anything are in
[`docs/REMOTE-SESSIONS.md`](docs/REMOTE-SESSIONS.md).

```bash
export ATERM_REMOTE_DIR=/tmp/session-repo        # try it with zero config
agent-sessions-cli remote-plan claude <id>       # which files would go up
agent-sessions-cli remote-publish "$PWD" team claude <id>
agent-sessions-cli remote-list "$PWD" team
```

### MCP server

`agent-sessions-cli serve` speaks JSON-RPC 2.0 over stdio (MCP protocol
2024-11-05) and exposes your history **to the agent itself**: `list_sessions`,
`get_session_turns`, `search_sessions`. With this, Claude Code can search its own
past conversations instead of you pasting them in.

```json
{
  "mcpServers": {
    "agent-sessions": {
      "command": "/path/to/agent-sessions-cli",
      "args": ["serve"]
    }
  }
}
```

## Pro edition

The project is **open-core**. This repo is the **Community** edition (MIT) and is
fully functional:

```bash
cargo run -p aterm            # Community edition, everything above
```

The advanced features — **parallel compare** (the same prompt to N agents, each in
its own git worktree, with a comparison report and cleanup), **workspace
profiles**, an **advanced dashboard** with CSV export, **export conversation to
HTML**, **port a session to another provider**, **memory graph** and **one-click
MCP server setup** — are **Pro**, and their code lives in the private `aterm-pro`
repo, which produces the official binary.

In this checkout, Pro actions explain they need that edition. The seam is public
and auditable: the contract in
[`crates/aterm-pro-api`](./crates/aterm-pro-api), the stubs in
[`crates/aterm/src/pro.rs`](./crates/aterm/src/pro.rs) and the `pro` feature flag
in `crates/aterm/Cargo.toml` (with a placeholder crate at `crates/aterm-pro`,
because Cargo requires the manifest of every `path` dependency even when it's
disabled).

There is a **14-day trial**; after that, a licence (Lemon Squeezy). Details and
pricing at [atermlabs.jesuslorenzo.es](https://atermlabs.jesuslorenzo.es).

## Related repos (org `Aterm-labs`)

- [`agent-sessions`](https://github.com/Aterm-labs/agent-sessions) — the **VS Code
  extension**: the same manager inside the editor (and Cursor, VSCodium,
  Windsurf). Consumes this repo as a submodule for the sidecar.
- [`aterm-workspace`](https://github.com/Aterm-labs/aterm-workspace) — meta-repo
  grouping everything as submodules, with the shared tooling.
- [`aterm-web`](https://github.com/Aterm-labs/aterm-web) — the landing page.

## Licence

MIT.
