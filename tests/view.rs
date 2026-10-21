//! Interaction and state-rule tests for the window, driven through the
//! accessibility tree.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use omatuber::mouth::{GateParams, Mouth};
use omatuber::obs::{AudioInput, Connection, Status};
use omatuber::theme::Palette;
use omatuber::view::{self, Action, Mode, ViewState};
use uuid::Uuid;

fn state() -> ViewState {
    ViewState {
        palette: Palette::default(),
        obs_port: 4455,
        obs_password: String::new(),
        selected: None,
        params: GateParams::default(),
        testing: false,
        slots: Default::default(),
        picking: false,
        server: Ok("http://127.0.0.1:39017/".to_owned()),
        prefs_error: None,
        show_connection: true,
        copied_at: None,
        page: Default::default(),
        remember_password: false,
        scene_choice: None,
        calibration: Default::default(),
        open_offset: [0, 0],
        blink_slots: Default::default(),
        blinking: false,
        secret_note: None,
        library: Default::default(),
    }
}

fn harness(status: Status) -> Harness<'static, (ViewState, Vec<Action>)> {
    Harness::builder()
        .with_size([1100.0, 900.0])
        .build_ui_state(
            move |ui, (view_state, actions): &mut (ViewState, Vec<Action>)| {
                actions.extend(view::draw(ui, view_state, &status));
            },
            (state(), Vec::new()),
        )
}

fn idle() -> Status {
    Status {
        connection: Connection::Idle,
        inputs: Vec::new(),
        level_db: f32::NEG_INFINITY,
        mouth: Mouth::Closed,
        receiving: false,
        ..Default::default()
    }
}

fn live() -> Status {
    Status {
        connection: Connection::Connected,
        inputs: vec![AudioInput {
            uuid: Uuid::from_u128(9),
            name: "Mic".to_owned(),
        }],
        level_db: -20.0,
        mouth: Mouth::Open,
        receiving: true,
        ..Default::default()
    }
}

#[test]
fn connect_button_requests_connection() {
    let mut h = harness(idle());
    h.get_by_label("Conectar").click();
    h.run();
    assert_eq!(h.state().1, [Action::Connect]);
}

#[test]
fn connected_shows_disconnect_instead() {
    let mut h = harness(Status {
        connection: Connection::Connected,
        ..idle()
    });
    h.run();
    assert!(h.query_by_label("Conectar").is_none());
    h.get_by_label("Desconectar").click();
    h.run();
    assert_eq!(h.state().1, [Action::Disconnect]);
}

#[test]
fn clicking_a_thumbnail_picks_that_image() {
    let mut h = harness(idle());
    h.get_by_label("Imagem falando").click();
    h.run();
    assert_eq!(h.state().1, [Action::Pick(Mouth::Open)]);
}

#[test]
fn thumbnails_do_not_pick_while_a_dialog_is_open() {
    let mut h = harness(idle());
    h.state_mut().0.picking = true;
    h.get_by_label("Imagem quieto").click();
    h.run();
    assert!(h.state().1.is_empty());
}

#[test]
fn obs_step_toggles_connection_panel() {
    let mut h = harness(idle());
    h.get_by_label("OBS não conectado. Informe a senha do WebSocket para conectar")
        .click();
    h.run();
    assert!(!h.state().0.show_connection);
    assert!(h.query_by_label("Conectar").is_none());
}

#[test]
fn server_error_offers_retry() {
    let mut h = harness(idle());
    h.state_mut().0.server = Err("porta ocupada".to_owned());
    h.run();
    h.get_by_label("Tentar de novo").click();
    h.run();
    assert_eq!(h.state().1, [Action::RetryServer]);
}

#[test]
fn empty_avatar_offers_the_example() {
    let mut h = harness(idle());
    h.get_by_label("Usar exemplo").click();
    h.run();
    assert_eq!(h.state().1, [Action::UseExample]);
}

