//! Interaction tests for the window, driven through the accessibility tree.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use omatuber::mouth::{GateParams, Mouth};
use omatuber::obs::{Connection, Status};
use omatuber::theme::Palette;
use omatuber::view::{self, Action, ViewState};

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
    }
}

fn harness(status: Status) -> Harness<'static, (ViewState, Vec<Action>)> {
    Harness::builder().with_size([640.0, 700.0]).build_ui_state(
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
fn clicking_a_card_picks_that_image() {
    let mut h = harness(idle());
    h.get_by_label("Imagem falando").click();
    h.run();
    assert_eq!(h.state().1, [Action::Pick(Mouth::Open)]);
}

#[test]
fn cards_do_not_pick_while_a_dialog_is_open() {
    let mut h = harness(idle());
    h.state_mut().0.picking = true;
    h.get_by_label("Imagem quieto").click();
    h.run();
    assert!(h.state().1.is_empty());
}

#[test]
fn status_chip_toggles_connection_panel() {
    let mut h = harness(idle());
    h.get_by_label("OBS desconectado").click();
    h.run();
    assert!(!h.state().0.show_connection);
    assert!(h.query_by_label("Conectar").is_none());
}

#[test]
fn server_error_offers_retry() {
    let mut h = harness(idle());
    h.state_mut().0.server = Err("porta ocupada".to_owned());
    h.run();
    h.get_by_label("Servidor local parado · tentar de novo")
        .click();
    h.run();
    assert_eq!(h.state().1, [Action::RetryServer]);
}
