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
    /// Theme declares `mode = "light"`.
    pub light: bool,
    /// Label color for text on an `accent` fill.
    pub on_accent: Color32,
    /// Second square of the transparency checkerboard, over `sunken`.
    pub checker: Color32,
}

/// Enough for the checkerboard to read as a pattern without competing with
/// the avatar drawn over it.
pub const MIN_CHECKER_CONTRAST: f32 = 1.25;

/// WCAG 2 AA contrast for normal-size text.
pub const MIN_TEXT_CONTRAST: f32 = 4.5;

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
            light: false,
            on_accent: Color32::from_rgb(0x14, 0x15, 0x18),
            checker: Color32::from_rgb(0x1f, 0x21, 0x25),
        }
    }
}

impl Palette {
    /// Adjusts the colors used for text so each reaches `MIN_TEXT_CONTRAST`
    /// on the background. Many Omarchy themes define `muted` for borders and
    /// inactive UI, at 1.5–3:1, which is unreadable as secondary text.
    fn readable(mut self) -> Self {
        let bg = self.background;
        let fg = self.foreground;
        // Theme `red/yellow/green` are terminal colors chosen for looks; some
        // themes swap them (matte-black: yellow = #b91c1c). Status meaning
        // needs the expected hue, so keep a theme color only within range.
        let fallback = Palette::default();
        for (c, hues, standard) in [
            (&mut self.red, (330.0, 20.0), fallback.red),
            (&mut self.yellow, (30.0, 65.0), fallback.yellow),
            (&mut self.green, (85.0, 170.0), fallback.green),
        ] {
            if !hue_in(*c, hues) {
                *c = standard;
            }
        }
        self.muted = toward_contrast(self.muted, bg, fg, MIN_TEXT_CONTRAST);
        // Themes often make dark_background and lighter_background almost equal
        // (catppuccin-latte: 1.03:1), which hides the checkerboard.
        self.checker = toward_contrast(self.sunken, self.sunken, fg, MIN_CHECKER_CONTRAST);
        // Status colors darken or lighten instead: mixing with black or white
        // keeps the hue, mixing with a tinted foreground may not.
        let extreme = if self.light {
            Color32::BLACK
        } else {
            Color32::WHITE
        };
        for c in [&mut self.red, &mut self.yellow, &mut self.green] {
            *c = toward_contrast(*c, bg, extreme, MIN_TEXT_CONTRAST);
        }
        self.on_accent = [bg, fg, Color32::BLACK, Color32::WHITE]
            .into_iter()
            .find(|&c| contrast(c, self.accent) >= MIN_TEXT_CONTRAST)
            .unwrap_or_else(|| {
                if contrast(Color32::BLACK, self.accent) > contrast(Color32::WHITE, self.accent) {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                }
            });
        self
    }
}

/// Whether the color's hue, in degrees, lies in `(from, to)`, wrapping at 360.
/// Near-gray colors have no meaningful hue and never match.
fn hue_in(c: Color32, (from, to): (f32, f32)) -> bool {
    let (r, g, b) = (c.r() as f32, c.g() as f32, c.b() as f32);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min < 24.0 {
        return false;
    }
    let d = max - min;
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if from <= to {
        (from..=to).contains(&h)
    } else {
        h >= from || h <= to
    }
}

fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG 2 contrast ratio, 1–21.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Mixes `color` toward `target` in 5% steps until it reaches `min` contrast
/// on `bg`; keeps the hue as long as possible.
fn toward_contrast(color: Color32, bg: Color32, target: Color32, min: f32) -> Color32 {
    let mix = |t: f32| {
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color32::from_rgb(
            l(color.r(), target.r()),
            l(color.g(), target.g()),
            l(color.b(), target.b()),
        )
    };
    (0..=20)
        .map(|i| mix(i as f32 / 20.0))
        .find(|&c| contrast(c, bg) >= min)
        .unwrap_or(target)
}

pub fn omarchy_colors_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/omarchy/current/theme/colors.toml"))
}