#[test]
fn live_requires_connection_selection_and_signal() {
    let mut s = state();
    assert_eq!(
        view::mode(&s, &live()),
        Mode::NoSignal,
        "no source selected"
    );
    s.selected = Some(Uuid::from_u128(9));
    assert_eq!(view::mode(&s, &live()), Mode::Live);
    let silent = Status {
        receiving: false,
        ..live()
    };
    assert_eq!(view::mode(&s, &silent), Mode::NoSignal);
    // After losing OBS the status may still hold the last levels; never "live".
    for connection in [
        Connection::Idle,
        Connection::Connecting,
        Connection::Failed("x".to_owned()),
        Connection::Retrying {
            error: "x".to_owned(),
            after: Duration::from_secs(1),
        },
    ] {
        let lost = Status {
            connection,
            ..live()
        };
        assert_eq!(view::mode(&s, &lost), Mode::NoObs);
        assert_eq!(view::preview_mouth(&s, &lost), Mouth::Closed);
    }
}

#[test]
fn manual_test_is_preview_only() {
    let mut s = state();
    s.testing = true;
    assert_eq!(view::mode(&s, &idle()), Mode::Test);
    assert_eq!(view::preview_mouth(&s, &idle()), Mouth::Open);

    // Pressing the test button must not send anything to the worker/OBS.
    let mut h = harness(idle());
    h.get_by_label("Segure para testar a boca").click();
    h.run();
    assert!(h.state().1.is_empty());
}

#[test]
fn cause_explains_each_state() {
    let mut s = state();
    assert!(view::cause(&s, &idle()).contains("Sem OBS"));
    assert!(view::cause(&s, &live()).contains("Escolha o microfone"));
    s.selected = Some(Uuid::from_u128(9));
    assert!(view::cause(&s, &live()).contains("abriu a boca"));
    let quiet = Status {
        level_db: -55.0,
        mouth: Mouth::Closed,
        ..live()
    };
    assert!(view::cause(&s, &quiet).contains("ainda não passou da marca"));
    let muted = Status {
        level_db: f32::NEG_INFINITY,
        mouth: Mouth::Closed,
        ..live()
    };
    assert!(view::cause(&s, &muted).contains("silenciado"));
    let gone = Status {
        receiving: false,
        ..live()
    };
    assert!(view::cause(&s, &gone).contains("ativa na cena"));
    s.selected = Some(Uuid::from_u128(1));
    assert!(view::cause(&s, &live()).contains("não existe mais"));
}

mod library_editor {
    use super::*;
    use omatuber::library::{Library, Sentiment, Source};
    use omatuber::view::Page;

    fn harness_on_library(lib: Library) -> Harness<'static, (ViewState, Vec<Action>)> {
        let mut h = harness(idle());
        let s = &mut h.state_mut().0;
        s.page = Page::Library;
        s.library.draft = lib.clone();
        s.library.applied = lib;
        h.run();
        h
    }

    #[test]
    fn edits_stay_in_draft_until_applied_and_can_be_undone() {
        let mut h = harness_on_library(Library::default());
        h.get_by_label("Adicionar sentimento").click();
        h.run();
        assert_eq!(h.state().0.library.draft.sentiments.len(), 1);
        assert!(h.state().0.library.dirty());
        assert!(h.state().1.is_empty(), "nothing sent before Aplicar");

        h.get_by_label("Desfazer").click();
        h.run();
        assert!(!h.state().0.library.dirty());
        assert!(h.state().0.library.draft.sentiments.is_empty());

        h.get_by_label("Adicionar sentimento").click();
        h.run();
        h.get_by_label("Aplicar").click();
        h.run();
        assert_eq!(h.state().1, [Action::ApplyLibrary]);
    }

    #[test]
    fn show_now_needs_the_sentiment_applied() {
        let applied = Sentiment::new("Animado");
        let id = applied.id;
        let mut h = harness_on_library(Library {
            sentiments: vec![applied],
            objects: vec![],
        });
        h.get_by_label("Adicionar sentimento").click();
        h.run();
        // Two "Mostrar agora" buttons: the applied one works, the new one is disabled.
        let buttons = h.get_all_by_label("Mostrar agora").collect::<Vec<_>>();
        assert_eq!(buttons.len(), 2);
        buttons[1].click();
        h.run();
        assert!(h.state().1.is_empty());
        h.get_all_by_label("Mostrar agora").next().unwrap().click();
        h.run();
        assert_eq!(h.state().1, [Action::TriggerSentiment(id)]);
    }

    #[test]
    fn on_air_banner_offers_return_to_base() {
        let applied = Sentiment::new("Animado");
        let id = applied.id;
        let mut h = harness_on_library(Library {
            sentiments: vec![applied],
            objects: vec![],
        });
        h.state_mut().0.library.active = Some((id, Source::Shortcut));
        h.run();
        h.get_by_label("No ar: Animado · Atalho");
        h.get_by_label("Voltar à base").click();
        h.run();
        assert_eq!(h.state().1, [Action::ClearReaction]);
    }

    #[test]
    fn removing_an_object_unlinks_it_from_sentiments() {
        let obj = omatuber::library::Object::new("Caneca", "c.png".into());
        let mut s = Sentiment::new("Animado");
        s.objects = vec![obj.id];
        let mut h = harness_on_library(Library {
            sentiments: vec![s],
            objects: vec![obj],
        });
        h.get_all_by_label("Remover").last().unwrap().click();
        h.run();
        let draft = &h.state().0.library.draft;
        assert!(draft.objects.is_empty());
        assert!(draft.sentiments[0].objects.is_empty());
    }
}

