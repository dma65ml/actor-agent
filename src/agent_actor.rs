//! Acteur actix encapsulant un agent rig connecté au serveur Ollama local.
//! L'acteur conserve l'historique de conversation dans son état (multi-tours).

use actix::prelude::*;
use rig_agent::completion::{Message, PromptError};
use rig_agent::{Agent, AgentBuilder};
use rig_core::providers::ollama::OllamaConfig;

use crate::config::AppConfig;

/// Message envoyé à l'acteur : une question de l'utilisateur.
#[derive(Message)]
#[rtype(result = "Result<String, Box<PromptError>>")]
pub struct Ask(pub String);

/// Acteur détenant l'agent rig et l'historique de la conversation.
pub struct AgentActor {
    agent: Agent,
    history: Vec<Message>,
}

impl AgentActor {
    /// Construit l'agent à partir du serveur, du modèle et du préambule de `cfg`.
    pub fn new(cfg: &AppConfig) -> Self {
        let ollama = OllamaConfig::new().with_base_url(&cfg.ollama_url).client();
        let agent = AgentBuilder::new(ollama.completion(&cfg.model))
            .preamble(&cfg.preamble)
            .build();
        Self {
            agent,
            history: Vec::new(),
        }
    }
}

impl Actor for AgentActor {
    type Context = Context<Self>;
}

impl Handler<Ask> for AgentActor {
    type Result = AtomicResponse<Self, Result<String, Box<PromptError>>>;

    /// Traite une question ; `AtomicResponse` bloque l'acteur jusqu'à la réponse,
    /// ce qui sérialise les tours et garde l'historique cohérent.
    fn handle(&mut self, Ask(prompt): Ask, _ctx: &mut Self::Context) -> Self::Result {
        let agent = self.agent.clone();
        let mut history = std::mem::take(&mut self.history);
        let fut = async move {
            // En cas d'erreur, rig laisse `history` inchangé.
            let res = agent
                .chat(prompt, &mut history)
                .await
                .map(|r| r.output())
                .map_err(Box::new);
            (res, history)
        };
        AtomicResponse::new(Box::pin(fut.into_actor(self).map(
            |(res, history), act, _| {
                act.history = history;
                res
            },
        )))
    }
}
