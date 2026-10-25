//! Tests de l'interface terminal, branchés sur le faux serveur Ollama.

use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEvent, MouseEventKind,
};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use tokio::sync::oneshot;

use super::app::{App, Reply, Role};
use super::event_loop;
use crate::agent_actor::AgentError;
use crate::harness::guard::Approval;
use crate::test_support::start_agent;

/// Événement « touche pressée ».
fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Interface de test branchée sur le faux Ollama, avec le récepteur des réponses.
fn app() -> (App, UnboundedReceiver<Reply>) {
    let (agent, config) = start_agent();
    let (tx, rx) = unbounded_channel();
    (App::new(agent, config, tx), rx)
}

/// Saisit `text` caractère par caractère.
fn type_text(app: &mut App, text: &str) {
    text.chars()
        .for_each(|c| _ = app.on_key(press(KeyCode::Char(c))));
}

/// Dessine `app` sur un terminal virtuel et renvoie l'écran sous forme de texte.
fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect()
}

/// Échap et Ctrl+C quittent ; les autres touches non gérées sont ignorées.
#[actix_web::test]
async fn quit_keys() {
    let (mut app, _) = app();
    assert!(!app.on_key(press(KeyCode::Esc)));
    assert!(!app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    assert!(app.on_key(press(KeyCode::Tab)));
}

/// Saisie et effacement de caractères.
#[actix_web::test]
async fn editing_input() {
    let (mut app, _) = app();
    type_text(&mut app, "abc");
    app.on_key(press(KeyCode::Backspace));
    assert_eq!(app.input, "ab");
}

/// Les touches de défilement modifient le décalage sans passer sous zéro.
#[actix_web::test]
async fn scrolling_keys() {
    let (mut app, _) = app();
    app.on_key(press(KeyCode::PageUp));
    assert_eq!(app.scroll_up, 10);
    app.on_key(press(KeyCode::PageDown));
    app.on_key(press(KeyCode::PageDown));
    assert_eq!(app.scroll_up, 0);
}

/// La molette fait défiler l'historique sans passer sous zéro ; le reste est ignoré.
#[actix_web::test]
async fn mouse_wheel_scrolls_history() {
    let (mut app, _) = app();
    app.on_mouse(MouseEventKind::ScrollUp);
    app.on_mouse(MouseEventKind::ScrollUp);
    app.on_mouse(MouseEventKind::ScrollDown);
    assert_eq!(app.scroll_up, 3);
    app.on_mouse(MouseEventKind::Moved);
    app.on_mouse(MouseEventKind::ScrollDown);
    app.on_mouse(MouseEventKind::ScrollDown);
    assert_eq!(app.scroll_up, 0);
}

/// ↑ / ↓ parcourent les messages envoyés, bornés aux deux extrémités.
#[actix_web::test]
async fn arrows_recall_sent_messages() {
    let (mut app, _) = app();
    app.on_key(press(KeyCode::Up));
    app.on_key(press(KeyCode::Down));
    assert!(app.input.is_empty(), "rien à rappeler");
    for msg in ["/model a", "/model b"] {
        type_text(&mut app, msg);
        app.submit();
    }
    app.on_key(press(KeyCode::Up));
    assert_eq!(app.input, "/model b");
    app.on_key(press(KeyCode::Up));
    app.on_key(press(KeyCode::Up));
    assert_eq!(app.input, "/model a");
    app.on_key(press(KeyCode::Down));
    assert_eq!(app.input, "/model b");
    app.on_key(press(KeyCode::Down));
    assert!(app.input.is_empty() && app.recall.is_none());
}

/// Entrée envoie le message, passe en génération puis affiche la réponse de l'agent.
#[actix_web::test]
async fn submit_sends_and_receives_reply() {
    let (mut app, mut rx) = app();
    type_text(&mut app, "  salut ");
    app.on_key(press(KeyCode::Enter));
    assert!(app.busy && app.input.is_empty());
    let reply = rx.recv().await.unwrap();
    app.on_reply(reply);
    assert!(!app.busy);
    assert!(
        matches!(app.history.as_slice(), [(Role::User, u), (Role::Agent, a)] if u == "salut" && a == "2 messages : salut")
    );
}

/// Saisie vide ou génération en cours : rien n'est envoyé.
#[actix_web::test]
async fn submit_is_ignored_when_empty_or_busy() {
    let (mut app, _) = app();
    type_text(&mut app, "   ");
    app.submit();
    assert!(app.history.is_empty());
    app.busy = true;
    type_text(&mut app, "x");
    app.submit();
    assert!(app.history.is_empty());
}

/// Une erreur de l'agent s'affiche comme entrée d'erreur.
#[actix_web::test]
async fn error_reply_is_shown() {
    let (mut app, _) = app();
    app.on_reply(Reply::Answer(Err(AgentError::Timeout)));
    assert!(matches!(app.history.as_slice(), [(Role::Error, e)] if e.contains("délai")));
}

/// `/clear` vide l'historique local et celui de l'agent, puis annonce le mode d'outils redétecté.
#[actix_web::test]
async fn clear_command_resets_history() {
    let (mut app, mut rx) = app();
    type_text(&mut app, "un");
    app.submit();
    app.on_reply(rx.recv().await.unwrap());
    type_text(&mut app, "/clear");
    app.submit();
    assert!(app.history.is_empty());
    app.on_reply(rx.recv().await.unwrap());
    assert!(matches!(app.history.as_slice(), [(Role::System, m)] if m == "Outils : appel natif"));
    type_text(&mut app, "un");
    app.submit();
    assert!(matches!(rx.recv().await.unwrap(), Reply::Answer(Ok(a)) if a == "2 messages : un"));
}

/// `/model` et `/preamble` mettent à jour la configuration et l'annoncent.
#[actix_web::test]
async fn config_commands_update_config() {
    let (mut app, _) = app();
    type_text(&mut app, "/model qwen");
    app.submit();
    type_text(&mut app, "/preamble Sois bref.");
    app.submit();
    let cfg = &app.config;
    assert_eq!(
        (cfg.model.as_str(), cfg.preamble.as_str()),
        ("qwen", "Sois bref.")
    );
    assert!(matches!(
        app.history.as_slice(),
        [(Role::System, _), (Role::System, _)]
    ));
}

/// Le rendu montre les rôles, la saisie et le statut correspondant à l'état.
#[actix_web::test]
async fn draw_shows_history_input_and_status() {
    let (mut app, _) = app();
    app.history = vec![
        (Role::User, "question".into()),
        (Role::Agent, "réponse".into()),
        (Role::Error, "panne".into()),
        (Role::System, "info".into()),
    ];
    app.input = "brouillon".into();
    let screen = render(&mut app, 60, 20);
    for text in [
        "Vous",
        "Agent",
        "Erreur",
        "Système",
        "réponse",
        "brouillon",
        "En attente",
    ] {
        assert!(screen.contains(text), "{text} absent de l'écran");
    }
    app.busy = true;
    assert!(render(&mut app, 60, 20).contains("Génération en cours"));
}

/// Le défilement reste ancré en bas et est borné par la taille de l'historique.
#[actix_web::test]
async fn draw_scrolls_from_bottom() {
    let (mut app, _) = app();
    app.history = (0..30)
        .map(|i| (Role::Agent, format!("ligne{i:02}")))
        .collect();
    assert!(render(&mut app, 40, 12).contains("ligne29"));
    app.scroll_up = 1000;
    let screen = render(&mut app, 40, 12);
    assert!(screen.contains("ligne00") && !screen.contains("ligne29"));
    assert!(app.scroll_up < 1000);
}

/// Boucle complète : saisie, réponse affichée, événements ignorés, puis Échap.
#[actix_web::test]
async fn event_loop_end_to_end() {
    let (agent, config) = start_agent();
    let (keys, rx) = unbounded_channel();
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    let driver = actix_web::rt::spawn(async move {
        for c in "salut".chars() {
            keys.send(Event::Key(press(KeyCode::Char(c)))).unwrap();
        }
        keys.send(Event::Key(press(KeyCode::Enter))).unwrap();
        keys.send(Event::Resize(60, 20)).unwrap();
        let wheel = MouseEvent { kind: MouseEventKind::ScrollUp, column: 0, row: 0, modifiers: KeyModifiers::NONE };
        keys.send(Event::Mouse(wheel)).unwrap();
        let mut release = press(KeyCode::Char('x'));
        (release.kind, release.state) = (KeyEventKind::Release, KeyEventState::NONE);
        keys.send(Event::Key(release)).unwrap();
        actix_web::rt::time::sleep(Duration::from_millis(500)).await;
        keys.send(Event::Key(press(KeyCode::Esc))).unwrap();
    });
    event_loop(&mut terminal, rx, unbounded_channel().1, agent, config).await.unwrap();
    driver.await.unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("2 messages : salut"));
    assert!(
        !screen.contains("salutx"),
        "la touche relâchée ne doit pas être saisie"
    );
}

