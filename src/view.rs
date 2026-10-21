//! The window's contents, drawn from plain state and returning user actions.
//!
//! Keeping this free of I/O lets tests render it offscreen with fake status.

use std::path::PathBuf;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, Vec2, pos2, vec2,
};
use uuid::Uuid;

use crate::mouth::{GateParams, Mouth};
use crate::obs::{Connection, Status};
use crate::theme::Palette;

/// Gap between the open and close thresholds, in dB.
pub const HYSTERESIS_DB: f32 = 8.0;
/// Left end of the level meter.
pub const FLOOR_DB: f32 = -60.0;
/// Level fed to the gate while "Testar" is held.
pub const TEST_DB: f32 = -6.0;
const CARD_MAX: f32 = 300.0;

#[derive(Default)]
pub struct SlotView {
    pub file: Option<String>,
    pub size: Option<[u32; 2]>,
    pub texture: Option<TextureHandle>,
    pub error: Option<String>,
}

pub struct ViewState {
    pub palette: Palette,
    pub obs_port: u16,
    pub obs_password: String,
    pub selected: Option<Uuid>,
    pub params: GateParams,
    pub testing: bool,
    pub slots: [SlotView; 2],
    pub picking: bool,
    pub server: Result<String, String>,
    pub prefs_error: Option<String>,
    pub show_connection: bool,
    /// UI time at which the URL was copied, for a short confirmation.
    pub copied_at: Option<f64>,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Connect,
    Disconnect,
    Select(Option<Uuid>),
    SetParams(GateParams),
    Test(bool),
    Pick(Mouth),
    Dropped(Mouth, PathBuf),
    RetryServer,
}

pub fn draw(ui: &mut egui::Ui, s: &mut ViewState, status: &Status) -> Vec<Action> {
    let mut actions = Vec::new();
    let frame = egui::Frame::central_panel(ui.style()).inner_margin(16);
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            status_strip(ui, s, status, &mut actions);
            if s.show_connection {
                connection_panel(ui, s, status, &mut actions);
            }
            ui.add_space(8.0);
            stage(ui, s, status.mouth, &mut actions);
            ui.add_space(12.0);
            audio(ui, s, status, &mut actions);
            if let Some(e) = &s.prefs_error {
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(e.as_str()).color(s.palette.yellow))
                        .wrap(),
                );
            }
        });
    });
    actions
}

// ---------------------------------------------------------------- status strip

#[derive(Clone, Copy)]
enum Health {
    Ok,
    Pending,
    Bad,
    Action,
}

fn status_strip(ui: &mut egui::Ui, s: &mut ViewState, status: &Status, actions: &mut Vec<Action>) {
    let p = s.palette;
    ui.horizontal_wrapped(|ui| {
        let (health, text) = match &status.connection {
            Connection::Connected => (Health::Ok, "OBS conectado".to_owned()),
            Connection::Connecting => (Health::Pending, "Conectando ao OBS…".to_owned()),
            Connection::Idle => (Health::Pending, "OBS desconectado".to_owned()),
            Connection::Retrying { after, .. } => (
                Health::Bad,
                format!("OBS fora do ar · tentando em {} s", after.as_secs()),
            ),
            Connection::Failed(_) => (Health::Bad, "OBS recusou a conexão".to_owned()),
        };
        if chip(ui, &p, health, &text, true).clicked() {
            s.show_connection = !s.show_connection;
        }

        let source = s.selected.map(|uuid| {
            status
                .inputs
                .iter()
                .find(|i| i.uuid == uuid)
                .map(|i| i.name.as_str())
        });
        let (health, text) = match source {
            None => (Health::Pending, "Sem microfone".to_owned()),
            Some(Some(name)) => (Health::Ok, name.to_owned()),
            Some(None) if status.connection == Connection::Connected => {
                (Health::Bad, "Microfone não encontrado".to_owned())
            }
            Some(None) => (Health::Pending, "Microfone salvo".to_owned()),
        };
        chip(ui, &p, health, &text, false);

        match &s.server {
            Ok(url) => {
                let copied = s.copied_at.is_some_and(|t| ui.input(|i| i.time) - t < 1.5);
                let text = if copied {
                    "URL copiada".to_owned()
                } else {
                    "Copiar URL para o OBS".to_owned()
                };
                let r = chip(
                    ui,
                    &p,
                    if copied { Health::Ok } else { Health::Action },
                    &text,
                    true,
                )
                .on_hover_text(format!(
                    "{url}\nNo OBS: Fontes > + > Navegador, cole a URL. O fundo é transparente."
                ));
                if r.clicked() {
                    ui.ctx().copy_text(url.clone());
                    s.copied_at = Some(ui.input(|i| i.time));
                }
                if copied {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(300));
                }
            }
            Err(e) => {
                let r = chip(
                    ui,
                    &p,
                    Health::Bad,
                    "Servidor local parado · tentar de novo",
                    true,
                )
                .on_hover_text(e.as_str());
                if r.clicked() {
                    actions.push(Action::RetryServer);
                }
            }
        }
    });
}

