<div align="center">

# aterm

**Terminal nativo en Rust con un gestor de sesiones de agentes dentro.**

Lista, previsualiza y reanuda tus conversaciones de Claude Code, Codex,
OpenCode, Gemini CLI, Qwen Code, Goose y Factory Droid — sin salir del terminal
y sin depender de un editor.

[![Licencia](https://img.shields.io/badge/licencia-MIT-e0af68)](#licencia)
[![Rust](https://img.shields.io/badge/rust-edition%202021-e0883b)](#compilar)
[![Tests](https://img.shields.io/badge/tests-verdes-6bd089)](#tests)

[Web](https://atermlabs.jesuslorenzo.es) ·
[Extensión de VS Code](https://github.com/Aterm-labs/agent-sessions) ·
[English](./README.en.md)

</div>

---

## Qué es

Dos cosas en un binario:

1. **Un terminal de verdad** — emulador VT completo sobre
   [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) con
   pestañas, splits redimensionables, scrollback, búsqueda, selección y reporte
   de ratón. Render nativo con [egui](https://github.com/emilk/egui): ~40-80 MB
   de RAM, no 300-500 como una app Electron.
2. **Un gestor de agentes** — un panel lateral que descubre las sesiones que tus
   agentes ya guardan en disco y las reabre con un clic, con la metadata que
   ninguno de ellos te da: proyecto, rama, modelo, coste, % de contexto, cuota y
   estado en vivo.

## Por qué existe

El panel nació como fork del gestor de sesiones de Warp y antes de Terax. Los dos
son repos enormes y muy activos: mantener el fork era **deuda de rebase
perpetua**. `aterm` invierte la relación — en vez de forkear un terminal,
**embebe un emulador de terminal como librería** y le añade el panel encima. El
resultado es propio, mínimo y 100 % editable.

## Qué funciona hoy

**Terminal**

- PTY real + `Term` + `EventLoop` de `alacritty_terminal` 0.25 (paleta ANSI
  16/256/truecolor, estilos, cursor, scrollback).
- **Pestañas** (crear/cerrar/reordenar por drag & drop, `Ctrl+Tab`, `Alt+1..9`,
  `Ctrl+Shift+T` reabre la última en su **cwd real**) y **splits** en rejilla con
  divisores arrastrables.
- **Persistencia entre arranques**: al abrir, recupera las pestañas que tenías
  (argv + cwd vivo) desde `~/.config/aterm/session.json`.
- Selección con ratón, copy/paste (`Ctrl+Shift+C/V`, clic central en X11),
  **menú contextual**, zoom de fuente, **búsqueda en el scrollback**
  (`Ctrl+Shift+F`) con todas las coincidencias resaltadas.
- Fidelidad VT: reporte de ratón SGR/X10, bracketed paste, alt-scroll — las TUIs
  agresivas (claude, codex) se comportan.
- **11 temas** conmutables (Nexus por defecto, Catppuccin Mocha/Latte, Tokyo
  Night, Dracula, Nord, Gruvbox, Solarized, One Dark, Rosé Pine, Monokai) y una
  capa **HUD** opcional (rejilla de fondo, scanlines) que se apaga en Ajustes.

**Gestor de sesiones**

- Escaneo de los **7 proveedores** leyendo sus formatos nativos; tarjetas con
  modelo, rama, % de contexto, mensajes y tiempo relativo.
- **Filtros** con predicados y **búsqueda dentro del contenido** de las
  conversaciones (FTS).
- Agrupación por **proveedor / proyecto / cascada / grupo propio**, con alias y
  color por proyecto.
- **Preview** de la conversación; renombrar, etiquetas, color, notas, favoritos
  e iconos.
- **Lanzar**: nueva sesión eligiendo directorio, en varios proyectos a la vez,
  agente recomendado según el cwd, **plantillas** y **comandos del proyecto**
  (slash-commands de `.claude/commands/**` + scripts de
  `package.json`/`Makefile`/`justfile`/`Cargo`).
- **Compactar contexto** (`/compact`, Claude), **mover** una sesión a otro
  proyecto, **borrar** (una, por selección o por antigüedad).
- **Export/import** `.zip` byte-compatible con la extensión y con multi-claude, y
  **backup/restore** del catálogo.
- **Cuota** del proveedor, **estado del servicio** (statuspage) y avisos
  configurables cuando una sesión termina o se queda esperando.

Todo local: se lee de tu `$HOME`, sin servidor de por medio. La metadata vive en
`~/.config/aterm/` y es **la misma que usa la extensión de VS Code**, así que las
dos interfaces se ven los cambios entre sí.

## Compilar

```bash
cargo run -p aterm            # arrancar la app
cargo build --release         # binario optimizado (lto thin)
cargo check                   # validación rápida del workspace
```

Requisitos: Rust estable (edition 2021). El cwd vivo de la shell se resuelve vía
`/proc/<pid>/cwd`, así que la restauración de directorio es una mejora exclusiva
de Linux; el resto es multiplataforma.

## Tests

```bash
cargo test --workspace
```

**73** en el núcleo `agent-sessions` (parsers de los 7 proveedores, metadata,
transfer) y **27** en el crate `aterm` — e2e del terminal sobre un PTY real
(salida del hijo, echo de teclado, exit code), detección de URLs, ratón SGR/X10 y
helpers de sesión y estado.

## Estructura

```
crates/
├── agent-sessions/       # núcleo: descubrimiento de sesiones (read-only)
│   └── src/providers/    #   claude · codex · opencode · gemini · qwen · goose · factory
├── agent-sessions-cli/   # sidecar: el núcleo como JSON por stdout, + servidor MCP
├── aterm/                # la app
│   ├── app.rs            #   chrome, pestañas, splits, ratón/teclado
│   ├── sessions.rs       #   el panel de sesiones
│   ├── term/             #   PTY + grid → egui (mod · render · input)
│   └── …                 #   theme · settings · persist · groups · templates · …
└── aterm-pro-api/        # contrato open-core (traits ProHost / ProModule)
```

`agent-sessions` es **read-only por diseño** para las sesiones: cada proveedor
deriva sus rutas del `$HOME` y nunca acepta paths del llamante. Sí escribe
metadata en `~/.config/aterm/**` y en `~/.claude/projects/**` al importar.

## El sidecar (`agent-sessions-cli`)

Envuelve el núcleo y emite JSON por stdout, para que cualquier front-end pueda
usarlo sin enlazar Rust. Es lo que consume la extensión de VS Code:

```bash
agent-sessions-cli scan               # todas las sesiones, JSON
agent-sessions-cli preview claude <id>
agent-sessions-cli resume-argv claude <id>
agent-sessions-cli search-content "parser de rollouts"
```

Comandos: `scan`, `providers`, `preview`, `transcript`, `resume-argv`,
`new-argv`, `compact-argv`, `metadata-{get,set,clear}`,
`projects-{get,set,clear}`, `export`, `import`, `archive`, `unarchive`,
`archive-restore`, `delete`, `move`, `backup`, `restore`, `service-status`,
`live`, `search-content`, `templates-{get,set,delete}`, `serve`.

### Servidor MCP

`agent-sessions-cli serve` habla JSON-RPC 2.0 sobre stdio (protocolo MCP
2024-11-05) y expone el historial **al propio agente**: `list_sessions`,
`get_session_turns`, `search_sessions`. Con esto, Claude Code puede buscar en sus
propias conversaciones pasadas en vez de que se las pegues tú.

```json
{
  "mcpServers": {
    "agent-sessions": {
      "command": "/ruta/a/agent-sessions-cli",
      "args": ["serve"]
    }
  }
}
```

## Edición Pro

El proyecto es **open-core**. Este repo es la edición **Community** (MIT) y es
completamente funcional:

```bash
cargo run -p aterm            # edición Community, todo lo de arriba
```

Las funciones avanzadas —**comparativa paralela** (el mismo prompt a N agentes,
cada uno en su git worktree, con informe comparativo y limpieza), **perfiles de
espacio de trabajo**, **dashboard avanzado** con export CSV, **exportar
conversación a HTML**, **portar una sesión a otro proveedor**, **gráfico de
memoria** y **configurar el servidor MCP** con un clic— son **Pro**, y su código
vive en el repo privado `aterm-pro`, que produce el binario oficial.

En este checkout, las acciones Pro explican que necesitan esa edición. El seam es
público y auditable: el contrato en
[`crates/aterm-pro-api`](./crates/aterm-pro-api), los stubs en
[`crates/aterm/src/pro.rs`](./crates/aterm/src/pro.rs) y el feature flag `pro` en
`crates/aterm/Cargo.toml` (con un crate placeholder en `crates/aterm-pro`, porque
Cargo exige el manifest de toda dependencia `path` aunque esté desactivada).

Hay **14 días de prueba**; luego licencia (Lemon Squeezy). Detalles y precios en
[atermlabs.jesuslorenzo.es](https://atermlabs.jesuslorenzo.es).

## Repos relacionados (org `Aterm-labs`)

- [`agent-sessions`](https://github.com/Aterm-labs/agent-sessions) — la
  **extensión de VS Code**: el mismo gestor dentro del editor (y de Cursor,
  VSCodium, Windsurf). Consume este repo como submódulo para el sidecar.
- [`aterm-workspace`](https://github.com/Aterm-labs/aterm-workspace) — meta-repo
  que agrupa todo como submódulos y centraliza el tooling.
- [`aterm-web`](https://github.com/Aterm-labs/aterm-web) — la landing.

## Licencia

MIT.