mod teaching {
    use super::*;
    use omatuber::library::{Ending, Library, Sentiment};
    use omatuber::library_view::{TrainingReport, VoiceView};
    use omatuber::view::Page;

    fn harness_with(voice: VoiceView) -> Harness<'static, (ViewState, Vec<Action>)> {
        let mut h = harness(idle());
        let s = &mut h.state_mut().0;
        s.page = Page::Library;
        s.library.voice = voice;
        h.run();
        h
    }

    fn examples(list: &[(&str, usize)]) -> Vec<(String, usize)> {
        list.iter().map(|(c, n)| (c.to_string(), *n)).collect()
    }

    #[test]
    fn record_and_train_follow_example_counts() {
        let mut h = harness_with(VoiceView {
            available: true,
            examples: examples(&[("fala", 0), ("outros", 2), ("risada", 1)]),
            ..Default::default()
        });
        h.get_by_label("Gravar risada").click();
        h.run();
        assert_eq!(h.state().1, [Action::RecordExample("risada".to_owned())]);

        // Only one sound has 2 examples: training stays disabled.
        h.state_mut().1.clear();
        h.get_by_label("Treinar").click();
        h.run();
        assert!(h.state().1.is_empty());

        h.state_mut().0.library.voice.examples = examples(&[("outros", 2), ("risada", 2)]);
        h.run();
        h.get_by_label("Treinar").click();
        h.run();
        assert_eq!(h.state().1, [Action::TrainSounds]);
    }

    #[test]
    fn nothing_can_start_while_recording() {
        let mut h = harness_with(VoiceView {
            available: true,
            examples: examples(&[("outros", 3), ("risada", 3)]),
            recording: Some("risada".to_owned()),
            ..Default::default()
        });
        h.get_by_label("Gravar outros").click();
        h.get_by_label("Treinar").click();
        h.run();
        assert!(h.state().1.is_empty());
    }

    #[test]
    fn report_waits_for_a_decision() {
        let mut h = harness_with(VoiceView {
            available: true,
            examples: examples(&[("outros", 3), ("risada", 3)]),
            report: Some(TrainingReport {
                per_class: vec![("outros".to_owned(), 3, 3), ("risada".to_owned(), 2, 3)],
                warnings: vec!["Grave também \"fala\"".to_owned()],
            }),
            ..Default::default()
        });
        h.get_by_label("Reconheceu 5 de 6 exemplos (cada um testado sem ser usado no treino)");
        h.get_by_label("Usar este modelo").click();
        h.run();
        h.get_by_label("Descartar").click();
        h.run();
        assert_eq!(h.state().1, [Action::AcceptModel, Action::DiscardModel]);
    }

    #[test]
    fn a_sound_bound_sentiment_says_it_lasts_while_the_sound_does() {
        let mut s = Sentiment::new("Rindo");
        s.sound = Some("risada".to_owned());
        s.ending = Ending::After { ms: 1500 };
        let lib = Library {
            sentiments: vec![s],
            objects: vec![],
        };
        let mut h = harness_with(VoiceView {
            available: true,
            classes: vec!["risada".to_owned()],
            ..Default::default()
        });
        h.state_mut().0.library.draft = lib.clone();
        h.state_mut().0.library.applied = lib;
        h.run();
        h.get_by_label("Fica enquanto o som continuar e mais 1,5 s depois.");
    }

    #[test]
    fn new_sound_names_are_validated() {
        let mut h = harness_with(VoiceView {
            available: true,
            examples: examples(&[("risada", 0)]),
            new_class: "Susto".to_owned(),
            ..Default::default()
        });
        h.get_by_label("Adicionar").click();
        h.run();
        let names: Vec<_> = h
            .state()
            .0
            .library
            .voice
            .examples
            .iter()
            .map(|(c, _)| c.clone())
            .collect();
        assert_eq!(names, ["risada", "susto"], "lowercased and added");
        h.state_mut().0.library.voice.new_class = "risada".to_owned();
        h.run();
        h.get_by_label("Adicionar").click();
        h.run();
        assert_eq!(
            h.state().0.library.voice.examples.len(),
            2,
            "duplicate refused"
        );
    }
}

