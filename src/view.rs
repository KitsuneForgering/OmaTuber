//! The window's contents, drawn from plain state and returning user actions.
//!
//! Keeping this free of I/O lets tests render it offscreen with fake status.
//! Layout and wording follow `docs/ui-mockup.svg` and the state tables in
//! `docs/implementation-plan.md`: one `Mode` drives the badge, the stage, the
//! active thumbnail and the explanation under the meter, so they cannot
//! disagree.

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, Vec2, pos2, vec2,
};
use uuid::Uuid;

use crate::calibration::{Problem, Suggestion};
use crate::library_view::{FileTarget, LibraryView};
use crate::mouth::{GateParams, Mouth};
use crate::obs::{Connection, Status};
use crate::scene::Placement;
use crate::theme::Palette;

/// Gap between the open and close thresholds, in dB.
pub const HYSTERESIS_DB: f32 = 8.0;
/// Left end of the level meter.
pub const FLOOR_DB: f32 = -60.0;
/// Most sensitive open threshold the controls allow.
pub const MIN_OPEN_DB: f32 = FLOOR_DB + HYSTERESIS_DB;
/// Below this width the two columns stack.
const TWO_COLUMNS_MIN: f32 = 860.0;
const SIDE_WIDTH: f32 = 360.0;

#[derive(Default)]
pub struct SlotView {
    pub file: Option<String>,
    pub size: Option<[u32; 2]>,
    /// First frame; what thumbnails show.
    pub texture: Option<TextureHandle>,
    /// Every frame of an animation with its delay; empty for a still image.
    pub frames: Vec<(TextureHandle, Duration)>,
    /// UI time (`InputState::time`) the animation started at.
    pub started: f64,
    pub error: Option<String>,
}

impl SlotView {
    /// Texture to draw at UI time `now` and how long until it changes.
    pub fn texture_at(&self, now: f64) -> Option<(&TextureHandle, Option<Duration>)> {
        if self.frames.len() > 1 {
            let delays: Vec<Duration> = self.frames.iter().map(|(_, d)| *d).collect();
            let elapsed = Duration::from_secs_f64((now - self.started).max(0.0));
            let (i, next) = crate::avatar::frame_index(&delays, elapsed);
            return Some((&self.frames[i].0, next));
        }
        self.texture.as_ref().map(|t| (t, None))
    }
}

pub struct ViewState {
    pub palette: Palette,
    pub obs_port: u16,
    pub obs_password: String,
    pub selected: Option<Uuid>,
    pub params: GateParams,
    /// "Testar boca" is held: the preview shows the open mouth. Never sent to
    /// OBS (plan: manual test is preview-only).
    pub testing: bool,
    pub slots: [SlotView; 2],
    pub picking: bool,
    pub server: Result<String, String>,
    pub prefs_error: Option<String>,
    pub show_connection: bool,
    /// UI time at which the URL was copied, for a short confirmation.
    pub copied_at: Option<f64>,
    pub page: Page,
    /// Keep the OBS password in the system keyring (plan: "Lembrar neste
    /// computador"); never in preferences.
    pub remember_password: bool,
    /// Scene to add the avatar to; `None` means the one on air.
    pub scene_choice: Option<String>,
    pub calibration: Calibration,
    /// Why remembering is unavailable or what just happened to the stored
    /// password; `None` when there is nothing to say.
    pub secret_note: Option<String>,
    pub library: LibraryView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Avatar,
    Library,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Connect,
    Disconnect,
    Select(Option<Uuid>),
    SetParams(GateParams),
    Pick(Mouth),
    Dropped(Mouth, PathBuf),
    UseExample,
    RetryServer,
    PickLibraryFile(FileTarget),
    /// Save the draft library and send it to the reactions task.
    ApplyLibrary,
    TriggerSentiment(Uuid),
    ClearReaction,
    ToggleObject(Uuid),
    /// Start or stop listening for trained sounds.
    SetVoice(bool),
    /// Remember (store after the next successful connection) or forget.
    SetRemember(bool),
    ChooseScene(Option<String>),
    /// Create, link or re-show the avatar's browser source in the scene.
    AddToScene,
    /// Begin guided calibration (silence, then speech).
    StartCalibration,
    /// Record one 2.5 s example of this sound.
    RecordExample(String),
    RemoveLastExample(String),
    TrainSounds,
    AcceptModel,
    DiscardModel,
}

/// Examples per sound below which the editor highlights the count; mirrors
/// `training::MIN_CLIPS`, which is only built with the `sound` feature.
pub const MIN_TEACH_CLIPS: usize = 3;

/// Guided calibration as the voice card shows it; `main` advances it as the
/// OBS worker finishes each 3 s batch of levels.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Calibration {
    #[default]
    Idle,
    Silence,
    Speech {
        silence: Vec<f32>,
    },
    Done(Result<Suggestion, Problem>),
}

impl Calibration {
    /// Next step once a batch of levels is ready; `true` means another batch
    /// must be gathered (the speech phase).
    pub fn advance(self, levels: Vec<f32>) -> (Self, bool) {
        match self {
            Calibration::Silence => (Calibration::Speech { silence: levels }, true),
            Calibration::Speech { silence } => (
                Calibration::Done(crate::calibration::suggest(&silence, &levels, MIN_OPEN_DB)),
                false,
            ),
            other => (other, false),
        }
    }
}

/// Where the preview's mouth comes from right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// "Testar boca" held; preview only.
    Test,
    /// Connected and the selected source is delivering levels.
    Live,
    /// Connected, but no levels from the selected source (or none selected).
    NoSignal,
    /// Not connected to OBS.
    NoObs,
}

pub fn mode(s: &ViewState, status: &Status) -> Mode {
    if s.testing {
        Mode::Test
    } else if status.connection != Connection::Connected {
        Mode::NoObs
    } else if status.receiving && source_name(s, status).is_some() {
        Mode::Live
    } else {
        Mode::NoSignal
    }
}

/// Mouth shown in the preview: the test overrides the live state.
pub fn preview_mouth(s: &ViewState, status: &Status) -> Mouth {
    match mode(s, status) {
        Mode::Test => Mouth::Open,
        Mode::Live => status.mouth,
        Mode::NoSignal | Mode::NoObs => Mouth::Closed,
    }
}