/// La boucle s'arrête proprement si la source d'événements se ferme.
#[actix_web::test]
async fn event_loop_stops_when_input_closes() {
    let (agent, config) = start_agent();
    let (keys, rx) = unbounded_channel::<Event>();
    drop(keys);
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
    assert!(event_loop(&mut terminal, rx, unbounded_channel().1, agent, config).await.is_ok());
}

/// Une demande d'autorisation s'affiche ; `o` autorise, une autre touche refuse.
#[actix_web::test]
async fn approval_is_answered_by_key() {
    let (mut app, _) = app();
    for (key, expected) in [('o', true), ('x', false)] {
        let (reply, answer) = oneshot::channel();
        app.on_approval(Approval { request: "run_bash rm x".into(), reply });
        assert!(render(&mut app, 80, 20).contains("Autoriser cet appel"));
        type_text(&mut app, &key.to_string());
        assert_eq!(answer.await.unwrap(), expected);
        assert!(app.pending.is_none() && app.input.is_empty());
    }
    let notes: Vec<_> = app.history.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(notes[1..], ["Autorisé : run_bash rm x", "Autorisation demandée : run_bash rm x", "Refusé : run_bash rm x"]);
}

/// La boucle transmet les demandes d'autorisation à l'interface.
#[actix_web::test]
async fn event_loop_forwards_approvals() {
    let (agent, config) = start_agent();
    let (keys, rx) = unbounded_channel();
    let (approvals, approval_rx) = unbounded_channel();
    let (reply, answer) = oneshot::channel();
    approvals.send(Approval { request: "run_bash rm x".into(), reply }).unwrap();
    keys.send(Event::Key(press(KeyCode::Char('o')))).unwrap();
    let driver = actix_web::rt::spawn(async move {
        assert!(answer.await.unwrap());
        keys.send(Event::Key(press(KeyCode::Esc))).unwrap();
    });
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    event_loop(&mut terminal, rx, approval_rx, agent, config).await.unwrap();
    driver.await.unwrap();
}
