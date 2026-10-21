//! Colors from the active Omarchy theme, applied to egui.
//!
//! Omarchy keeps the current theme at
//! `~/.local/state/omarchy/current/theme/colors.toml` as flat `key = "#rrggbb"`
//! lines. Missing keys or a missing file fall back to a dark palette.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, Visuals};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub background: Color32,
    pub surface: Color32,
    pub sunken: Color32,
    pub foreground: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub selection: Color32,
    pub red: Color32,
    pub yellow: Color32,
    pub green: Color32,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            background: Color32::from_rgb(0x14, 0x15, 0x18),
            surface: Color32::from_rgb(0x1f, 0x21, 0x25),
            sunken: Color32::from_rgb(0x0b, 0x0c, 0x0e),
            foreground: Color32::from_rgb(0xe6, 0xe8, 0xeb),
            muted: Color32::from_rgb(0x80, 0x86, 0x8f),
            accent: Color32::from_rgb(0x5a, 0xb4, 0xff),
            selection: Color32::from_rgb(0x1e, 0x3a, 0x55),
            red: Color32::from_rgb(0xff, 0x5c, 0x6c),
            yellow: Color32::from_rgb(0xf5, 0xc5, 0x42),
            green: Color32::from_rgb(0x5f, 0xd3, 0x8d),
        }
    }
}

pub fn omarchy_colors_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/omarchy/current/theme/colors.toml"))
}

pub fn load(path: Option<&Path>) -> Palette {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Reads the keys we use; unknown keys and malformed lines are ignored.
pub fn parse(text: &str) -> Palette {
    let mut p = Palette::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(color) = hex(value.trim().trim_matches('"')) else {
            continue;
        };
        let slot = match key.trim() {
            "background" => &mut p.background,
            "lighter_background" => &mut p.surface,
            "dark_background" => &mut p.sunken,
            "foreground" => &mut p.foreground,
            "muted" => &mut p.muted,
            "accent" => &mut p.accent,
            "selection" => &mut p.selection,
            "red" => &mut p.red,
            "yellow" => &mut p.yellow,
            "green" => &mut p.green,
            _ => continue,
        };
        *slot = color;
    }
    p
}

fn hex(s: &str) -> Option<Color32> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn apply(ctx: &egui::Context, p: &Palette) {
    let mut v = Visuals::dark();
    v.override_text_color = Some(p.foreground);
    v.panel_fill = p.background;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.sunken;
    v.faint_bg_color = p.surface;
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.red;
    v.warn_fg_color = p.yellow;
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    let radius = CornerRadius::same(6);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.surface);
    v.widgets.inactive.weak_bg_fill = p.surface;
    // Text fields and drag values: sunken, so they read as inputs on panels.
    v.widgets.inactive.bg_fill = p.sunken;
    v.widgets.hovered.weak_bg_fill = p.selection;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.weak_bg_fill = p.selection;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    ctx.set_visuals(v);

    ctx.all_styles_mut(|style| {
        use egui::FontFamily::{Monospace, Proportional};
        style.text_styles = [
            (TextStyle::Heading, FontId::new(18.0, Proportional)),
            (TextStyle::Body, FontId::new(15.0, Proportional)),
            (TextStyle::Button, FontId::new(15.0, Proportional)),
            (TextStyle::Small, FontId::new(12.5, Proportional)),
            (TextStyle::Monospace, FontId::new(14.0, Monospace)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_omarchy_keys_and_ignores_the_rest() {
        let p = parse(
            "mode = \"dark\"\naccent = \"#3ee8ff\"\n# comment\nbackground = \"#08090a\"\nred=\"#ff3b5c\"\nmuted = \"bad\"\n",
        );
        assert_eq!(p.accent, Color32::from_rgb(0x3e, 0xe8, 0xff));
        assert_eq!(p.background, Color32::from_rgb(0x08, 0x09, 0x0a));
        assert_eq!(p.red, Color32::from_rgb(0xff, 0x3b, 0x5c));
        assert_eq!(p.muted, Palette::default().muted);
    }

    #[test]
    fn missing_file_gives_default() {
        assert_eq!(
            load(Some(Path::new("/nonexistent/colors.toml"))),
            Palette::default()
        );
    }
}
