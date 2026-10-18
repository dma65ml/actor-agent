//! Interface terminal (ratatui) : historique défilant, zone de saisie et barre de statut.
//! Les touches arrivent d'un thread dédié et les réponses de tâches actix : la boucle de
//! rendu ne bloque jamais pendant la génération.

use actix::Addr;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::backend::Backend;
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::config::{ConfigActor, UpdateConfig};
use crate::proxy::{ClearSession, ProxyAgent, ProxyError, ask};

/// Session unique utilisée par le terminal.
const SESSION_ID: &str = "cli";

/// Auteur d'une entrée de l'historique.
enum Role {
    User,
    Agent,
    Error,
    System,
}

/// État de l'interface.
struct App {
    proxy: Addr<ProxyAgent>,
    config: Addr<ConfigActor>,
    replies: UnboundedSender<Result<String, ProxyError>>,
    history: Vec<(Role, String)>,
    input: String,
    busy: bool,
    /// Décalage du défilement compté depuis le bas (0 = dernières lignes visibles).
    scroll_up: usize,
}

/// Lance le TUI jusqu'à Échap / Ctrl+C ; le terminal est restauré même en cas d'erreur.
pub async fn run(proxy: Addr<ProxyAgent>, config: Addr<ConfigActor>) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let res = event_loop(&mut terminal, spawn_input_thread(), proxy, config).await;
    ratatui::restore();
    res
}

/// Boucle principale : dessine, puis attend une touche ou une réponse de l'agent.
/// Le terminal et la source d'événements sont injectés pour pouvoir la tester.
async fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut keys: UnboundedReceiver<Event>,
    proxy: Addr<ProxyAgent>,
    config: Addr<ConfigActor>,
) -> Result<(), B::Error> {
    let (replies, mut reply_rx) = unbounded_channel();
    let mut app = App::new(proxy, config, replies);
    loop {
        terminal.draw(|f| app.draw(f))?;
        tokio::select! {
            ev = keys.recv() => match ev {
                Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if !app.on_key(key) { return Ok(()) }
                }
                Some(_) => {}
                None => return Ok(()),
            },
            Some(reply) = reply_rx.recv() => app.on_reply(reply),
        }
    }
}

/// Lit les événements crossterm (bloquants) dans un thread et les transmet à la boucle async.
fn spawn_input_thread() -> UnboundedReceiver<Event> {
    let (tx, rx) = unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    rx
}

impl App {
    /// Crée une interface vide, prête à saisir.
    fn new(
        proxy: Addr<ProxyAgent>,
        config: Addr<ConfigActor>,
        replies: UnboundedSender<Result<String, ProxyError>>,
    ) -> Self {
        Self { proxy, config, replies, history: Vec::new(), input: String::new(), busy: false, scroll_up: 0 }
    }