#[test]
fn remember_password_checkbox_emits_its_choice() {
    let mut h = harness(idle());
    h.get_by_label("Lembrar senha neste computador").click();
    h.run();
    h.get_by_label("Lembrar senha neste computador").click();
    h.run();
    assert_eq!(
        h.state().1,
        [Action::SetRemember(true), Action::SetRemember(false)]
    );
}

mod scene_step {
    use super::*;
    use omatuber::scene::{Placement, Scenes};

    fn on_scene(placement: Placement) -> Status {
        Status {
            connection: Connection::Connected,
            scenes: Scenes {
                names: vec!["Principal".to_owned(), "Intervalo".to_owned()],
                program: Some("Principal".to_owned()),
            },
            placement: Some(("Principal".to_owned(), placement)),
            ..idle()
        }
    }

    #[test]
    fn not_added_offers_adding_to_the_named_scene() {
        let mut h = harness(on_scene(Placement::NotAdded));
        h.run();
        h.get_by_label("Adicionar à cena Principal").click();
        h.run();
        assert_eq!(h.state().1, [Action::AddToScene]);
    }

    #[test]
    fn hidden_offers_showing_and_visible_offers_nothing() {
        let mut h = harness(on_scene(Placement::Hidden));
        h.run();
        h.get_by_label("Mostrar no OBS").click();
        h.run();
        assert_eq!(h.state().1, [Action::AddToScene]);

        let mut h = harness(on_scene(Placement::Visible));
        h.run();
        h.get_by_label("Visível na cena Principal. Avatar adicionado ao OBS");
        assert!(h.query_by_label("Mostrar no OBS").is_none());
        assert!(h.query_by_label("Adicionar à cena Principal").is_none());
    }

    #[test]
    fn disconnected_never_claims_a_scene_state() {
        let mut h = harness(Status {
            connection: Connection::Idle,
            ..on_scene(Placement::Visible)
        });
        h.run();
        assert!(
            h.query_by_label("Visível na cena Principal. Avatar adicionado ao OBS")
                .is_none()
        );
        h.get_by_label("Na cena: não verificado. Conecte ao OBS para adicionar o avatar sozinho, ou copie a URL");
    }
}

mod calibration_flow {
    use super::*;
    use omatuber::view::Calibration;

    fn live_on_mic() -> (Status, Uuid) {
        let mic = Uuid::from_u128(9);
        (live(), mic)
    }

