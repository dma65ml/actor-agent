//! Interface terminal (ratatui) : historique défilant, zone de saisie et barre de statut.
//! Les touches arrivent d'un thread dédié et les réponses de tâches actix : la boucle de
//! rendu ne bloque jamais pendant la génération.

use actix::Addr;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
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
    let res = event_loop(&mut terminal, proxy, config).await;
    ratatui::restore();
    res
}

/// Boucle principale : dessine, puis attend une touche ou une réponse de l'agent.
async fn event_loop(
    terminal: &mut DefaultTerminal,
    proxy: Addr<ProxyAgent>,
    config: Addr<ConfigActor>,
) -> std::io::Result<()> {
    let mut keys = spawn_input_thread();
    let (replies, mut reply_rx) = unbounded_channel();
    let mut app = App { proxy, config, replies, history: Vec::new(), input: String::new(), busy: false, scroll_up: 0 };
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