/// Sensitivity 0–100 shown to the user; 100 opens the mouth at the faintest sound.
pub fn sensitivity(open_db: f32) -> f32 {
    (open_db / MIN_OPEN_DB * 100.0).clamp(0.0, 100.0)
}

pub fn open_db_for(sensitivity: f32) -> f32 {
    (sensitivity.clamp(0.0, 100.0) / 100.0 * MIN_OPEN_DB).round()
}

/// One sentence under the meter explaining the mouth's position.
pub fn cause(s: &ViewState, status: &Status) -> String {
    let source = source_name(s, status);
    match mode(s, status) {
        Mode::Test => "Teste manual: a prévia mostra a boca aberta. O OBS não muda.".to_owned(),
        Mode::NoObs => "Sem OBS conectado, a boca não reage à voz. Use Testar boca para conferir as imagens.".to_owned(),
        Mode::NoSignal => match (s.selected, source) {
            (None, _) => "Escolha o microfone para a boca reagir.".to_owned(),
            (Some(_), None) => "O microfone escolhido não existe mais no OBS. Escolha outro.".to_owned(),
            (Some(_), Some(name)) => format!(
                "Nenhum som chega de {name}. A fonte precisa estar ativa na cena atual do OBS."
            ),
        },
        Mode::Live if status.muted == Some(true) => {
            "Seu microfone está silenciado no OBS. Ative o som dele no mixer de áudio do OBS.".to_owned()
        }
        Mode::Live if status.level_db == f32::NEG_INFINITY && status.muted == Some(false) => {
            "Silêncio total, embora o microfone não esteja silenciado no OBS. Confira o dispositivo de entrada dele.".to_owned()
        }
        Mode::Live if status.level_db == f32::NEG_INFINITY => {
            "Silêncio total. O microfone pode estar silenciado no OBS.".to_owned()
        }
        Mode::Live if status.mouth == Mouth::Open => {
            "Sua voz passou da marca e abriu a boca.".to_owned()
        }
        Mode::Live if status.level_db >= s.params.close_db => {
            "Fala detectada perto da marca.".to_owned()
        }
        Mode::Live => {
            "Seu som ainda não passou da marca. Se a boca não abre ao falar, aumente a sensibilidade.".to_owned()
        }
    }
}

fn source_name<'a>(s: &ViewState, status: &'a Status) -> Option<&'a str> {
    let uuid = s.selected?;
    status
        .inputs
        .iter()
        .find(|i| i.uuid == uuid)
        .map(|i| i.name.as_str())
}

pub fn draw(ui: &mut egui::Ui, s: &mut ViewState, status: &Status) -> Vec<Action> {
    let mut actions = Vec::new();
    let frame = egui::Frame::central_panel(ui.style()).inner_margin(20);
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            header(ui, s, status);
            ui.add_space(12.0);
            if s.page == Page::Library {
                crate::library_view::draw(ui, s, &mut actions);
                return;
            }
            let width = ui.available_width();
            if width >= TWO_COLUMNS_MIN {
                let gap = 16.0;
                ui.horizontal_top(|ui| {
                    ui.allocate_ui(vec2(width - SIDE_WIDTH - gap, 0.0), |ui| {
                        ui.vertical(|ui| preview_card(ui, s, status, &mut actions));
                    });
                    ui.add_space(gap - ui.spacing().item_spacing.x);
                    ui.allocate_ui(vec2(SIDE_WIDTH, 0.0), |ui| {
                        ui.vertical(|ui| {
                            checklist_card(ui, s, status, &mut actions);
                            ui.add_space(8.0);
                            voice_card(ui, s, status, &mut actions);
                        });
                    });
                });
            } else {
                preview_card(ui, s, status, &mut actions);
                ui.add_space(8.0);
                checklist_card(ui, s, status, &mut actions);
                ui.add_space(8.0);
                voice_card(ui, s, status, &mut actions);
            }
            if let Some(e) = &s.prefs_error {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(e.as_str()).color(s.palette.yellow)).wrap());
            }
        });
    });
    actions
}

// --------------------------------------------------------------------- header

fn header(ui: &mut egui::Ui, s: &mut ViewState, status: &Status) {
    let p = s.palette;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new("OmaTuber").size(24.0).strong());
            ui.label(RichText::new("Seu avatar, pronto para aparecer no OBS").color(p.muted));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            mode_badge(ui, &p, mode(s, status));
            ui.add_space(12.0);
            let dirty = if s.library.dirty() { " •" } else { "" };
            ui.selectable_value(
                &mut s.page,
                Page::Library,
                format!("Reações e objetos{dirty}"),
            );
            ui.selectable_value(&mut s.page, Page::Avatar, "Avatar");
        });
    });
}

fn mode_badge(ui: &mut egui::Ui, p: &Palette, mode: Mode) -> egui::Response {
    let (text, color) = match mode {
        Mode::Live => ("AO VIVO · voz", p.green),
        Mode::Test => ("TESTE MANUAL · só prévia", p.yellow),
        Mode::NoSignal => ("SEM SINAL", p.red),
        Mode::NoObs => ("SEM OBS", p.muted),
    };
    pill(ui, p, color, text, false)
}

/// Rounded label with a colored dot. Named for screen readers by its text.
fn pill(
    ui: &mut egui::Ui,
    p: &Palette,
    dot: Color32,
    text: &str,
    clickable: bool,
) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::proportional(14.0), p.foreground);
    let size = vec2(galley.size().x + 34.0, 30.0);
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let role = if clickable {
        egui::Role::Button
    } else {
        egui::Role::Label
    };
    response.widget_info(|| egui::WidgetInfo::labeled(role, true, text));
    let fill = if clickable && response.hovered() {
        p.selection
    } else {
        p.surface
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(15), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(15),
        Stroke::new(1.0, dot.gamma_multiply(0.5)),
        StrokeKind::Inside,
    );
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

fn card_frame(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.surface.gamma_multiply(0.6))
        .stroke(Stroke::new(1.0, p.surface))
        .corner_radius(14)
        .inner_margin(16)
}

// -------------------------------------------------------------------- preview

