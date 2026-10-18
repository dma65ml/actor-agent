//! Acteur proxy (routeur/superviseur) : point d'entrée unique des interfaces.
//! Il associe chaque session à son propre `AgentActor`, ce qui isole les historiques.

use std::collections::HashMap;
use std::time::Duration;

use actix::prelude::*;
use rig_agent::completion::PromptError;

use crate::agent_actor::{AgentActor, Ask};
use crate::config::{ConfigActor, GetConfig};

/// Erreurs remontées aux interfaces utilisateur.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    /// L'agent a échoué (ex. serveur Ollama indisponible, modèle absent).
    #[error("échec de l'agent (Ollama est-il démarré ?) : {0}")]
    Agent(#[from] Box<PromptError>),
    /// L'agent n'a pas répondu dans le délai imparti.
    #[error("délai de réponse dépassé")]
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
                    let agent = act.sessions.entry(session_id).or_insert_with(|| AgentActor::new(&cfg).start());
                    Ok((agent.clone(), Duration::from_secs(cfg.answer_timeout_secs)))
                })
                .then(|res, act, _| {
                    async move {
                        let (agent, timeout) = res?;
                        Ok(agent.send(Ask(content)).timeout(timeout).await??)
                    }
                    .into_actor(act)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UpdateConfig;
    use crate::test_support::start_actors;

    /// Raccourci : envoie `content` dans la session `id`.
    async fn say(proxy: &Addr<ProxyAgent>, id: &str, content: &str) -> Result<String, ProxyError> {
        ask(proxy, id.into(), content.into()).await
    }

    /// L'historique s'accumule dans une session (préambule + tours précédents).
    #[actix_web::test]
    async fn session_keeps_history() {
        let (proxy, _) = start_actors();
        assert_eq!(say(&proxy, "a", "un").await.unwrap(), "2 messages : un");
        assert_eq!(say(&proxy, "a", "deux").await.unwrap(), "4 messages : deux");
    }

    /// Deux sessions ont des historiques indépendants.
    #[actix_web::test]
    async fn sessions_are_isolated() {
        let (proxy, _) = start_actors();
        say(&proxy, "a", "un").await.unwrap();
        assert_eq!(say(&proxy, "b", "un").await.unwrap(), "2 messages : un");
    }

    /// `ClearSession` repart d'un historique vide.
    #[actix_web::test]
    async fn clear_session_resets_history() {
        let (proxy, _) = start_actors();
        say(&proxy, "a", "un").await.unwrap();
        proxy.send(ClearSession { session_id: "a".into() }).await.unwrap();
        assert_eq!(say(&proxy, "a", "un").await.unwrap(), "2 messages : un");
    }

    /// Un échec d'Ollama remonte en `Agent` et ne pollue pas l'historique.
    #[actix_web::test]
    async fn agent_failure_is_reported() {
        let (proxy, _) = start_actors();
        assert!(matches!(say(&proxy, "a", "ERREUR").await, Err(ProxyError::Agent(_))));
        assert_eq!(say(&proxy, "a", "un").await.unwrap(), "2 messages : un");
    }

    /// Une réponse trop lente remonte en `Timeout`.
    #[actix_web::test]
    async fn slow_answer_times_out() {
        let (proxy, _) = start_actors();
        assert!(matches!(say(&proxy, "a", "LENT").await, Err(ProxyError::Timeout)));
    }

    /// Un changement de modèle ou de préambule ne s'applique qu'aux sessions créées ensuite.
    #[actix_web::test]
    async fn config_update_applies_to_new_sessions() {
        let (proxy, config) = start_actors();
        say(&proxy, "ancienne", "un").await.unwrap();
        config.send(UpdateConfig::Model("qwen".into())).await.unwrap();
        config.send(UpdateConfig::Preamble("autre".into())).await.unwrap();
        assert_eq!(say(&proxy, "ancienne", "MODELE").await.unwrap(), "llama3.2");
        assert_eq!(say(&proxy, "nouvelle", "MODELE").await.unwrap(), "qwen");
        assert_eq!(say(&proxy, "nouvelle", "PREAMBULE").await.unwrap(), "autre");
    }

    /// Un `ConfigActor` arrêté rend le proxy indisponible (`Mailbox`), sans panique.
    #[actix_web::test]
    async fn stopped_config_actor_gives_mailbox_error() {
        let config = ConfigActor::create(|ctx| {
            ctx.stop();
            ConfigActor::new(crate::config::AppConfig::default())
        });
        let proxy = ProxyAgent::new(config).start();
        assert!(matches!(say(&proxy, "a", "un").await, Err(ProxyError::Mailbox)));
    }
}
