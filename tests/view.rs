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