fn preview_card(ui: &mut egui::Ui, s: &mut ViewState, status: &Status, actions: &mut Vec<Action>) {
    let p = s.palette;
    card_frame(&p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Prévia do avatar").size(18.0).strong());
        ui.add_space(4.0);
        let shown = preview_mouth(s, status);
        let m = mode(s, status);
        let has_images = s.slots.iter().any(|slot| slot.texture.is_some());

        let width = ui.available_width();
        let height = (width * 0.62).clamp(220.0, 440.0);
        let (stage, stage_response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        // The stage is painted; describe it for screen readers too.
        let description = if has_images {
            format!(
                "Prévia do avatar: {}",
                caption_text(shown, m, active_label(s).as_deref())
            )
        } else {
            "Prévia do avatar: Comece pelas imagens".to_owned()
        };
        stage_response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &description));
        let painter = ui.painter_at(stage);
        checkerboard(&painter, stage, &p);
        // Fall back to the other image so a half-configured avatar still shows.
        let now = ui.input(|i| i.time);
        let slot = &s.slots[shown as usize];
        let slot = if slot.texture.is_some() {
            slot
        } else {
            &s.slots[1 - shown as usize]
        };
        // An active sentiment replaces the base, as on the browser source.
        let base = slot
            .texture_at(now)
            .zip(slot.size)
            .map(|((t, next), size)| {
                if let Some(next) = next {
                    ui.ctx().request_repaint_after(next);
                }
                (t, size)
            });
        let sentiment = sentiment_image(s, shown, now).map(|(t, size, next)| {
            if let Some(next) = next {
                ui.ctx().request_repaint_after(next);
            }
            (t, size)
        });
        if let Some((texture, [w, h])) = sentiment.or(base) {
            let inner = stage.shrink(16.0);
            let scale = (inner.width() / w as f32).min(inner.height() / h as f32);
            let img = Rect::from_center_size(inner.center(), vec2(w as f32, h as f32) * scale);
            let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
            painter.image(texture.id(), img, uv, Color32::WHITE);
            // Objects: center and width as fractions of the drawn avatar.
            let lib = &s.library;
            for id in &lib.objects_on {
                let Some(o) = lib.applied.object(*id) else {
                    continue;
                };
                let Some(picture) = lib.textures.get(&o.file) else {
                    continue;
                };
                let (tex, next) = picture.at(since(now, lib.objects_since));
                if let Some(next) = next {
                    ui.ctx().request_repaint_after(next);
                }
                let [ow, oh] = picture.size.map(|v| v as f32);
                let width = o.scale * img.width();
                let size = vec2(width, width * oh / ow);
                let center = img.min + vec2(o.x * img.width(), o.y * img.height());
                painter.image(
                    tex.id(),
                    Rect::from_center_size(center, size),
                    uv,
                    Color32::WHITE,
                );
            }
        }
        if has_images {
            stage_caption(&painter, &p, stage, shown, m, active_label(s).as_deref());
        } else {
            painter.text(
                stage.center() - vec2(0.0, 30.0),
                Align2::CENTER_CENTER,
                "Comece pelas imagens",
                FontId::proportional(20.0),
                p.foreground,
            );
            painter.text(
                stage.center(),
                Align2::CENTER_CENTER,
                "Um PNG para Quieto e outro para Falando",
                FontId::proportional(14.0),
                p.muted,
            );
            let buttons =
                Rect::from_center_size(stage.center() + vec2(0.0, 44.0), vec2(300.0, 36.0));
            // A child UI does not move this UI's cursor, unlike `scope_builder`.
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(buttons)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            if child.add(accent_button(&p, "Escolher imagens")).clicked() && !s.picking {
                actions.push(Action::Pick(Mouth::Closed));
            }
            if child.button("Usar exemplo").clicked() {
                actions.push(Action::UseExample);
            }
        }

        ui.add_space(10.0);
        thumbnails(ui, s, shown, actions);
    });
}

/// The active sentiment's image for this mouth, chosen as the browser page
/// does: talking image with an open mouth if there is one, else quiet.
/// Its animation runs from the last trigger, like the page's fresh image.
fn sentiment_image(
    s: &ViewState,
    shown: Mouth,
    now: f64,
) -> Option<(&TextureHandle, [u32; 2], Option<Duration>)> {
    let lib = &s.library;
    let sentiment = lib.applied.sentiment(lib.active?.0)?;
    let talking = sentiment.talking.as_ref().filter(|_| shown == Mouth::Open);
    let file = talking.or(sentiment.quiet.as_ref())?;
    let picture = lib.textures.get(file)?;
    let (texture, next) = picture.at(since(now, lib.active_since));
    Some((texture, picture.size, next))
}

/// Time from UI timestamp `start` to `now`, never negative.
fn since(now: f64, start: f64) -> Duration {
    Duration::from_secs_f64((now - start).max(0.0))
}

/// "Animado · Atalho" while a sentiment is on air.
fn active_label(s: &ViewState) -> Option<String> {
    let (id, source) = s.library.active?;
    let name = &s.library.applied.sentiment(id)?.name;
    Some(format!(
        "{name} · {}",
        crate::library_view::source_label(source)
    ))
}

fn accent_button(p: &Palette, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(p.on_accent)).fill(p.accent)
}

/// The stage caption, also used as the stage's accessible description.
fn caption_text(shown: Mouth, m: Mode, reaction: Option<&str>) -> String {
    let state = if shown == Mouth::Open {
        "Falando"
    } else {
        "Quieto"
    };
    let detail = match m {
        Mode::Live if shown == Mouth::Open => "voz detectada",
        Mode::Live => "sem voz agora",
        Mode::Test => "prévia apenas, não altera o OBS",
        Mode::NoSignal | Mode::NoObs => "sem leitura ao vivo",
    };
    match reaction {
        Some(reaction) => format!("{reaction}  ·  {state}"),
        None => format!("{state}  ·  {detail}"),
    }
}

