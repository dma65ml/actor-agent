//! Acteur proxy (routeur/superviseur) : point d'entrée unique des interfaces.
//! Il associe chaque session à son propre `AgentActor`, ce qui isole les historiques.

use std::collections::HashMap;
use std::time::Duration;

use actix::prelude::*;
use rig_agent::completion::PromptError;

use crate::agent_actor::{AgentActor, Ask};
use crate::config::{ConfigActor, GetConfig};

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
    config: Addr<ConfigActor>,
    sessions: HashMap<String, Addr<AgentActor>>,
}

impl ProxyAgent {
    /// Crée un proxy qui lira modèle et préambule auprès de `config` à chaque nouvelle session.
    pub fn new(config: Addr<ConfigActor>) -> Self {
        Self { config, sessions: HashMap::new() }
    }
}

impl Actor for ProxyAgent {
    type Context = Context<Self>;
}

impl Handler<SendMessage> for ProxyAgent {
    type Result = ResponseActFuture<Self, Result<String, ProxyError>>;

    /// Récupère la configuration à jour, crée l'agent si la session est nouvelle,
    /// puis attend sa réponse sans bloquer le proxy pour les autres sessions.
    /// Un changement de modèle ne s'applique donc qu'aux sessions créées ensuite.
    fn handle(&mut self, msg: SendMessage, _ctx: &mut Self::Context) -> Self::Result {
        let SendMessage { session_id, content } = msg;
        let config = self.config.clone();
        // ponytail: config relue à chaque message (aller-retour local négligeable face au LLM) ;
        // l'insertion se fait dans le contexte de l'acteur, donc sans course entre messages.
        Box::pin(
            async move { config.send(GetConfig).await }
                .into_actor(self)
                .map(|cfg, act, _| -> Result<_, ProxyError> {
                    let cfg = cfg?;
                    Ok(act
                        .sessions
                        .entry(session_id)
                        .or_insert_with(|| AgentActor::new(&cfg.model, &cfg.preamble).start())
                        .clone())
                })
                .then(|agent, act, _| {
                    async move { Ok(agent?.send(Ask(content)).timeout(ANSWER_TIMEOUT).await??) }.into_actor(act)
                }),
        )
    }
}

impl Handler<ClearSession> for ProxyAgent {
    type Result = ();

    /// Retire la session ; l'agent s'arrête dès que sa dernière adresse est libérée.
    fn handle(&mut self, msg: ClearSession, _ctx: &mut Self::Context) {
        self.sessions.remove(&msg.session_id);
    }
}