pub fn load(path: Option<&Path>) -> Palette {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| parse(&text))
        .unwrap_or_else(|| Palette::default().readable())
}

/// Reads the keys we use; unknown keys and malformed lines are ignored.
pub fn parse(text: &str) -> Palette {
    let mut p = Palette::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "mode" {
            p.light = value.trim().trim_matches('"') == "light";
            continue;
        }
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
    p.readable()
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
    let mut v = if p.light {
        Visuals::light()
    } else {
        Visuals::dark()
    };
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
            Palette::default().readable()
        );
    }

    #[test]
    fn swapped_status_colors_fall_back_to_expected_hues() {
        let p = parse(
            "background = \"#121212\"\nforeground = \"#bebebe\"\nyellow = \"#b91c1c\"\ngreen = \"#FFC107\"\nred = \"#D35F5F\"\n",
        );
        assert!(hue_in(p.yellow, (30.0, 65.0)));
        assert!(hue_in(p.green, (85.0, 170.0)));
        assert!(hue_in(p.red, (330.0, 20.0)));
    }

    #[test]
    fn hue_ranges() {
        assert!(
            hue_in(Color32::from_rgb(255, 0, 0), (330.0, 20.0)),
            "wraps at 360"
        );
        assert!(hue_in(Color32::from_rgb(0, 200, 0), (85.0, 170.0)));
        assert!(
            !hue_in(Color32::from_rgb(128, 128, 128), (0.0, 360.0)),
            "gray has no hue"
        );
    }

    #[test]
    fn contrast_matches_wcag_reference_values() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.01);
        // #767676 on white is the classic 4.54:1 AA boundary.
        assert!(
            (contrast(Color32::from_rgb(0x76, 0x76, 0x76), Color32::WHITE) - 4.54).abs() < 0.02
        );
    }

    #[test]
    fn low_contrast_muted_is_raised_and_mode_is_read() {
        // catppuccin-latte: muted #acb0be on #eff1f5 is 1.9:1.
        let p = parse(
            "mode = \"light\"\nbackground = \"#eff1f5\"\nforeground = \"#4c4f69\"\nmuted = \"#acb0be\"\naccent = \"#1e66f5\"\n",
        );
        assert!(p.light);
        assert!(contrast(p.muted, p.background) >= MIN_TEXT_CONTRAST);
        assert_ne!(p.muted, p.foreground, "should stay distinct from body text");
        assert!(contrast(p.on_accent, p.accent) >= MIN_TEXT_CONTRAST);
    }

    /// Every theme shipped with Omarchy yields readable text roles. Skipped
    /// where Omarchy is not installed.
    #[test]
    fn installed_omarchy_themes_have_readable_text() {
        let Ok(dirs) = std::fs::read_dir("/usr/share/omarchy/themes") else {
            return;
        };
        let mut checked = 0;
        for dir in dirs.flatten() {
            let Ok(text) = std::fs::read_to_string(dir.path().join("colors.toml")) else {
                continue;
            };
            let p = parse(&text);
            let name = dir.file_name();
            for (role, c) in [
                ("fg", p.foreground),
                ("muted", p.muted),
                ("red", p.red),
                ("yellow", p.yellow),
                ("green", p.green),
            ] {
                assert!(
                    contrast(c, p.background) >= MIN_TEXT_CONTRAST,
                    "{name:?} {role}"
                );
            }
            assert!(
                contrast(p.on_accent, p.accent) >= MIN_TEXT_CONTRAST,
                "{name:?} on_accent"
            );
            assert!(hue_in(p.red, (330.0, 25.0)), "{name:?} red hue");
            assert!(hue_in(p.yellow, (25.0, 70.0)), "{name:?} yellow hue");
            assert!(hue_in(p.green, (80.0, 175.0)), "{name:?} green hue");
            assert!(
                contrast(p.checker, p.sunken) >= MIN_CHECKER_CONTRAST,
                "{name:?} checker"
            );
            checked += 1;
        }
        assert!(checked > 0);
    }
}