/// Badge in the stage's top-left and state caption in its bottom-left.
fn stage_caption(
    painter: &egui::Painter,
    p: &Palette,
    stage: Rect,
    shown: Mouth,
    m: Mode,
    reaction: Option<&str>,
) {
    let (badge, badge_color) = match m {
        Mode::Live => ("VOZ AO VIVO", p.green),
        Mode::Test => ("TESTE MANUAL", p.yellow),
        Mode::NoSignal => ("SEM SINAL DO MICROFONE", p.red),
        Mode::NoObs => ("SEM OBS", p.muted),
    };
    overlay_pill(
        painter,
        p,
        stage.left_top() + vec2(14.0, 14.0),
        badge,
        badge_color,
        13.0,
    );

    let text = caption_text(shown, m, reaction);
    let dot = if shown == Mouth::Open {
        p.accent
    } else {
        p.muted
    };
    let font = FontId::proportional(15.0);
    let galley = painter.layout_no_wrap(text, font, p.foreground);
    let rect = Rect::from_min_size(
        pos2(stage.left() + 14.0, stage.bottom() - 14.0 - 36.0),
        vec2(galley.size().x + 40.0, 36.0),
    );
    painter.rect_filled(rect, CornerRadius::same(8), p.sunken.gamma_multiply(0.92));
    painter.circle_filled(pos2(rect.left() + 18.0, rect.center().y), 6.0, dot);
    painter.galley(
        pos2(rect.left() + 32.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.foreground,
    );
}

fn overlay_pill(
    painter: &egui::Painter,
    p: &Palette,
    at: Pos2,
    text: &str,
    color: Color32,
    size: f32,
) {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(size), p.foreground);
    let rect = Rect::from_min_size(at, vec2(galley.size().x + 34.0, galley.size().y + 14.0));
    painter.rect_filled(rect, CornerRadius::same(14), p.sunken.gamma_multiply(0.92));
    painter.rect_stroke(
        rect,
        CornerRadius::same(14),
        Stroke::new(1.0, color),
        StrokeKind::Inside,
    );
    painter.circle_filled(pos2(rect.left() + 15.0, rect.center().y), 4.5, color);
    painter.galley(
        pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.foreground,
    );
}

fn thumbnails(ui: &mut egui::Ui, s: &mut ViewState, shown: Mouth, actions: &mut Vec<Action>) {
    let p = s.palette;
    // While a sentiment fills the stage, neither base image is on screen.
    let sentiment_on = sentiment_image(s, shown, ui.input(|i| i.time)).is_some();
    let gap = 12.0;
    let width = ui.available_width();
    let w = (width - gap) / 2.0;
    let (row, _) = ui.allocate_exact_size(vec2(width, 96.0), Sense::hover());
    let rects = [
        Rect::from_min_size(row.min, vec2(w, row.height())),
        Rect::from_min_size(row.min + vec2(w + gap, 0.0), vec2(w, row.height())),
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
    let under_pointer = |r: &Rect| pointer.is_some_and(|pt| r.contains(pt));

    for (i, (slot_mouth, title)) in [(Mouth::Closed, "QUIETO"), (Mouth::Open, "FALANDO")]
        .into_iter()
        .enumerate()
    {
        let rect = rects[i];
        let slot = &s.slots[i];
        let response = ui
            .interact(rect, ui.scope_id().with(("thumb", i)), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Clique ou arraste um PNG para trocar");
        // Painted file name, so include it for screen readers.
        let name = match &slot.file {
            Some(file) => format!("Imagem {}: {file}", title.to_lowercase()),
            None => format!("Imagem {}", title.to_lowercase()),
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
        if response.clicked() && !s.picking {
            actions.push(Action::Pick(slot_mouth));
        }
        let active = !sentiment_on && shown == slot_mouth && slot.texture.is_some();
        let hovered = response.hovered() || (hovering_files && under_pointer(&rect));
        let painter = ui.painter_at(rect.expand(2.0));
        let fill = if active {
            p.selection.gamma_multiply(0.7)
        } else {
            p.sunken.gamma_multiply(0.6)
        };
        painter.rect_filled(rect, CornerRadius::same(12), fill);
        let stroke = if active {
            Stroke::new(2.0, p.accent)
        } else if hovered {
            Stroke::new(1.5, p.muted)
        } else {
            Stroke::new(1.0, p.surface)
        };
        painter.rect_stroke(rect, CornerRadius::same(12), stroke, StrokeKind::Inside);

        let thumb = Rect::from_min_size(
            rect.min + vec2(12.0, 12.0),
            Vec2::splat(rect.height() - 24.0),
        );
        checkerboard(&painter, thumb, &p);
        if let (Some(texture), Some([tw, th])) = (&slot.texture, slot.size) {
            let scale = (thumb.width() / tw as f32).min(thumb.height() / th as f32);
            let img = Rect::from_center_size(thumb.center(), vec2(tw as f32, th as f32) * scale);
            painter.image(
                texture.id(),
                img,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.text(
                thumb.center(),
                Align2::CENTER_CENTER,
                "+",
                FontId::proportional(26.0),
                p.muted,
            );
        }

        let x = thumb.right() + 14.0;
        let color = if active { p.accent } else { p.muted };
        painter.text(
            pos2(x, rect.top() + 22.0),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(12.5),
            color,
        );
        let file = slot.file.as_deref().unwrap_or("Escolher PNG");
        painter.text(
            pos2(x, rect.top() + 46.0),
            Align2::LEFT_CENTER,
            file,
            FontId::proportional(15.0),
            p.foreground,
        );
        let detail = if active {
            "Exibindo agora".to_owned()
        } else if let Some([w, h]) = slot.size {
            format!("{w} × {h}")
        } else {
            "clique ou arraste".to_owned()
        };
        painter.text(
            pos2(x, rect.top() + 70.0),
            Align2::LEFT_CENTER,
            detail,
            FontId::proportional(13.0),
            p.muted,
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
            ui.add(egui::Label::new(RichText::new(e.as_str()).color(p.red)).wrap());
        }
    }
    if let (Some([a, b]), Some([c, d])) = (s.slots[0].size, s.slots[1].size)
        && [a, b] != [c, d]
    {
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "As imagens têm tamanhos diferentes ({a} × {b} e {c} × {d}); o avatar pode pular ao falar."
                ))
                .color(p.yellow),
            )
            .wrap(),
        );
    }
}

/// Two-tone squares so transparent areas of the PNG are visible.
fn checkerboard(painter: &egui::Painter, rect: Rect, p: &Palette) {
    const SQ: f32 = 14.0;
    painter.rect_filled(rect, CornerRadius::same(10), p.sunken);
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let cols = (rect.width() / SQ).ceil() as i32;
    let rows = (rect.height() / SQ).ceil() as i32;
    for y in 0..rows {
        for x in (y % 2..cols).step_by(2) {
            let min = rect.min + vec2(x as f32 * SQ, y as f32 * SQ);
            painter.rect_filled(Rect::from_min_size(min, Vec2::splat(SQ)), 0.0, p.checker);
        }
    }
}

// ------------------------------------------------------------------ checklist

#[derive(Clone, Copy)]
enum Step {
    Done,
    Waiting,
    Problem,
    Active,
}