fn chip(
    ui: &mut egui::Ui,
    p: &Palette,
    health: Health,
    text: &str,
    clickable: bool,
) -> egui::Response {
    let dot = match health {
        Health::Ok => p.green,
        Health::Pending => p.yellow,
        Health::Bad => p.red,
        Health::Action => p.accent,
    };
    let font = FontId::proportional(14.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, p.foreground);
    let size = vec2(galley.size().x + 34.0, 30.0);
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let kind = if clickable {
        egui::Role::Button
    } else {
        egui::Role::Label
    };
    response.widget_info(|| egui::WidgetInfo::labeled(kind, true, text));
    let fill = if clickable && response.hovered() {
        p.selection
    } else {
        p.surface
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(15), fill);
    painter.circle_filled(pos2(rect.left() + 15.0, rect.center().y), 4.5, dot);
    painter.galley(
        pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.foreground,
    );
    if clickable {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

fn connection_panel(
    ui: &mut egui::Ui,
    s: &mut ViewState,
    status: &Status,
    actions: &mut Vec<Action>,
) {
    let p = s.palette;
    egui::Frame::new()
        .fill(p.surface)
        .corner_radius(8)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let active = !matches!(status.connection, Connection::Idle | Connection::Failed(_));
            ui.add_enabled_ui(!active, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label("Senha do WebSocket");
                    let field = ui
                        .add(egui::TextEdit::singleline(&mut s.obs_password).password(true).desired_width(180.0))
                        .labelled_by(label.id);
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        actions.push(Action::Connect);
                    }
                    let label = ui.label("Porta");
                    ui.add(egui::DragValue::new(&mut s.obs_port).range(1..=u16::MAX))
                        .labelled_by(label.id);
                });
            });
            ui.horizontal(|ui| {
                if active {
                    if ui.button("Desconectar").clicked() {
                        actions.push(Action::Disconnect);
                    }
                } else if ui
                    .add(egui::Button::new(egui::RichText::new("Conectar").color(p.background)).fill(p.accent))
                    .clicked()
                {
                    actions.push(Action::Connect);
                }
            });
            if let Connection::Retrying { error, .. } | Connection::Failed(error) = &status.connection {
                ui.add(egui::Label::new(egui::RichText::new(error.as_str()).color(p.red)).wrap());
            }
            ui.add(egui::Label::new(
                egui::RichText::new(
                    "A senha fica em OBS > Ferramentas > Configurações do servidor WebSocket > Mostrar informações de conexão.",
                )
                .small()
                .color(p.muted),
            ).wrap());
        });
}

// ----------------------------------------------------------------------- stage

