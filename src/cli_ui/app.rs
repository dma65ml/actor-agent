//! État de l'interface : historique, saisie, défilement et envoi des messages à l'agent.

use actix::Addr;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent_actor::{AgentActor, AgentError, Ask, GetToolMode, Reset};
use crate::config::AppConfig;
use crate::harness::guard::Approval;
use crate::harness::probe::ToolMode;

use super::history::History;
use super::input::Input;

/// Auteur d'une entrée de l'historique.
pub(super) enum Role {
    User,
    Agent,
    Error,
    System,
}

/// Retour asynchrone de l'agent vers l'interface.
pub(super) enum Reply {
    /// Réponse (ou erreur) à un message envoyé.
    Answer(Result<String, AgentError>),
    /// Mode d'appel d'outils retenu.
    Mode(ToolMode),
}

/// État de l'interface.
pub(super) struct App {
    pub(super) agent: Addr<AgentActor>,
    /// Configuration à appliquer au prochain `/clear` (modifiée par `/model`, `/preamble`).
    pub(super) config: AppConfig,
    pub(super) replies: UnboundedSender<Reply>,
    pub(super) history: Vec<(Role, String)>,
    pub(super) input: Input,
    pub(super) busy: bool,
    /// Décalage du défilement compté depuis le bas (0 = dernières lignes visibles).
    pub(super) scroll_up: usize,
    /// Messages déjà envoyés, de cette séance et des précédentes, rappelables avec ↑ / ↓.
    pub(super) sent: History,
    /// Appel d'outil en attente d'accord : `o` autorise, toute autre touche refuse.
    pub(super) pending: Option<Approval>,
}

/// Lignes défilées par cran de molette.
const WHEEL_STEP: usize = 3;

impl App {
    /// Crée une interface vide, prête à saisir, avec l'historique des séances précédentes.
    pub(super) fn new(
        agent: Addr<AgentActor>,
        config: AppConfig,
        replies: UnboundedSender<Reply>,
    ) -> Self {
        Self {
            sent: History::load(&config.history_file),
            agent,
            config,
            replies,
            history: Vec::new(),
            input: Input::default(),
            busy: false,
            scroll_up: 0,
            pending: None,
        }
    }