fn checklist_card(
    ui: &mut egui::Ui,
    s: &mut ViewState,
    status: &Status,
    actions: &mut Vec<Action>,
) {
    let p = s.palette;
    card_frame(&p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("O que está acontecendo").size(18.0).strong());
        ui.label(
            RichText::new("Cada etapa mostra seu próprio estado.")
                .small()
                .color(p.muted),
        );
        ui.add_space(6.0);

        // OBS
        let (step, title, detail) = match &status.connection {
            Connection::Connected => (
                Step::Done,
                "OBS conectado".to_owned(),
                "Recebendo dados do OBS".to_owned(),
            ),
            Connection::Connecting => (
                Step::Waiting,
                "Conectando ao OBS…".to_owned(),
                String::new(),
            ),
            Connection::Idle => (
                Step::Waiting,
                "OBS não conectado".to_owned(),
                "Informe a senha do WebSocket para conectar".to_owned(),
            ),
            Connection::Retrying { after, .. } => (
                Step::Problem,
                "OBS fechado ou WebSocket desligado".to_owned(),
                format!("Nova tentativa em {} s", after.as_secs()),
            ),
            Connection::Failed(e) => (Step::Problem, "OBS recusou a conexão".to_owned(), e.clone()),
        };
        if step_row(ui, &p, step, &title, &detail, true).clicked() {
            s.show_connection = !s.show_connection;
        }
        if s.show_connection {
            connection_panel(ui, s, status, actions);
        }
        divider(ui, &p);

        // Microphone
        let connected = status.connection == Connection::Connected;
        let name = source_name(s, status);
        let (step, title, detail) = match (s.selected, name) {
            (None, _) => (
                Step::Waiting,
                "Escolha o microfone".to_owned(),
                "Em Sua voz, abaixo".to_owned(),
            ),
            (Some(_), None) if connected => (
                Step::Problem,
                "Microfone não encontrado".to_owned(),
                "Ele foi removido ou renomeado no OBS".to_owned(),
            ),
            (Some(_), None) => (
                Step::Waiting,
                "Microfone escolhido".to_owned(),
                "Aguardando o OBS".to_owned(),
            ),
            (Some(_), Some(n)) if !connected => {
                (Step::Waiting, n.to_owned(), "Aguardando o OBS".to_owned())
            }
            // Mute is the most actionable cause, so it comes before the rest.
            (Some(_), Some(n)) if status.muted == Some(true) => (
                Step::Problem,
                "Microfone silenciado".to_owned(),
                format!("Ative {n} no mixer de áudio do OBS"),
            ),
            (Some(_), Some(n)) if !status.receiving => (
                Step::Problem,
                "Sem sinal desta fonte".to_owned(),
                format!("{n} precisa estar ativo na cena atual"),
            ),
            (Some(_), Some(n)) if status.level_db == f32::NEG_INFINITY => (
                Step::Problem,
                "Microfone em silêncio total".to_owned(),
                if status.muted == Some(false) {
                    format!("{n} não está silenciado; confira o dispositivo de entrada")
                } else {
                    format!("{n} pode estar silenciado no OBS")
                },
            ),
            (Some(_), Some(n)) => (
                Step::Done,
                "Microfone ativo".to_owned(),
                format!("{n} · som recebido agora"),
            ),
        };
        step_row(ui, &p, step, &title, &detail, false);
        divider(ui, &p);

        // Mouth
        let both = s.slots.iter().all(|slot| slot.texture.is_some());
        let shown = preview_mouth(s, status);
        let (step, title, detail) = match mode(s, status) {
            _ if !both => (
                Step::Waiting,
                "Faltam imagens",
                "Escolha os PNGs Quieto e Falando",
            ),
            Mode::Test => (
                Step::Active,
                "Teste manual",
                "Prévia apenas, não altera o OBS",
            ),
            Mode::Live if shown == Mouth::Open => {
                (Step::Active, "Boca aberta", "Som acima da sensibilidade")
            }
            Mode::Live => (Step::Done, "Boca fechada", "Som abaixo da marca"),
            Mode::NoSignal | Mode::NoObs => (Step::Waiting, "Boca parada", "Sem leitura ao vivo"),
        };
        step_row(ui, &p, step, title, detail, false);
        divider(ui, &p);

        // Scene: verified through OBS when connected, never assumed.
        let server = s.server.clone();
        match &server {
            Ok(url) => {
                let connected = status.connection == Connection::Connected;
                let program = status.scenes.program.as_deref();
                match (&status.placement, connected) {
                    (Some((scene, Placement::Visible)), true) => {
                        let detail = if program.is_some_and(|p| p != scene) {
                            "Pronto nesta cena; outra cena está no ar"
                        } else {
                            "Avatar adicionado ao OBS"
                        };
                        step_row(
                            ui,
                            &p,
                            Step::Done,
                            &format!("Visível na cena {scene}"),
                            detail,
                            false,
                        );
                    }
                    (Some((scene, Placement::Hidden)), true) => {
                        step_row(
                            ui,
                            &p,
                            Step::Problem,
                            &format!("Oculto na cena {scene}"),
                            "A fonte existe, mas está desligada no OBS",
                            false,
                        );
                        indented(ui, |ui| {
                            if ui.add(accent_button(&p, "Mostrar no OBS")).clicked() {
                                actions.push(Action::AddToScene);
                            }
                        });
                    }
                    (Some((scene, Placement::NotAdded)), true) => {
                        step_row(
                            ui,
                            &p,
                            Step::Waiting,
                            "Ainda não adicionado",
                            "O OmaTuber cria uma fonte Navegador com a URL e o tamanho do avatar",
                            false,
                        );
                        keep_open_hint(ui, &p);
                        indented(ui, |ui| {
                            if ui
                                .add(accent_button(&p, &format!("Adicionar à cena {scene}")))
                                .clicked()
                            {
                                actions.push(Action::AddToScene);
                            }
                        });
                    }
                    (None, true) => {
                        step_row(ui, &p, Step::Waiting, "Verificando a cena…", "", false);
                    }
                    (_, false) => {
                        step_row(
                            ui,
                            &p,
                            Step::Waiting,
                            "Na cena: não verificado",
                            "Conecte ao OBS para adicionar o avatar sozinho, ou copie a URL",
                            false,
                        );
                    }
                }
                if connected && status.scenes.names.len() > 1 {
                    indented(ui, |ui| scene_picker(ui, s, status, actions));
                }
                if let Some(msg) = &status.scene_message {
                    indented(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(msg.as_str()).small().color(p.muted))
                                .wrap(),
                        );
                    });
                }
                let copied = s.copied_at.is_some_and(|t| ui.input(|i| i.time) - t < 1.5);
                indented(ui, |ui| {
                    let label = match (copied, connected) {
                        (true, _) => "URL copiada",
                        (false, true) => "Ou copiar URL",
                        (false, false) => "Copiar URL",
                    };
                    let button = if connected {
                        egui::Button::new(label)
                    } else {
                        accent_button(&p, label)
                    };
                    if ui.add(button).on_hover_text(url.as_str()).clicked() {
                        ui.ctx().copy_text(url.clone());
                        s.copied_at = Some(ui.input(|i| i.time));
                    }
                    ui.label(
                        RichText::new(url.as_str())
                            .monospace()
                            .small()
                            .color(p.muted),
                    );
                });
                if !connected {
                    keep_open_hint(ui, &p);
                }
                if copied {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(300));
                }
            }
            Err(e) => {
                step_row(ui, &p, Step::Problem, "Servidor local parado", e, false);
                ui.horizontal(|ui| {
                    ui.add_space(36.0);
                    if ui.button("Tentar de novo").clicked() {
                        actions.push(Action::RetryServer);
                    }
                });
            }
        }
    });
}

