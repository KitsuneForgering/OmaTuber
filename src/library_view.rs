//! "Reações e objetos": editor for the sentiment/object library.
//!
//! Edits go to a draft; nothing reaches OBS until **Aplicar**, and
//! **Desfazer** returns to the applied version (plan: "permitir desfazer uma
//! edição ainda não aplicada"). Kept off the main screen until the basic flow
//! has been observed with users, as the plan asks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, TextureHandle, Vec2, pos2, vec2,
};
use uuid::Uuid;

use crate::library::{Ending, Library, Object, Sentiment, Source};
use crate::theme::Palette;
use crate::view::{Action, ViewState};

/// Which file a picker result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileTarget {
    Quiet(Uuid),
    Talking(Uuid),
    Object(Uuid),
}

#[derive(Default)]
pub struct LibraryView {
    pub draft: Library,
    /// What the reactions task is using.
    pub applied: Library,
    /// From the reactions task: per item, why its file is not in use.
    pub errors: HashMap<Uuid, String>,
    pub active: Option<(Uuid, Source)>,
    pub objects_on: Vec<Uuid>,
    /// First frame of each library file, for thumbnails and the preview.
    pub textures: HashMap<PathBuf, (TextureHandle, [u32; 2])>,
    pub save_error: Option<String>,
    pub voice: VoiceView,
}

/// Sound detection as the editor shows it; filled by `main` from the voice
/// thread. `available` is false when built without the `sound` feature.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoiceView {
    pub available: bool,
    pub enabled: bool,
    pub running: bool,
    pub error: Option<String>,
    /// Trained classes a sentiment can be bound to.
    pub classes: Vec<String>,
    pub heard: Option<(String, f32)>,
}

impl LibraryView {
    pub fn dirty(&self) -> bool {
        self.draft != self.applied
    }

    /// Stores a picked file in the draft.
    pub fn set_file(&mut self, target: FileTarget, path: PathBuf) {
        match target {
            FileTarget::Quiet(id) => {
                if let Some(s) = self.draft.sentiments.iter_mut().find(|s| s.id == id) {
                    s.quiet = Some(path);
                }
            }
            FileTarget::Talking(id) => {
                if let Some(s) = self.draft.sentiments.iter_mut().find(|s| s.id == id) {
                    s.talking = Some(path);
                }
            }
            FileTarget::Object(id) => {
                if let Some(o) = self.draft.objects.iter_mut().find(|o| o.id == id) {
                    o.file = path;
                }
            }
        }
    }
}

pub fn source_label(source: Source) -> &'static str {
    match source {
        Source::Manual => "Manual",
        Source::Shortcut => "Atalho",
        Source::Voice => "Acionado por voz",
        Source::Test => "Teste",
    }
}