    /// Traite une touche ; renvoie `false` pour quitter (une demande en attente est alors refusée).
    /// Entrée envoie ; Alt+Entrée, Maj+Entrée ou Ctrl+J insèrent un saut de ligne.
    /// ↑ / ↓ changent de ligne, puis rappellent l'historique depuis la première / dernière ligne.
    pub(super) fn on_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Char('c') if ctrl => return false,
            _ if self.pending.is_some() => self.answer(matches!(key.code, KeyCode::Char('o' | 'O'))),
            KeyCode::Enter if key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => self.input.insert("\n"),
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Enter => self.submit(),
            KeyCode::Char(c) if !ctrl => self.input.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up if !self.input.up() => {
                if let Some(entry) = self.sent.previous() {
                    self.input.set(entry);
                }
            }
            KeyCode::Down if !self.input.down() => {
                if let Some(entry) = self.sent.next() {
                    self.input.set(entry);
                }
            }
            KeyCode::PageUp => self.scroll_up += 10,
            KeyCode::PageDown => self.scroll_up = self.scroll_up.saturating_sub(10),
            _ => {}
        }
        true
    }

    /// Collage (bracketed paste) : le texte est inséré tel quel, sauts de ligne compris, sans être envoyé.
    pub(super) fn on_paste(&mut self, text: &str) {
        if self.pending.is_none() {
            self.input.insert(text);
        }
    }

    /// Affiche une demande d'autorisation et la garde jusqu'à la réponse de l'utilisateur.
    pub(super) fn on_approval(&mut self, approval: Approval) {
        self.history.push((Role::System, format!("Autorisation demandée : {}", approval.request)));
        self.scroll_up = 0;
        self.pending = Some(approval);
    }

    /// Transmet la réponse à la demande en attente et la note dans l'historique.
    fn answer(&mut self, allowed: bool) {
        let Some(approval) = self.pending.take() else { return };
        let verdict = if allowed { "Autorisé" } else { "Refusé" };
        self.history.push((Role::System, format!("{verdict} : {}", approval.request)));
        // L'agent a pu abandonner entre-temps (délai dépassé) : rien à faire.
        let _ = approval.reply.send(allowed);
    }

    /// Molette : fait défiler l'historique ; les autres actions de la souris sont ignorées.
    pub(super) fn on_mouse(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollUp => self.scroll_up += WHEEL_STEP,
            MouseEventKind::ScrollDown => self.scroll_up = self.scroll_up.saturating_sub(WHEEL_STEP),
            _ => {}
        }
    }

    /// Envoie la saisie à l'agent en tâche de fond, ou exécute une commande :
    /// `/clear`, `/copy`, `/model <nom>`, `/preamble <texte>`.
    pub(super) fn submit(&mut self) {
        if self.input.text().trim().is_empty() || self.busy {
            return;
        }
        // Seules les lignes vides et les espaces de fin sont retirés : l'indentation d'un code collé est gardée.
        let content = self.input.take().trim_end().trim_start_matches(['\n', '\r']).to_owned();
        self.scroll_up = 0;
        self.sent.push(&content);
        if content == "/copy" {
            let note = self.copy_last_reply();
            self.history.push((Role::System, note));
            return;
        }
        if content == "/clear" {
            self.agent.do_send(Reset(self.config.clone()));
            self.history.clear();
            self.show_tool_mode();
            return;
        }
        let field = match content.split_once(' ') {
            Some(("/model", v)) => Some((&mut self.config.model, v)),
            Some(("/preamble", v)) => Some((&mut self.config.preamble, v)),
            _ => None,
        };
        if let Some((field, value)) = field {
            *field = value.trim().into();
            let note = format!("{content} appliqué au prochain /clear.");
            self.history.push((Role::System, note));
            return;
        }
        self.history.push((Role::User, content.clone()));
        self.busy = true;
        let (agent, replies) = (self.agent.clone(), self.replies.clone());
        actix::spawn(async move {
            // Le récepteur n'existe plus si l'utilisateur a quitté : rien à faire.
            let reply = agent.send(Ask(content)).await.map_err(Into::into).and_then(|r| r);
            let _ = replies.send(Reply::Answer(reply));
        });
    }

    /// Copie la dernière réponse de l'agent dans le presse-papiers système ; renvoie le compte rendu.
    fn copy_last_reply(&self) -> String {
        let Some((_, reply)) = self.history.iter().rev().find(|(role, _)| matches!(role, Role::Agent)) else {
            return "Aucune réponse à copier.".into();
        };
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(reply.as_str())) {
            Ok(()) => "Dernière réponse copiée dans le presse-papiers.".into(),
            Err(e) => format!("Copie impossible : {e}"),
        }
    }

    /// Demande à l'agent son mode d'appel d'outils (servi après la détection) pour l'afficher.
    pub(super) fn show_tool_mode(&self) {
        let (agent, replies) = (self.agent.clone(), self.replies.clone());
        actix::spawn(async move {
            if let Ok(mode) = agent.send(GetToolMode).await {
                let _ = replies.send(Reply::Mode(mode));
            }
        });
    }

    /// Ajoute la réponse (ou l'erreur) de l'agent, ou le mode d'outils, à l'historique.
    pub(super) fn on_reply(&mut self, reply: Reply) {
        self.scroll_up = 0;
        let entry = match reply {
            Reply::Mode(mode) => (Role::System, format!("Outils : {mode}")),
            Reply::Answer(answer) => {
                self.busy = false;
                match answer {
                    Ok(text) => (Role::Agent, text),
                    Err(e) => (Role::Error, e.to_string()),
                }
            }
        };
        self.history.push(entry);
    }
}