/// Row content aligned with the step text, past the icon column.
fn indented(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(36.0);
        add(ui);
    });
}

/// Plan: shown before the source is added to a scene.
fn keep_open_hint(ui: &mut egui::Ui, p: &Palette) {
    indented(ui, |ui| {
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Mantenha o OmaTuber aberto durante a transmissão; pode minimizá-lo.",
                )
                .small()
                .color(p.muted),
            )
            .wrap(),
        );
    });
}

fn scene_picker(ui: &mut egui::Ui, s: &mut ViewState, status: &Status, actions: &mut Vec<Action>) {
    let label = ui.label("Cena");
    let on_air = "A que estiver no ar";
    let before = s.scene_choice.clone();
    egui::ComboBox::from_id_salt("scene-choice")
        .selected_text(s.scene_choice.as_deref().unwrap_or(on_air))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut s.scene_choice, None, on_air);
            for name in &status.scenes.names {
                ui.selectable_value(&mut s.scene_choice, Some(name.clone()), name);
            }
        })
        .response
        .labelled_by(label.id);
    if s.scene_choice != before {
        actions.push(Action::ChooseScene(s.scene_choice.clone()));
    }
}

fn divider(ui: &mut egui::Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    ui.painter().hline(
        rect.left() + 36.0..=rect.right(),
        rect.center().y,
        Stroke::new(1.0, p.surface),
    );
}

fn step_row(
    ui: &mut egui::Ui,
    p: &Palette,
    step: Step,
    title: &str,
    detail: &str,
    clickable: bool,
) -> egui::Response {
    let width = ui.available_width();
    let text_width = width - 40.0;
    let title_galley = ui.painter().layout(
        title.to_owned(),
        FontId::proportional(16.0),
        p.foreground,
        text_width,
    );
    let detail_galley = ui.painter().layout(
        detail.to_owned(),
        FontId::proportional(13.0),
        p.muted,
        text_width,
    );
    let height = (title_galley.size().y + detail_galley.size().y + 4.0).max(32.0);
    let sense = if clickable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), sense);
    let label = if detail.is_empty() {
        title.to_owned()
    } else {
        format!("{title}. {detail}")
    };
    let role = if clickable {
        egui::Role::Button
    } else {
        egui::Role::Label
    };
    response.widget_info(|| egui::WidgetInfo::labeled(role, true, &label));
    let painter = ui.painter();
    if clickable && response.hovered() {
        painter.rect_filled(
            rect.expand(3.0),
            CornerRadius::same(6),
            p.selection.gamma_multiply(0.5),
        );
    }
    step_icon(
        painter,
        p,
        pos2(rect.left() + 14.0, rect.top() + 14.0),
        step,
    );
    painter.galley(
        pos2(rect.left() + 36.0, rect.top()),
        title_galley.clone(),
        p.foreground,
    );
    painter.galley(
        pos2(rect.left() + 36.0, rect.top() + title_galley.size().y + 2.0),
        detail_galley,
        p.muted,
    );
    if clickable {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

fn step_icon(painter: &egui::Painter, p: &Palette, c: Pos2, step: Step) {
    let r = 12.0;
    match step {
        Step::Done => {
            painter.circle(
                c,
                r,
                p.green.gamma_multiply(0.18),
                Stroke::new(1.5, p.green),
            );
            let stroke = Stroke::new(2.0, p.green);
            painter.line_segment([c + vec2(-5.0, 0.5), c + vec2(-1.5, 4.0)], stroke);
            painter.line_segment([c + vec2(-1.5, 4.0), c + vec2(5.0, -4.0)], stroke);
        }
        Step::Waiting => {
            painter.circle_stroke(c, r, Stroke::new(1.5, p.yellow));
            painter.circle_filled(c, 3.0, p.yellow);
        }
        Step::Problem => {
            painter.circle(c, r, p.red.gamma_multiply(0.18), Stroke::new(1.5, p.red));
            painter.text(
                c,
                Align2::CENTER_CENTER,
                "!",
                FontId::proportional(15.0),
                p.red,
            );
        }
        Step::Active => {
            painter.circle(
                c,
                r,
                p.accent.gamma_multiply(0.22),
                Stroke::new(1.5, p.accent),
            );
            let stroke = Stroke::new(1.8, p.accent);
            let pts = [
                (-6.0, 0.0),
                (-3.0, 0.0),
                (-1.0, -5.0),
                (1.5, 5.0),
                (3.5, 0.0),
                (6.0, 0.0),
            ];
            for w in pts.windows(2) {
                painter.line_segment([c + vec2(w[0].0, w[0].1), c + vec2(w[1].0, w[1].1)], stroke);
            }
        }
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
        .fill(p.sunken.gamma_multiply(0.7))
        .corner_radius(10)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let active = !matches!(status.connection, Connection::Idle | Connection::Failed(_));
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "No OBS: Ferramentas > Configurações do servidor WebSocket. Ative o servidor e use Mostrar informações de conexão para copiar a senha.",
                    )
                    .small()
                    .color(p.muted),
                )
                .wrap(),
            );
            ui.add_enabled_ui(!active, |ui| {
                let label = ui.label("Senha");
                let field = ui
                    .add(
                        egui::TextEdit::singleline(&mut s.obs_password)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    )
                    .labelled_by(label.id);
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    actions.push(Action::Connect);
                }
            });
            if ui
                .checkbox(&mut s.remember_password, "Lembrar senha neste computador")
                .on_hover_text("Guarda no chaveiro do sistema e conecta sozinho ao abrir")
                .changed()
            {
                actions.push(Action::SetRemember(s.remember_password));
            }
            if let Some(note) = &s.secret_note {
                ui.add(egui::Label::new(RichText::new(note.as_str()).small().color(p.muted)).wrap());
            }
            ui.horizontal(|ui| {
                if active {
                    if ui.button("Desconectar").clicked() {
                        actions.push(Action::Disconnect);
                    }
                } else if ui.add(accent_button(&p, "Conectar")).clicked() {
                    actions.push(Action::Connect);
                }
            });
            egui::CollapsingHeader::new("Ajustes avançados")
                .id_salt("connection-advanced")
                .show(ui, |ui| {
                    ui.add_enabled_ui(!active, |ui| {
                        ui.horizontal(|ui| {
                            let label = ui.label("Porta do WebSocket");
                            ui.add(egui::DragValue::new(&mut s.obs_port).range(1..=u16::MAX))
                                .labelled_by(label.id);
                        });
                    });
                });
        });
}