fn stage(ui: &mut egui::Ui, s: &mut ViewState, mouth: Mouth, actions: &mut Vec<Action>) {
    let gap = 12.0;
    let side = ((ui.available_width() - gap) / 2.0).min(CARD_MAX);
    let total = side * 2.0 + gap;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), side + 28.0), Sense::hover());
    let left = row.center().x - total / 2.0;
    let rects = [
        Rect::from_min_size(pos2(left, row.top()), Vec2::splat(side)),
        Rect::from_min_size(pos2(left + side + gap, row.top()), Vec2::splat(side)),
    ];

    let (dropped, hovering_files, pointer) = ui.input(|i| {
        let dropped: Vec<PathBuf> = i
            .raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect();
        (
            dropped,
            !i.raw.hovered_files.is_empty(),
            i.pointer.latest_pos(),
        )
    });
    let under_pointer = |r: &Rect| pointer.is_some_and(|p| r.contains(p));

    for (i, (slot_mouth, title)) in [(Mouth::Closed, "Quieto"), (Mouth::Open, "Falando")]
        .into_iter()
        .enumerate()
    {
        let rect = rects[i];
        let response = ui
            .interact(rect, ui.scope_id().with(("card", i)), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(match (&s.slots[i].file, s.slots[i].size) {
                (Some(file), Some([w, h])) => {
                    format!("{file} ({w}×{h})\nClique ou arraste um PNG para trocar")
                }
                _ => "Clique ou arraste um PNG".to_owned(),
            });
        let name = format!("Imagem {}", title.to_lowercase());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
        if response.clicked() && !s.picking {
            actions.push(Action::Pick(slot_mouth));
        }
        let active = mouth == slot_mouth && s.slots[i].texture.is_some();
        let drop_target = hovering_files && under_pointer(&rect);
        card(
            ui,
            &s.palette,
            rect,
            &s.slots[i],
            active,
            response.hovered() || drop_target,
        );

        let label_pos = pos2(rect.center().x, rect.bottom() + 14.0);
        let color = if active {
            s.palette.accent
        } else {
            s.palette.muted
        };
        ui.painter().text(
            label_pos,
            Align2::CENTER_CENTER,
            title,
            FontId::proportional(15.0),
            color,
        );
    }

    // Drop onto the card under the pointer; if the compositor did not report a
    // position, fill the first empty card, else the closed one.
    if let Some(path) = dropped.into_iter().next() {
        let target = (0..2)
            .find(|&i| under_pointer(&rects[i]))
            .or_else(|| (0..2).find(|&i| s.slots[i].texture.is_none()))
            .unwrap_or(0);
        let mouth = if target == 0 {
            Mouth::Closed
        } else {
            Mouth::Open
        };
        actions.push(Action::Dropped(mouth, path));
    }

    for slot in &s.slots {
        if let Some(e) = &slot.error {
            ui.add(egui::Label::new(egui::RichText::new(e.as_str()).color(s.palette.red)).wrap());
        }
    }
}

fn card(ui: &egui::Ui, p: &Palette, rect: Rect, slot: &SlotView, active: bool, hovered: bool) {
    let painter = ui.painter_at(rect.expand(3.0));
    let radius = CornerRadius::same(10);
    match (&slot.texture, slot.size) {
        (Some(texture), Some([w, h])) => {
            checkerboard(&painter, rect.shrink(1.0), p);
            let inner = rect.shrink(10.0);
            let scale = (inner.width() / w as f32).min(inner.height() / h as f32);
            let img = Rect::from_center_size(inner.center(), vec2(w as f32, h as f32) * scale);
            let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
            painter.image(texture.id(), img, uv, Color32::WHITE);
        }
        _ => {
            painter.rect_filled(rect, radius, p.surface);
            painter.text(
                rect.center() - vec2(0.0, 10.0),
                Align2::CENTER_CENTER,
                "+",
                FontId::proportional(34.0),
                p.muted,
            );
            painter.text(
                rect.center() + vec2(0.0, 22.0),
                Align2::CENTER_CENTER,
                "Escolher PNG",
                FontId::proportional(14.0),
                p.muted,
            );
        }
    }
    let stroke = if active {
        Stroke::new(3.0, p.accent)
    } else if hovered {
        Stroke::new(1.5, p.muted)
    } else if slot.texture.is_none() {
        Stroke::new(1.0, p.muted.gamma_multiply(0.5))
    } else {
        Stroke::new(1.0, p.surface)
    };
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
}

/// Two-tone squares so transparent areas of the PNG are visible.
fn checkerboard(painter: &egui::Painter, rect: Rect, p: &Palette) {
    const SQ: f32 = 12.0;
    let light = p.surface;
    let dark = p.sunken;
    painter.rect_filled(rect, CornerRadius::same(9), dark);
    let painter = painter.with_clip_rect(rect);
    let cols = (rect.width() / SQ).ceil() as i32;
    let rows = (rect.height() / SQ).ceil() as i32;
    for y in 0..rows {
        for x in (y % 2..cols).step_by(2) {
            let min = rect.min + vec2(x as f32 * SQ, y as f32 * SQ);
            painter.rect_filled(Rect::from_min_size(min, Vec2::splat(SQ)), 0.0, light);
        }
    }
}