pub fn draw(ui: &mut egui::Ui, s: &mut ViewState, actions: &mut Vec<Action>) {
    let p = s.palette;
    let lib = &mut s.library;

    ui.horizontal_wrapped(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new("Reações e objetos").size(18.0).strong());
            ui.label(
                RichText::new("Sentimentos trocam a expressão por um tempo; objetos aparecem por cima do avatar.")
                    .color(p.muted),
            );
        });
    });
    ui.horizontal_wrapped(|ui| {
        let dirty = lib.dirty();
        if ui.add_enabled(dirty, accent(&p, "Aplicar")).clicked() {
            actions.push(Action::ApplyLibrary);
        }
        if ui
            .add_enabled(dirty, egui::Button::new("Desfazer"))
            .clicked()
        {
            lib.draft = lib.applied.clone();
        }
        if dirty {
            ui.label(RichText::new("Alterações ainda não aplicadas").color(p.yellow));
        }
        if let Some((id, source)) = lib.active {
            let name = lib.applied.sentiment(id).map_or("?", |s| s.name.as_str());
            ui.label(
                RichText::new(format!("No ar: {name} · {}", source_label(source))).color(p.green),
            );
            if ui.button("Voltar à base").clicked() {
                actions.push(Action::ClearReaction);
            }
        }
    });
    if let Some(e) = &lib.save_error {
        ui.add(egui::Label::new(RichText::new(e.as_str()).color(p.red)).wrap());
    }
    ui.add_space(8.0);

    if lib.voice.available {
        voice_section(ui, &p, &mut lib.voice, actions);
        ui.add_space(8.0);
    }

    // ------------------------------------------------------------ sentiments
    ui.horizontal(|ui| {
        ui.label(RichText::new("Sentimentos").size(16.0).strong());
        if ui.button("Adicionar sentimento").clicked() {
            let n = lib.draft.sentiments.len() + 1;
            lib.draft
                .sentiments
                .push(Sentiment::new(format!("Sentimento {n}")));
        }
    });
    if lib.draft.sentiments.is_empty() {
        ui.label(
            RichText::new("Nenhum sentimento ainda. Um sentimento é uma expressão sua, como Animado ou Cansado, com uma imagem para quando está quieto e, se quiser, outra para quando fala.")
                .color(p.muted),
        );
    }
    let objects: Vec<(Uuid, String)> = lib
        .draft
        .objects
        .iter()
        .map(|o| (o.id, o.name.clone()))
        .collect();
    let mut remove = None;
    for (i, sentiment) in lib.draft.sentiments.iter_mut().enumerate() {
        let applied_name = lib
            .applied
            .sentiments
            .iter()
            .find(|a| a.id == sentiment.id)
            .map(|a| a.name.clone());
        let applied = applied_name.is_some();
        let on_air = lib.active.map(|(id, _)| id) == Some(sentiment.id);
        card(&p, on_air).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.push_id(sentiment.id, |ui| {
                ui.horizontal(|ui| {
                    thumb(
                        ui,
                        &p,
                        sentiment.quiet.as_deref().and_then(|f| lib.textures.get(f)),
                    );
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let label = ui.label("Nome");
                            ui.add(
                                egui::TextEdit::singleline(&mut sentiment.name)
                                    .desired_width(180.0),
                            )
                            .labelled_by(label.id);
                            ui.checkbox(&mut sentiment.enabled, "Ativo");
                        });
                        ui.horizontal_wrapped(|ui| {
                            let show = ui
                                .add_enabled(
                                    applied && sentiment.enabled,
                                    accent(&p, "Mostrar agora"),
                                )
                                .on_hover_text("Aparece também no OBS")
                                .on_disabled_hover_text("Aplique as alterações primeiro");
                            if show.clicked() {
                                actions.push(Action::TriggerSentiment(sentiment.id));
                            }
                            if ui.button("Remover").clicked() {
                                remove = Some(i);
                            }
                            if on_air {
                                ui.label(RichText::new("No ar").color(p.green));
                            }
                        });
                    });
                });
                file_row(
                    ui,
                    &p,
                    "Quieto",
                    sentiment.quiet.as_deref(),
                    FileTarget::Quiet(sentiment.id),
                    actions,
                );
                ui.horizontal(|ui| {
                    file_row(
                        ui,
                        &p,
                        "Falando",
                        sentiment.talking.as_deref(),
                        FileTarget::Talking(sentiment.id),
                        actions,
                    );
                    if sentiment.talking.is_some()
                        && ui.small_button("Sem imagem falando").clicked()
                    {
                        sentiment.talking = None;
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    let mut until_next = sentiment.ending == Ending::UntilNext;
                    if ui.checkbox(&mut until_next, "Fica até o próximo").changed() {
                        sentiment.ending = if until_next {
                            Ending::UntilNext
                        } else {
                            Ending::After { ms: 3000 }
                        };
                    }
                    if let Ending::After { ms } = &mut sentiment.ending {
                        let mut secs = *ms as f32 / 1000.0;
                        let label = ui.label("Duração (s)");
                        if ui
                            .add(
                                egui::DragValue::new(&mut secs)
                                    .range(0.1..=600.0)
                                    .speed(0.1)
                                    .fixed_decimals(1),
                            )
                            .labelled_by(label.id)
                            .changed()
                        {
                            *ms = (secs * 1000.0).round() as u32;
                        }
                    }
                    let label = ui.label("Prioridade");
                    ui.add(egui::DragValue::new(&mut sentiment.priority).range(0..=9))
                        .labelled_by(label.id)
                        .on_hover_text(
                            "Um sentimento de prioridade maior não é interrompido por um menor",
                        );
                });
                if lib.voice.available {
                    ui.horizontal(|ui| {
                        let label = ui.label("Som que aciona");
                        let current = sentiment.sound.clone().unwrap_or_else(|| "nenhum".to_owned());
                        egui::ComboBox::from_id_salt("sound")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut sentiment.sound, None, "nenhum");
                                for class in &lib.voice.classes {
                                    ui.selectable_value(&mut sentiment.sound, Some(class.clone()), class);
                                }
                            })
                            .response
                            .labelled_by(label.id);
                        if let Some(class) = &sentiment.sound
                            && !lib.voice.classes.is_empty()
                            && !lib.voice.classes.contains(class)
                        {
                            ui.label(RichText::new("esse som não está no modelo treinado").color(p.yellow));
                        }
                    });
                }
                if !objects.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Objetos junto:");
                        for (id, name) in &objects {
                            let mut with = sentiment.objects.contains(id);
                            if ui.checkbox(&mut with, name.as_str()).changed() {
                                if with {
                                    sentiment.objects.push(*id);
                                } else {
                                    sentiment.objects.retain(|o| o != id);
                                }
                            }
                        }
                    });
                }
                // The command resolves names in the applied library.
                let hint = match &applied_name {
                    Some(name) if *name == sentiment.name => {
                        format!("Atalho no Hyprland: omatuber reagir \"{name}\"")
                    }
                    Some(name) => format!(
                        "Atalho no Hyprland: omatuber reagir \"{name}\" (o novo nome vale depois de aplicar)"
                    ),
                    None => "O atalho funciona depois de aplicar".to_owned(),
                };
                ui.label(RichText::new(hint).monospace().small().color(p.muted));
                if let Some(e) = lib.errors.get(&sentiment.id) {
                    ui.add(egui::Label::new(RichText::new(e.as_str()).color(p.red)).wrap());
                }
            });
        });
        ui.add_space(6.0);
    }
    if let Some(i) = remove {
        lib.draft.sentiments.remove(i);
    }

    // --------------------------------------------------------------- objects
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Objetos").size(16.0).strong());
        if ui.button("Adicionar objeto").clicked() {
            let n = lib.draft.objects.len() + 1;
            lib.draft
                .objects
                .push(Object::new(format!("Objeto {n}"), PathBuf::new()));
        }
    });
    if lib.draft.objects.is_empty() {
        ui.label(RichText::new("Nenhum objeto ainda. Objetos são imagens por cima do avatar, como uma caneca ou óculos.").color(p.muted));
    }
    let mut remove = None;
    let mut removed_ids = Vec::new();
    for (i, object) in lib.draft.objects.iter_mut().enumerate() {
        let applied = lib.applied.objects.iter().any(|a| a.id == object.id);
        let on = lib.objects_on.contains(&object.id);
        card(&p, on).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.push_id(object.id, |ui| {
                ui.horizontal(|ui| {
                    let file =
                        (!object.file.as_os_str().is_empty()).then_some(object.file.as_path());
                    thumb(ui, &p, file.and_then(|f| lib.textures.get(f)));
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let label = ui.label("Nome");
                            ui.add(
                                egui::TextEdit::singleline(&mut object.name).desired_width(180.0),
                            )
                            .labelled_by(label.id);
                            ui.checkbox(&mut object.enabled, "Ativo");
                        });
                        ui.horizontal(|ui| {
                            let text = if on { "Ocultar" } else { "Mostrar" };
                            if ui
                                .add_enabled(applied && object.enabled, egui::Button::new(text))
                                .on_disabled_hover_text("Aplique as alterações primeiro")
                                .clicked()
                            {
                                actions.push(Action::ToggleObject(object.id));
                            }
                            if ui.button("Remover").clicked() {
                                remove = Some(i);
                                removed_ids.push(object.id);
                            }
                        });
                    });
                });
                let file = (!object.file.as_os_str().is_empty()).then_some(object.file.as_path());
                file_row(
                    ui,
                    &p,
                    "Imagem",
                    file,
                    FileTarget::Object(object.id),
                    actions,
                );
                ui.horizontal_wrapped(|ui| {
                    for (label, value, range) in [
                        ("Horizontal", &mut object.x, -0.5..=1.5),
                        ("Vertical", &mut object.y, -0.5..=1.5),
                        ("Tamanho", &mut object.scale, 0.02..=2.0),
                    ] {
                        let l = ui.label(label);
                        ui.add(
                            egui::DragValue::new(value)
                                .range(range)
                                .speed(0.005)
                                .fixed_decimals(2),
                        )
                        .labelled_by(l.id);
                    }
                });
                if let Some(e) = lib.errors.get(&object.id) {
                    ui.add(egui::Label::new(RichText::new(e.as_str()).color(p.red)).wrap());
                }
            });
        });
        ui.add_space(6.0);
    }
    if let Some(i) = remove {
        lib.draft.objects.remove(i);
        for s in &mut lib.draft.sentiments {
            s.objects.retain(|o| !removed_ids.contains(o));
        }
    }
}

