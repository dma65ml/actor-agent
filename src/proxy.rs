//! Acteur proxy (routeur/superviseur) : point d'entrée unique des interfaces.
//! Il associe chaque session à son propre `AgentActor`, ce qui isole les historiques.

use std::collections::HashMap;
use std::time::Duration;

use actix::prelude::*;
use rig_agent::completion::PromptError;

use crate::agent_actor::{AgentActor, Ask};

/// Durée maximale accordée à l'agent pour répondre.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

/// Erreurs remontées aux interfaces utilisateur.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// L'agent a échoué (ex. serveur Ollama indisponible, modèle absent).
    #[error("échec de l'agent (Ollama est-il démarré ?) : {0}")]
    Agent(#[from] Box<PromptError>),
    /// L'agent n'a pas répondu dans le délai imparti.
    #[error("délai de réponse dépassé ({}s)", ANSWER_TIMEOUT.as_secs())]
    Timeout,
    /// L'acteur agent s'est arrêté avant de répondre.
    #[error("acteur agent indisponible")]
    Mailbox,
}

impl From<MailboxError> for ProxyError {
    /// Distingue le dépassement de délai d'une boîte aux lettres fermée.
    fn from(e: MailboxError) -> Self {
        match e {
            MailboxError::Timeout => Self::Timeout,
            MailboxError::Closed => Self::Mailbox,
        }
    }
}

/// Envoie un prompt dans la session donnée (créée si inexistante).
#[derive(Message)]
#[rtype(result = "Result<String, ProxyError>")]
pub struct SendMessage {
    pub session_id: String,
    pub content: String,
}

/// Raccourci des interfaces : envoie un prompt au proxy et aplatit les erreurs.
pub async fn ask(proxy: &Addr<ProxyAgent>, session_id: String, content: String) -> Result<String, ProxyError> {
    proxy.send(SendMessage { session_id, content }).await?
}

/// Oublie une session et son historique.
#[derive(Message)]
#[rtype(result = "()")]
pub struct ClearSession {
    pub session_id: String,
}

/// Routeur des sessions vers leurs agents dédiés.
pub struct ProxyAgent {
    model: String,
    preamble: String,
    sessions: HashMap<String, Addr<AgentActor>>,
}

impl ProxyAgent {
    /// Crée un proxy dont les agents utiliseront ce modèle Ollama et ce préambule.
    pub fn new(model: impl Into<String>, preamble: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            preamble: preamble.into(),
            sessions: HashMap::new(),
        }
    }
}

impl Actor for ProxyAgent {
    type Context = Context<Self>;
}

impl Handler<SendMessage> for ProxyAgent {
    type Result = ResponseFuture<Result<String, ProxyError>>;

    /// Route le prompt vers l'agent de la session ; le proxy n'attend pas la réponse,
    /// il reste donc disponible pour les autres sessions pendant la génération.
    fn handle(&mut self, msg: SendMessage, _ctx: &mut Self::Context) -> Self::Result {
        let agent = self
            .sessions
            .entry(msg.session_id)
            .or_insert_with(|| AgentActor::new(&self.model, &self.preamble).start())
            .clone();
        Box::pin(async move { Ok(agent.send(Ask(msg.content)).timeout(ANSWER_TIMEOUT).await??) })
    }
}

impl Handler<ClearSession> for ProxyAgent {
    type Result = ();

    /// Retire la session ; l'agent s'arrête dès que sa dernière adresse est libérée.
    fn handle(&mut self, msg: ClearSession, _ctx: &mut Self::Context) {
        self.sessions.remove(&msg.session_id);
    }
}