// ----------------------------------------------------------------------- audio

fn audio(ui: &mut egui::Ui, s: &mut ViewState, status: &Status, actions: &mut Vec<Action>) {
    let p = s.palette;
    let label = ui.label(egui::RichText::new("Microfone").strong());
    ui.horizontal_wrapped(|ui| {
        source_picker(ui, s, status, actions, label.id);
        let test = ui.add(egui::Button::new("Segure para testar").sense(Sense::drag()));
        let held = test.is_pointer_button_down_on();
        if held != s.testing {
            s.testing = held;
            actions.push(Action::Test(held));
        }
    });

    if let Some(params) = meter(ui, &p, status.level_db, s.params) {
        s.params = params;
        actions.push(Action::SetParams(params));
    }
    ui.label(
        egui::RichText::new("Arraste a linha: a boca abre quando o som passa dela.")
            .small()
            .color(p.muted),
    );
}

fn source_picker(
    ui: &mut egui::Ui,
    s: &mut ViewState,
    status: &Status,
    actions: &mut Vec<Action>,
    label: egui::Id,
) {
    let current = match s.selected {
        None => "Escolher fonte de áudio".to_owned(),
        Some(uuid) => status.inputs.iter().find(|i| i.uuid == uuid).map_or_else(
            || "Fonte salva (fora do OBS)".to_owned(),
            |i| i.name.clone(),
        ),
    };
    let before = s.selected;
    ui.add_enabled_ui(!status.inputs.is_empty() || s.selected.is_some(), |ui| {
        egui::ComboBox::from_id_salt("audio-source")
            .selected_text(current)
            .width(200.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut s.selected, None, "Nenhuma");
                for input in &status.inputs {
                    ui.selectable_value(&mut s.selected, Some(input.uuid), &input.name);
                }
            })
            .response
            .labelled_by(label)
            .on_disabled_hover_text("Conecte ao OBS para listar as fontes de áudio");
    });
    if s.selected != before {
        actions.push(Action::Select(s.selected));
    }
}

/// Level bar with the open threshold as a draggable line. Returns new
/// parameters when the user moved it.
fn meter(ui: &mut egui::Ui, p: &Palette, level_db: f32, params: GateParams) -> Option<GateParams> {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click_and_drag());
    let to_x = |db: f32| rect.left() + ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * rect.width();
    let to_db = |x: f32| FLOOR_DB + (x - rect.left()) / rect.width() * -FLOOR_DB;
    let painter = ui.painter();
    let radius = CornerRadius::same(8);
    painter.rect_filled(rect, radius, p.sunken);

    // Band where an open mouth stays open: between close and open thresholds.
    let band = Rect::from_x_y_ranges(to_x(params.close_db)..=to_x(params.open_db), rect.y_range());
    painter.rect_filled(band, 0.0, p.selection.gamma_multiply(0.6));

    if level_db > FLOOR_DB {
        let mut bar = rect.shrink(4.0);
        bar.set_right(to_x(level_db).max(bar.left() + 4.0));
        let fill = if level_db >= params.open_db {
            p.accent
        } else {
            p.muted
        };
        painter.rect_filled(bar, CornerRadius::same(5), fill);
    }

    let x = to_x(params.open_db);
    painter.vline(x, rect.y_range(), Stroke::new(3.0, p.foreground));
    painter.circle_filled(pos2(x, rect.top()), 5.0, p.foreground);
    painter.circle_filled(pos2(x, rect.bottom()), 5.0, p.foreground);

    let response = response.on_hover_cursor(CursorIcon::ResizeHorizontal);
    let pos = response.interact_pointer_pos()?;
    if !(response.dragged() || response.clicked()) {
        return None;
    }
    let open_db = to_db(pos.x).clamp(FLOOR_DB + HYSTERESIS_DB, 0.0).round();
    (open_db != params.open_db).then_some(GateParams {
        open_db,
        close_db: open_db - HYSTERESIS_DB,
        ..params
    })
}