    #[test]
    fn needs_live_signal_to_start() {
        let mut h = harness(idle());
        h.get_by_label("Calibrar sensibilidade").click();
        h.run();
        assert!(h.state().1.is_empty(), "disabled without OBS");

        let (status, mic) = live_on_mic();
        let mut h = harness(status);
        h.state_mut().0.selected = Some(mic);
        h.run();
        h.get_by_label("Calibrar sensibilidade").click();
        h.run();
        assert_eq!(h.state().1, [Action::StartCalibration]);
    }

    #[test]
    fn advances_silence_then_speech_then_suggests() {
        let silence: Vec<f32> = (0..60).map(|i| -55.0 + (i % 5) as f32 - 2.0).collect();
        let speech: Vec<f32> = (0..60).map(|i| -20.0 + (i % 5) as f32 - 2.0).collect();
        let (step, again) = Calibration::Silence.advance(silence.clone());
        assert!(again, "speech phase still to gather");
        assert_eq!(step, Calibration::Speech { silence });
        let (step, again) = step.advance(speech);
        assert!(!again);
        let Calibration::Done(Ok(s)) = step else {
            panic!("{step:?}")
        };
        assert_eq!((s.open_db, s.close_db), (-42.0, -50.0));
        // A stray batch while idle changes nothing.
        assert_eq!(
            Calibration::Idle.advance(vec![-1.0]),
            (Calibration::Idle, false)
        );
    }

    #[test]
    fn applying_the_suggestion_sets_both_thresholds() {
        let (status, mic) = live_on_mic();
        let mut h = harness(status);
        let s = &mut h.state_mut().0;
        s.selected = Some(mic);
        s.calibration = Calibration::Done(Ok(omatuber::calibration::Suggestion {
            open_db: -42.0,
            close_db: -50.0,
            noise_db: -53.0,
            speech_db: -20.0,
        }));
        h.run();
        h.get_by_label("Aplicar sugestão").click();
        h.run();
        let st = h.state();
        assert_eq!(st.0.calibration, Calibration::Idle);
        assert_eq!((st.0.params.open_db, st.0.params.close_db), (-42.0, -50.0));
        assert_eq!(st.1, [Action::SetParams(st.0.params)]);
    }

    #[test]
    fn problems_offer_retry_instead_of_a_guess() {
        let (status, mic) = live_on_mic();
        let mut h = harness(status);
        let s = &mut h.state_mut().0;
        s.selected = Some(mic);
        s.calibration = Calibration::Done(Err(omatuber::calibration::Problem::NoSpeech));
        h.run();
        assert!(h.query_by_label("Aplicar sugestão").is_none());
        h.get_by_label("Tentar de novo").click();
        h.run();
        assert_eq!(h.state().1, [Action::StartCalibration]);
    }
}

#[test]
fn muted_microphone_is_named_as_such() {
    let mut s = state();
    s.selected = Some(Uuid::from_u128(9));
    let muted = Status {
        level_db: f32::NEG_INFINITY,
        mouth: Mouth::Closed,
        muted: Some(true),
        ..live()
    };
    assert!(view::cause(&s, &muted).contains("está silenciado no OBS"));
    let not_muted = Status {
        muted: Some(false),
        ..muted.clone()
    };
    assert!(view::cause(&s, &not_muted).contains("não esteja silenciado"));
    let unknown = Status {
        muted: None,
        ..muted.clone()
    };
    assert!(view::cause(&s, &unknown).contains("pode estar silenciado"));

    let mut h = harness(muted);
    h.state_mut().0.selected = Some(Uuid::from_u128(9));
    h.run();
    h.get_by_label("Microfone silenciado. Ative Mic no mixer de áudio do OBS");
}

mod keyboard {
    use super::*;
    use eframe::egui;
    use egui_kittest::kittest::NodeT;

    fn focused_label(h: &Harness<'static, (ViewState, Vec<Action>)>) -> Option<String> {
        h.root()
            .children_recursive()
            .find(|n| n.accesskit_node().is_focused())
            .and_then(|n| n.accesskit_node().label().map(|l| l.to_string()))
    }