fn voice_section(ui: &mut egui::Ui, p: &Palette, voice: &mut VoiceView, actions: &mut Vec<Action>) {
    card(p, voice.running).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Reagir a sons").size(16.0).strong());
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Reconhece sons que você ensinou, como sua risada, e mostra o sentimento ligado a eles. \
                     Lê o microfone pelo PipeWire; o áudio é analisado na memória e descartado, nada é gravado.",
                )
                .color(p.muted),
            )
            .wrap(),
        );
        let mut enabled = voice.enabled;
        if ui.checkbox(&mut enabled, "Ouvir o microfone").changed() {
            voice.enabled = enabled;
            actions.push(Action::SetVoice(enabled));
        }
        if let Some(e) = &voice.error {
            ui.add(egui::Label::new(RichText::new(e.as_str()).color(p.red)).wrap());
        } else if voice.running {
            let heard = match &voice.heard {
                Some((class, prob)) => format!("Ouvindo · agora: {class} ({:.0}%)", prob * 100.0),
                None => "Ouvindo · silêncio".to_owned(),
            };
            ui.label(RichText::new(heard).color(p.green));
        }
        if !voice.classes.is_empty() {
            ui.label(
                RichText::new(format!("Sons treinados: {}", voice.classes.join(", ")))
                    .small()
                    .color(p.muted),
            );
        }
    });
}