    /// Traite une touche ; renvoie `false` pour quitter.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => _ = self.input.pop(),
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Up => self.scroll_up += 1,
            KeyCode::Down => self.scroll_up = self.scroll_up.saturating_sub(1),
            KeyCode::PageUp => self.scroll_up += 10,
            KeyCode::PageDown => self.scroll_up = self.scroll_up.saturating_sub(10),
            _ => {}
        }
        true
    }

    /// Envoie la saisie à l'agent en tâche de fond, ou exécute une commande :
    /// `/clear`, `/model <nom>`, `/preamble <texte>`.
    fn submit(&mut self) {
        let content = self.input.trim().to_owned();
        if content.is_empty() || self.busy {
            return;
        }
        self.input.clear();
        self.scroll_up = 0;
        if content == "/clear" {
            self.proxy.do_send(ClearSession { session_id: SESSION_ID.into() });
            self.history.clear();
            return;
        }
        let change = match content.split_once(' ') {
            Some(("/model", v)) => Some(UpdateConfig::Model(v.trim().into())),
            Some(("/preamble", v)) => Some(UpdateConfig::Preamble(v.trim().into())),
            _ => None,
        };
        if let Some(change) = change {
            self.config.do_send(change);
            let note = format!("{content} appliqué aux nouvelles sessions (/clear pour l'utiliser ici).");
            self.history.push((Role::System, note));
            return;
        }
        self.history.push((Role::User, content.clone()));
        self.busy = true;
        let (proxy, replies) = (self.proxy.clone(), self.replies.clone());
        actix::spawn(async move {
            // Le récepteur n'existe plus si l'utilisateur a quitté : rien à faire.
            let _ = replies.send(ask(&proxy, SESSION_ID.into(), content).await);
        });
    }

    /// Ajoute la réponse (ou l'erreur) de l'agent à l'historique.
    fn on_reply(&mut self, reply: Result<String, ProxyError>) {
        self.busy = false;
        self.scroll_up = 0;
        self.history.push(match reply {
            Ok(text) => (Role::Agent, text),
            Err(e) => (Role::Error, e.to_string()),
        });
    }

    /// Dessine l'historique, la saisie et le statut.
    fn draw(&mut self, f: &mut Frame) {
        let [hist_area, input_area, status_area] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)]).areas(f.area());

        let mut lines = Vec::new();
        for (role, text) in &self.history {
            let (label, color) = match role {
                Role::User => ("Vous", Color::Cyan),
                Role::Agent => ("Agent", Color::Green),
                Role::Error => ("Erreur", Color::Red),
                Role::System => ("Système", Color::DarkGray),
            };
            lines.push(Line::styled(label, Style::new().fg(color).bold()));
            lines.extend(text.lines().map(Line::raw));
            lines.push(Line::default());
        }
        let history = Paragraph::new(lines)
            .block(Block::bordered().title(" Historique "))
            .wrap(Wrap { trim: false });
        // Défilement ancré en bas : on borne le décalage puis on le convertit depuis le haut.
        let max_top = history.line_count(hist_area.width).saturating_sub(hist_area.height as usize);
        self.scroll_up = self.scroll_up.min(max_top);
        let top = u16::try_from(max_top - self.scroll_up).unwrap_or(u16::MAX);
        f.render_widget(history.scroll((top, 0)), hist_area);

        let input = Paragraph::new(self.input.as_str())
            .block(Block::bordered().title(" Message — Entrée : envoyer · /clear · /model · /preamble · Échap : quitter "));
        f.render_widget(input, input_area);
        // ponytail: curseur placé au nombre de caractères, faux pour les glyphes larges (CJK, emoji).
        let col = u16::try_from(self.input.chars().count()).unwrap_or(u16::MAX);
        f.set_cursor_position((input_area.x.saturating_add(1).saturating_add(col), input_area.y + 1));

        let status = if self.busy {
            Line::styled(" ⏳ Génération en cours...", Style::new().fg(Color::Yellow))
        } else {
            Line::styled(" ✔ En attente...", Style::new().fg(Color::Green))
        };
        f.render_widget(status, status_area);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyEventState, KeyModifiers};

    use super::*;
    use crate::config::GetConfig;
    use crate::test_support::start_actors;

    /// Événement « touche pressée ».
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Interface de test branchée sur le faux Ollama, avec le récepteur des réponses.
    fn app() -> (App, UnboundedReceiver<Result<String, ProxyError>>) {
        let (proxy, config) = start_actors();
        let (tx, rx) = unbounded_channel();
        (App::new(proxy, config, tx), rx)
    }

    /// Saisit `text` caractère par caractère.
    fn type_text(app: &mut App, text: &str) {
        text.chars().for_each(|c| _ = app.on_key(press(KeyCode::Char(c))));
    }

    /// Dessine `app` sur un terminal virtuel et renvoie l'écran sous forme de texte.
    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect()
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
        app.on_key(press(KeyCode::Up));
        assert_eq!(app.scroll_up, 11);
        app.on_key(press(KeyCode::Down));
        assert_eq!(app.scroll_up, 10);
        app.on_key(press(KeyCode::PageDown));
        app.on_key(press(KeyCode::PageDown));
        assert_eq!(app.scroll_up, 0);
        app.on_key(press(KeyCode::Down));
        assert_eq!(app.scroll_up, 0);
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
        assert!(matches!(app.history.as_slice(), [(Role::User, u), (Role::Agent, a)] if u == "salut" && a == "2 messages : salut"));
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
        app.on_reply(Err(ProxyError::Timeout));
        assert!(matches!(app.history.as_slice(), [(Role::Error, e)] if e.contains("délai")));
    }

    /// `/clear` vide l'historique local et la session côté proxy.
    #[actix_web::test]
    async fn clear_command_resets_history() {
        let (mut app, mut rx) = app();
        type_text(&mut app, "un");
        app.submit();
        app.on_reply(rx.recv().await.unwrap());
        type_text(&mut app, "/clear");
        app.submit();
        assert!(app.history.is_empty());
        type_text(&mut app, "un");
        app.submit();
        assert_eq!(rx.recv().await.unwrap().unwrap(), "2 messages : un");
    }

    /// `/model` et `/preamble` mettent à jour la configuration et l'annoncent.
    #[actix_web::test]
    async fn config_commands_update_config() {
        let (mut app, _) = app();
        type_text(&mut app, "/model qwen");
        app.submit();
        type_text(&mut app, "/preamble Sois bref.");
        app.submit();
        let cfg = app.config.send(GetConfig).await.unwrap();
        assert_eq!((cfg.model.as_str(), cfg.preamble.as_str()), ("qwen", "Sois bref."));
        assert!(matches!(app.history.as_slice(), [(Role::System, _), (Role::System, _)]));
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
        for text in ["Vous", "Agent", "Erreur", "Système", "réponse", "brouillon", "En attente"] {
            assert!(screen.contains(text), "{text} absent de l'écran");
        }
        app.busy = true;
        assert!(render(&mut app, 60, 20).contains("Génération en cours"));
    }

    /// Le défilement reste ancré en bas et est borné par la taille de l'historique.
    #[actix_web::test]
    async fn draw_scrolls_from_bottom() {
        let (mut app, _) = app();
        app.history = (0..30).map(|i| (Role::Agent, format!("ligne{i:02}"))).collect();
        assert!(render(&mut app, 40, 12).contains("ligne29"));
        app.scroll_up = 1000;
        let screen = render(&mut app, 40, 12);
        assert!(screen.contains("ligne00") && !screen.contains("ligne29"));
        assert!(app.scroll_up < 1000);
    }

    /// Boucle complète : saisie, réponse affichée, événements ignorés, puis Échap.
    #[actix_web::test]
    async fn event_loop_end_to_end() {
        let (proxy, config) = start_actors();
        let (keys, rx) = unbounded_channel();
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        let driver = actix_web::rt::spawn(async move {
            for c in "salut".chars() {
                keys.send(Event::Key(press(KeyCode::Char(c)))).unwrap();
            }
            keys.send(Event::Key(press(KeyCode::Enter))).unwrap();
            keys.send(Event::Resize(60, 20)).unwrap();
            let mut release = press(KeyCode::Char('x'));
            (release.kind, release.state) = (KeyEventKind::Release, KeyEventState::NONE);
            keys.send(Event::Key(release)).unwrap();
            actix_web::rt::time::sleep(Duration::from_millis(500)).await;
            keys.send(Event::Key(press(KeyCode::Esc))).unwrap();
        });
        event_loop(&mut terminal, rx, proxy, config).await.unwrap();
        driver.await.unwrap();
        let screen: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("2 messages : salut"));
        assert!(!screen.contains("salutx"), "la touche relâchée ne doit pas être saisie");
    }

    /// La boucle s'arrête proprement si la source d'événements se ferme.
    #[actix_web::test]
    async fn event_loop_stops_when_input_closes() {
        let (proxy, config) = start_actors();
        let (keys, rx) = unbounded_channel::<Event>();
        drop(keys);
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        assert!(event_loop(&mut terminal, rx, proxy, config).await.is_ok());
    }
}
