//! État de l'interface : historique, saisie, défilement et envoi des messages à l'agent.

use actix::Addr;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent_actor::{AgentActor, AgentError, Ask, GetToolMode, Reset};
use crate::config::AppConfig;
use crate::harness::guard::Approval;
use crate::harness::probe::ToolMode;

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
    pub(super) input: String,
    pub(super) busy: bool,
    /// Décalage du défilement compté depuis le bas (0 = dernières lignes visibles).
    pub(super) scroll_up: usize,
    /// Messages déjà envoyés, rappelables avec ↑ / ↓.
    pub(super) sent: Vec<String>,
    /// Position dans `sent` pendant le rappel (`None` = saisie libre).
    pub(super) recall: Option<usize>,
    /// Appel d'outil en attente d'accord : `o` autorise, toute autre touche refuse.
    pub(super) pending: Option<Approval>,
}

/// Lignes défilées par cran de molette.
const WHEEL_STEP: usize = 3;

impl App {
    /// Crée une interface vide, prête à saisir.
    pub(super) fn new(
        agent: Addr<AgentActor>,
        config: AppConfig,
        replies: UnboundedSender<Reply>,
    ) -> Self {
        Self {
            agent,
            config,
            replies,
            history: Vec::new(),
            input: String::new(),
            busy: false,
            scroll_up: 0,
            sent: Vec::new(),
            recall: None,
            pending: None,
        }
    }

    /// Traite une touche ; renvoie `false` pour quitter (une demande en attente est alors refusée).
    pub(super) fn on_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            _ if self.pending.is_some() => self.answer(matches!(key.code, KeyCode::Char('o' | 'O'))),
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => _ = self.input.pop(),
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Up => self.recall_previous(),
            KeyCode::Down => self.recall_next(),
            KeyCode::PageUp => self.scroll_up += 10,
            KeyCode::PageDown => self.scroll_up = self.scroll_up.saturating_sub(10),
            _ => {}
        }
        true
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

    /// ↑ : remplace la saisie par le message envoyé précédent (s'arrête au plus ancien).
    fn recall_previous(&mut self) {
        let Some(last) = self.sent.len().checked_sub(1) else { return };
        let i = self.recall.map_or(last, |i| i.saturating_sub(1));
        self.recall = Some(i);
        self.input = self.sent[i].clone();
    }

    /// ↓ : passe au message suivant ; au-delà du plus récent, revient à une saisie vide.
    fn recall_next(&mut self) {
        let Some(i) = self.recall else { return };
        self.recall = (i + 1 < self.sent.len()).then_some(i + 1);
        self.input = self.recall.map(|i| self.sent[i].clone()).unwrap_or_default();
    }

    /// Envoie la saisie à l'agent en tâche de fond, ou exécute une commande :
    /// `/clear`, `/model <nom>`, `/preamble <texte>`.
    pub(super) fn submit(&mut self) {
        let content = self.input.trim().to_owned();
        if content.is_empty() || self.busy {
            return;
        }
        self.input.clear();
        self.scroll_up = 0;
        self.recall = None;
        self.sent.push(content.clone());
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
