//! Boucle d'agent du mode texte : le modèle répond, ses balises d'outils sont exécutées
//! (après les garde-fous) et les résultats lui sont renvoyés, jusqu'à une réponse sans balise.

use rig_agent::Agent;
use rig_agent::completion::Message;

use super::guard::{self, Denial, Guard};
use crate::agent_actor::AgentError;
use crate::tools::{Workspace, text};

/// Boucle bornée à `max_turns` appels au modèle. Comme rig, elle laisse `history`
/// inchangé en cas d'erreur.
pub async fn chat(
    agent: &Agent,
    guard: &Guard,
    ws: &Workspace,
    max_turns: usize,
    prompt: String,
    history: &mut Vec<Message>,
) -> Result<String, AgentError> {
    let mut work = history.clone();
    let mut recent = Vec::new();
    let mut message = prompt;
    for _ in 0..max_turns {
        let reply = agent.chat(message, &mut work).await?.output();
        let calls = text::parse(&reply);
        if calls.is_empty() {
            *history = work;
            return Ok(reply);
        }
        let mut results = String::new();
        for (name, args) in calls {
            let raw = args.to_string();
            let stuck = guard::record(&mut recent, name, &raw);
            let output = match guard.authorize(stuck, name, &raw).await {
                Ok(()) => text::run(ws, name, args).await,
                Err(Denial::Refused(reason)) => reason,
                Err(Denial::Stop(reason)) => return Err(AgentError::Stopped(reason)),
            };
            results.push_str(&format!("<result tool=\"{name}\">\n{output}\n</result>\n"));
        }
        message = results;
    }
    Err(AgentError::MaxTurns(max_turns))
}