// ---------------------------------------------------------------------- voice

fn voice_card(ui: &mut egui::Ui, s: &mut ViewState, status: &Status, actions: &mut Vec<Action>) {
    let p = s.palette;
    card_frame(&p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let heading = ui.label(RichText::new("Sua voz").size(18.0).strong());
        ui.horizontal(|ui| {
            source_picker(ui, s, status, actions, heading.id);
            let muted = status.connection == Connection::Connected && status.muted == Some(true);
            let (level, color) = if muted {
                ("silenciado no OBS".to_owned(), p.yellow)
            } else if mode(s, status) == Mode::Live && status.level_db > FLOOR_DB {
                (format!("{:.0} dB", status.level_db), p.muted)
            } else {
                ("—".to_owned(), p.muted)
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(level).color(color));
            });
        });

        let live_level = if mode(s, status) == Mode::Live {
            status.level_db
        } else {
            f32::NEG_INFINITY
        };
        if let Some(params) = meter(ui, &p, live_level, s.params) {
            s.params = params;
            actions.push(Action::SetParams(params));
        }

        ui.horizontal(|ui| {
            let label = ui.label("Sensibilidade");
            ui.spacing_mut().slider_width = ui.available_width() - 8.0;
            let mut sens = sensitivity(s.params.open_db);
            let slider = egui::Slider::new(&mut sens, 0.0..=100.0).show_value(false);
            if ui.add(slider).labelled_by(label.id).changed() {
                let open_db = open_db_for(sens);
                s.params = GateParams {
                    open_db,
                    close_db: open_db - HYSTERESIS_DB,
                    ..s.params
                };
                actions.push(Action::SetParams(s.params));
            }
        });
        ui.add(egui::Label::new(RichText::new(cause(s, status))).wrap());
        calibration_ui(ui, s, status, actions);

        ui.horizontal(|ui| {
            let test = ui
                .add(egui::Button::new("Segure para testar a boca").sense(Sense::click_and_drag()))
                .on_hover_text("Mostra a boca aberta só nesta prévia; o OBS não muda.");
            let held = test.is_pointer_button_down_on()
                || (test.has_focus() && ui.input(|i| i.key_down(egui::Key::Space)));
            s.testing = held;
            ui.label(RichText::new("Prévia apenas").small().color(p.muted));
        });

        egui::CollapsingHeader::new("Ajustes avançados")
            .id_salt("voice-advanced")
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "A boca abre em {:.0} dBFS e fecha abaixo de {:.0} dBFS, depois de {} ms.",
                        s.params.open_db,
                        s.params.close_db,
                        s.params.hold.as_millis()
                    ))
                    .small()
                    .color(p.muted),
                );
            });
    });
}

fn calibration_ui(
    ui: &mut egui::Ui,
    s: &mut ViewState,
    status: &Status,
    actions: &mut Vec<Action>,
) {
    let p = s.palette;
    match s.calibration.clone() {
        Calibration::Idle => {
            let ready = mode(s, status) == Mode::Live;
            let button = ui
                .add_enabled(ready, egui::Button::new("Calibrar sensibilidade"))
                .on_hover_text(
                    "3 s de silêncio e 3 s de fala; só os níveis são usados, nada é gravado",
                )
                .on_disabled_hover_text(
                    "Conecte ao OBS e escolha um microfone que esteja recebendo som",
                );
            if button.clicked() {
                actions.push(Action::StartCalibration);
            }
        }
        Calibration::Silence => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("1/2 · Fique em silêncio por 3 segundos…").strong());
            });
        }
        Calibration::Speech { .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new("2/2 · Agora fale normalmente por 3 segundos, como numa live…")
                        .strong(),
                );
            });
        }
        Calibration::Done(Ok(sug)) => {
            // Plan: dB values stay out of the main screen; they are in the
            // hover text for whoever wants them.
            ui.add(
                egui::Label::new(format!(
                    "Seu silêncio e sua fala ficaram bem separados. Sugestão: sensibilidade {:.0}.",
                    sensitivity(sug.open_db)
                ))
                .wrap(),
            )
            .on_hover_text(format!(
                "Silêncio até {:.0} dBFS; fala em torno de {:.0} dBFS. A boca abriria em {:.0} e fecharia abaixo de {:.0} dBFS.",
                sug.noise_db.max(FLOOR_DB),
                sug.speech_db,
                sug.open_db,
                sug.close_db
            ));
            ui.horizontal(|ui| {
                if ui.add(accent_button(&p, "Aplicar sugestão")).clicked() {
                    s.params = GateParams {
                        open_db: sug.open_db,
                        close_db: sug.close_db,
                        ..s.params
                    };
                    actions.push(Action::SetParams(s.params));
                    s.calibration = Calibration::Idle;
                }
                if ui.button("Cancelar").clicked() {
                    s.calibration = Calibration::Idle;
                }
            });
        }
        Calibration::Done(Err(problem)) => {
            let text = match problem {
                Problem::TooShort => {
                    "Quase nenhum nível chegou do OBS durante a calibração. Confira se o microfone está ativo na cena atual."
                        .to_owned()
                }
                Problem::NoSpeech => {
                    "Não ouvi fala na segunda parte. Tente de novo falando no volume de uma live.".to_owned()
                }
                Problem::TooClose { .. } => {
                    "Sua fala ficou muito perto do ruído do ambiente para uma sugestão confiável. \
                     Ajuste a sensibilidade à mão, ou aproxime o microfone."
                        .to_owned()
                }
            };
            ui.add(egui::Label::new(RichText::new(text).color(p.yellow)).wrap());
            ui.horizontal(|ui| {
                if ui.button("Tentar de novo").clicked() {
                    actions.push(Action::StartCalibration);
                }
                if ui.button("Fechar").clicked() {
                    s.calibration = Calibration::Idle;
                }
            });
        }
    }
}