    /// Labels reached by pressing Tab from the top, in order.
    fn tab_order(h: &mut Harness<'static, (ViewState, Vec<Action>)>) -> Vec<String> {
        let mut seen = Vec::new();
        for _ in 0..60 {
            h.key_press(egui::Key::Tab);
            h.run();
            match focused_label(h) {
                Some(l) if seen.first() == Some(&l) => break,
                Some(l) => seen.push(l),
                None => {}
            }
        }
        seen
    }

    #[test]
    fn main_actions_are_reachable_with_tab() {
        let mut h = harness(idle());
        h.run();
        let order = tab_order(&mut h);
        for needed in [
            "OBS não conectado. Informe a senha do WebSocket para conectar",
            "Senha",
            "Conectar",
            "Usar exemplo",
            "Imagem quieto",
            "Imagem falando",
            "Copiar URL",
            "Segure para testar a boca",
        ] {
            assert!(
                order.iter().any(|l| l == needed),
                "{needed:?} not reachable; order: {order:?}"
            );
        }
    }

    #[test]
    fn enter_on_a_focused_thumbnail_picks_it() {
        let mut h = harness(idle());
        h.run();
        for _ in 0..60 {
            h.key_press(egui::Key::Tab);
            h.run();
            if focused_label(&h).as_deref() == Some("Imagem falando") {
                break;
            }
        }
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().1, [Action::Pick(Mouth::Open)]);
    }

    #[test]
    fn arrow_keys_move_the_sensitivity_mark() {
        let mut h = harness(idle());
        h.run();
        h.get_by_label("Marca da sensibilidade").focus();
        h.run();
        h.key_press(egui::Key::ArrowRight);
        h.run();
        let before = GateParams::default().open_db;
        assert_eq!(h.state().0.params.open_db, before + 1.0);
        assert_eq!(h.state().1.len(), 1, "one SetParams");
    }

    /// Writes the focused thumbnail to target/tmp for a visual check when
    /// OMATUBER_RENDER_FOCUS is set.
    #[test]
    fn focus_ring_render() {
        let Some(out) = std::env::var_os("OMATUBER_RENDER_FOCUS") else {
            return;
        };
        let mut h = harness(idle());
        h.run();
        h.get_by_label("Imagem falando").focus();
        h.run();
        h.render().unwrap().save(out).unwrap();
    }
}

#[test]
fn alignment_control_appears_for_different_sizes_and_resets() {
    let mut h = harness(idle());
    h.run();
    assert!(
        h.query_by_label("Centralizar").is_none(),
        "hidden when nothing to align"
    );
    {
        let s = &mut h.state_mut().0;
        s.slots[0].size = Some([512, 512]);
        s.slots[1].size = Some([600, 600]);
        s.open_offset = [5, -3];
    }
    h.run();
    h.get_by_label("Centralizar").click();
    h.run();
    assert_eq!(h.state().0.open_offset, [0, 0]);
    assert_eq!(h.state().1, [Action::SetOpenOffset([0, 0])]);
}

#[test]
fn blink_row_picks_and_removes() {
    let mut h = harness(idle());
    h.run();
    h.get_by_label("Escolher Falando piscando").click();
    h.run();
    h.state_mut().0.blink_slots[0].file = Some("olhos.png".to_owned());
    h.run();
    h.get_by_label("Remover Quieto piscando").click();
    h.run();
    assert_eq!(
        h.state().1,
        [
            Action::PickBlink(Mouth::Open),
            Action::ClearBlink(Mouth::Closed)
        ]
    );
}

#[test]
fn silence_trigger_can_be_set_per_sentiment() {
    use omatuber::library::{Library, Sentiment};
    use omatuber::view::Page;
    let mut h = harness(idle());
    {
        let s = &mut h.state_mut().0;
        s.page = Page::Library;
        let lib = Library {
            sentiments: vec![Sentiment::new("Dormindo")],
            objects: vec![],
        };
        s.library.draft = lib.clone();
        s.library.applied = lib;
    }
    h.run();
    h.get_by_label("Aparece após silêncio").click();
    h.run();
    assert_eq!(
        h.state().0.library.draft.sentiments[0].after_silence_secs,
        Some(60)
    );
    assert!(h.state().0.library.dirty(), "takes effect on Aplicar");
}
