//! Nexus HUD chrome — the cyberpunk framing that wraps aterm's real
//! functionality: a faint background grid, an optional scanline sweep, accent
//! corner brackets, the session banner and the bottom status bar. These are
//! pure drawing helpers driven by the active [`theme`] palette; they add look,
//! not behaviour.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Color32, Stroke};

use crate::theme;

/// Grid cell size, mirroring the design's 44px lattice.
const GRID: f32 = 44.0;

/// Local wall-clock as `HH:MM:SS`. The UTC→local offset is resolved once (via
/// `date +%z`) and cached, so per-frame calls are just arithmetic.
pub fn clock() -> String {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    let off = *OFFSET.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| parse_utc_offset(s.trim()))
            .unwrap_or(0)
    });
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let t = (now + off).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

/// Parse an RFC-822 zone like `+0200` / `-0530` into seconds east of UTC.
fn parse_utc_offset(z: &str) -> Option<i64> {
    let b = z.as_bytes();
    if b.len() < 5 {
        return None;
    }
    let sign = match b[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let h: i64 = z.get(1..3)?.parse().ok()?;
    let m: i64 = z.get(3..5)?.parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// A provider's display label + brand accent, derived from a tab's session key
/// (`provider:id`) or, failing that, its launch argv.
pub fn provider_of(key: Option<&str>, argv: &[String]) -> (&'static str, Color32) {
    let p = theme::pal();
    let id = key
        .and_then(|k| k.split(':').next())
        .map(str::to_owned)
        .or_else(|| {
            argv.first().map(|a| {
                a.rsplit('/')
                    .next()
                    .unwrap_or(a)
                    .split_whitespace()
                    .next()
                    .unwrap_or(a)
                    .to_owned()
            })
        })
        .unwrap_or_default();
    match id.as_str() {
        "claude" => ("CLAUDE CODE", p.green),
        "codex" => ("CODEX", p.blue),
        "gemini" => ("GEMINI", p.mauve),
        "opencode" => ("OPENCODE", p.sapphire),
        "" => ("TERMINAL", p.overlay),
        other if other.contains("sh") || other == "fish" || other == "nu" => ("SHELL", p.overlay),
        _ => ("TERMINAL", p.overlay),
    }
}

/// Paint the faint background lattice over `rect` (drawn on the given painter's
/// current layer, so call it *after* the terminal so it reads as an overlay).
pub fn grid(painter: &egui::Painter, rect: egui::Rect) {
    let c = Color32::from_rgba_unmultiplied(120, 180, 220, 7);
    let stroke = Stroke::new(1.0, c);
    let mut x = rect.left();
    while x <= rect.right() {
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], stroke);
        x += GRID;
    }
    let mut y = rect.top();
    while y <= rect.bottom() {
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], stroke);
        y += GRID;
    }
}

/// Paint the animated scanline sweep. `phase` is a 0..1 progress value; the
/// band eases down the rect and wraps.
pub fn scanline(painter: &egui::Painter, rect: egui::Rect, phase: f32) {
    let accent = theme::pal().green;
    let band = 130.0_f32;
    let travel = rect.height() + band;
    let center = rect.top() - band * 0.5 + phase.fract() * travel;
    let slices = 14;
    for i in 0..slices {
        let f = i as f32 / (slices - 1) as f32; // 0..1 across the band
        let y = center - band * 0.5 + f * band;
        if y < rect.top() || y > rect.bottom() {
            continue;
        }
        // Triangular falloff → brightest in the middle of the band.
        let intensity = 1.0 - (f - 0.5).abs() * 2.0;
        let a = (intensity * 22.0) as u8;
        let col = Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), a);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(band / slices as f32 + 1.0, col),
        );
    }
}

/// Draw the four accent corner brackets just inside `rect`.
pub fn corner_brackets(painter: &egui::Painter, rect: egui::Rect) {
    let col = theme::pal().green.gamma_multiply(0.5);
    let s = Stroke::new(1.0, col);
    let n = 14.0; // arm length
    let m = 10.0; // inset from the edge
    let r = rect.shrink(m);
    let (l, t, ri, b) = (r.left(), r.top(), r.right(), r.bottom());
    // top-left
    painter.line_segment([egui::pos2(l, t), egui::pos2(l + n, t)], s);
    painter.line_segment([egui::pos2(l, t), egui::pos2(l, t + n)], s);
    // top-right
    painter.line_segment([egui::pos2(ri, t), egui::pos2(ri - n, t)], s);
    painter.line_segment([egui::pos2(ri, t), egui::pos2(ri, t + n)], s);
    // bottom-left
    painter.line_segment([egui::pos2(l, b), egui::pos2(l + n, b)], s);
    painter.line_segment([egui::pos2(l, b), egui::pos2(l, b - n)], s);
    // bottom-right
    painter.line_segment([egui::pos2(ri, b), egui::pos2(ri - n, b)], s);
    painter.line_segment([egui::pos2(ri, b), egui::pos2(ri, b - n)], s);
}

/// A glowing accent orb (concentric circles), sized `d`, centred at `c`.
pub fn orb(painter: &egui::Painter, c: egui::Pos2, d: f32, col: Color32) {
    painter.circle_filled(c, d * 0.5, col.gamma_multiply(0.16));
    painter.circle_filled(c, d * 0.34, col.gamma_multiply(0.4));
    painter.circle_filled(c, d * 0.22, col);
}

/// Summary of the focused terminal, rendered in the session banner.
pub struct SessionInfo {
    pub accent: Color32,
    pub provider: String,
    pub title: String,
    pub cwd: String,
    pub status: String,
    pub status_col: Color32,
}

/// The session banner strip: accent dot · provider · title · cwd · status pill.
pub fn banner(ui: &mut egui::Ui, info: &SessionInfo) {
    let p = theme::pal();
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
        orb(ui.painter(), rect.center(), 9.0, info.accent);
        ui.add_space(4.0);
        ui.label(theme::hud(&info.provider, 12.5).color(p.text));
        if !info.title.is_empty() {
            ui.label(egui::RichText::new(&info.title).size(11.0).color(p.overlay));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(18.0);
            theme::pill(ui, &info.status, info.status_col.gamma_multiply(0.22), info.status_col);
            if !info.cwd.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(&info.cwd)
                        .size(11.0)
                        .color(p.overlay.gamma_multiply(0.9)),
                );
            }
        });
    });
}

/// The bottom status bar: CONECTADO · N terminales · UTF-8 · … · version.
pub fn status_bar(ui: &mut egui::Ui, tabs: usize, running: usize) {
    let p = theme::pal();
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 2.5, p.green);
        ui.label(theme::hud("CONECTADO", 9.5).color(p.green));
        ui.add_space(10.0);
        ui.label(theme::hud(&format!("{tabs} TERMINALES"), 9.5).color(p.overlay));
        if running > 0 {
            ui.add_space(10.0);
            ui.label(theme::hud(&format!("{running} ACTIVOS"), 9.5).color(p.blue));
        }
        ui.add_space(10.0);
        ui.label(theme::hud("UTF-8", 9.5).color(p.overlay));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(24.0);
            ui.label(
                theme::hud(concat!("ATERM v", env!("CARGO_PKG_VERSION")), 9.5)
                    .color(p.overlay.gamma_multiply(0.8)),
            );
        });
    });
}