fn source_picker(
    ui: &mut egui::Ui,
    s: &mut ViewState,
    status: &Status,
    actions: &mut Vec<Action>,
    label: egui::Id,
) {
    let current = match s.selected {
        None => "Escolher microfone".to_owned(),
        Some(_) => {
            source_name(s, status).map_or_else(|| "Microfone salvo".to_owned(), str::to_owned)
        }
    };
    let before = s.selected;
    ui.add_enabled_ui(!status.inputs.is_empty(), |ui| {
        egui::ComboBox::from_id_salt("audio-source")
            .selected_text(current)
            .width(220.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut s.selected, None, "Nenhum");
                for input in &status.inputs {
                    ui.selectable_value(&mut s.selected, Some(input.uuid), &input.name);
                }
            })
            .response
            .labelled_by(label)
            .on_disabled_hover_text("Conecte ao OBS para listar os microfones");
    });
    if s.selected != before {
        actions.push(Action::Select(s.selected));
    }
}

/// Level bar with the open threshold as a draggable mark. Returns new
/// parameters when the user moved it.
fn meter(ui: &mut egui::Ui, p: &Palette, level_db: f32, params: GateParams) -> Option<GateParams> {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click_and_drag());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Slider, true, "Marca da sensibilidade")
    });
    let (labels, _) = ui.allocate_exact_size(vec2(rect.width(), 16.0), Sense::hover());

    let to_x = |db: f32| rect.left() + ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * rect.width();
    let to_db = |x: f32| FLOOR_DB + (x - rect.left()) / rect.width() * -FLOOR_DB;
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(11), p.sunken);
    if level_db > FLOOR_DB {
        let mut bar = rect;
        bar.set_right(to_x(level_db).max(bar.left() + 22.0));
        let fill = if level_db >= params.open_db {
            p.accent
        } else {
            p.muted
        };
        painter.rect_filled(bar, CornerRadius::same(11), fill);
    }
    let x = to_x(params.open_db);
    painter.vline(
        x,
        rect.y_range().expand(4.0),
        Stroke::new(3.0, p.foreground),
    );
    let font = FontId::proportional(12.5);
    painter.text(
        pos2(labels.left(), labels.center().y),
        Align2::LEFT_CENTER,
        "silêncio",
        font.clone(),
        p.muted,
    );
    painter.text(
        pos2(x, labels.center().y),
        Align2::CENTER_CENTER,
        "marca",
        font.clone(),
        p.foreground,
    );
    painter.text(
        pos2(labels.right(), labels.center().y),
        Align2::RIGHT_CENTER,
        "alto",
        font,
        p.muted,
    );

    let response = response.on_hover_cursor(CursorIcon::ResizeHorizontal);
    let pos = response.interact_pointer_pos()?;
    if !(response.dragged() || response.clicked()) {
        return None;
    }
    let open_db = to_db(pos.x).clamp(MIN_OPEN_DB, 0.0).round();
    (open_db != params.open_db).then_some(GateParams {
        open_db,
        close_db: open_db - HYSTERESIS_DB,
        ..params
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_round_trips_and_is_monotonic() {
        assert_eq!(sensitivity(0.0), 0.0);
        assert_eq!(sensitivity(MIN_OPEN_DB), 100.0);
        assert_eq!(open_db_for(100.0), MIN_OPEN_DB);
        assert_eq!(open_db_for(0.0), 0.0);
        assert!(
            open_db_for(80.0) < open_db_for(20.0),
            "more sensitive opens at a lower level"
        );
        assert_eq!(open_db_for(sensitivity(-30.0)), -30.0);
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;
    use eframe::egui::{ColorImage, Context, TextureOptions};

    fn texture(ctx: &Context, name: &str) -> TextureHandle {
        ctx.load_texture(
            name,
            ColorImage::new([1, 1], vec![Color32::RED]),
            TextureOptions::LINEAR,
        )
    }

    #[test]
    fn animated_slot_picks_frame_by_time_and_schedules_repaint() {
        let ctx = Context::default();
        let (a, b) = (texture(&ctx, "a"), texture(&ctx, "b"));
        let ms = Duration::from_millis;
        let slot = SlotView {
            texture: Some(a.clone()),
            frames: vec![(a.clone(), ms(100)), (b.clone(), ms(300))],
            started: 10.0,
            ..Default::default()
        };
        let at = |t: f64| {
            let (tex, next) = slot.texture_at(t).unwrap();
            (tex.id(), next.map(|d| d.as_millis()))
        };
        assert_eq!(at(10.0), (a.id(), Some(100)));
        assert_eq!(at(10.05), (a.id(), Some(50)));
        assert_eq!(at(10.15), (b.id(), Some(250)));
        assert_eq!(at(10.45), (a.id(), Some(50)), "loops after 400 ms");
        assert_eq!(
            at(9.0).0,
            a.id(),
            "clock before start shows the first frame"
        );
    }

    #[test]
    fn still_slot_never_schedules_repaint() {
        let ctx = Context::default();
        let a = texture(&ctx, "a");
        let slot = SlotView {
            texture: Some(a.clone()),
            ..Default::default()
        };
        let (tex, next) = slot.texture_at(123.0).unwrap();
        assert_eq!((tex.id(), next), (a.id(), None));
        assert!(SlotView::default().texture_at(0.0).is_none());
    }
}