fn accent(p: &Palette, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(p.on_accent)).fill(p.accent)
}

fn card(p: &Palette, highlighted: bool) -> egui::Frame {
    let stroke = if highlighted {
        egui::Stroke::new(2.0, p.accent)
    } else {
        egui::Stroke::new(1.0, p.surface)
    };
    egui::Frame::new()
        .fill(p.surface.gamma_multiply(0.6))
        .stroke(stroke)
        .corner_radius(12)
        .inner_margin(12)
}

fn file_row(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    file: Option<&Path>,
    target: FileTarget,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.label(format!("{label}:"));
        let name = file.and_then(|f| f.file_name()).map_or_else(
            || "nenhuma imagem".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        ui.label(RichText::new(name).color(if file.is_some() {
            p.foreground
        } else {
            p.muted
        }));
        let pick = ui.small_button("Escolher");
        pick.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Escolher imagem {label}"))
        });
        if pick.clicked() {
            actions.push(Action::PickLibraryFile(target));
        }
    });
}

/// 56 px checkerboard thumbnail of a library image, or an empty square.
fn thumb(ui: &mut egui::Ui, p: &Palette, texture: Option<&(TextureHandle, [u32; 2])>) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(8), p.sunken);
    const SQ: f32 = 8.0;
    for y in 0..7 {
        for x in (y % 2..7).step_by(2) {
            let min = rect.min + vec2(x as f32 * SQ, y as f32 * SQ);
            painter.rect_filled(Rect::from_min_size(min, Vec2::splat(SQ)), 0.0, p.checker);
        }
    }
    if let Some((texture, [w, h])) = texture {
        let scale = (rect.width() / *w as f32).min(rect.height() / *h as f32);
        let img = Rect::from_center_size(rect.center(), vec2(*w as f32, *h as f32) * scale);
        painter.image(
            texture.id(),
            img,
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}
