//! Acteur actix encapsulant le harness d'agent : agent rig outillé, garde-fous,
//! journal de session et compaction du contexte. L'historique vit dans l'état de l'acteur.

use std::time::Duration;

use actix::prelude::*;
use rig_agent::completion::{Message, PromptError};
use rig_agent::{Agent, AgentBuilder};
use rig_core::providers::ollama::OllamaConfig;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::AppConfig;
use crate::harness::guard::{Approval, Guard};
use crate::harness::session::{Event, SessionStore};
use crate::harness::tools::{ReadFile, RunBash, WriteFile};
use crate::harness::{compact, prompt};

/// Consigne de l'agent chargé des résumés de compaction.
const SUMMARIZER_PREAMBLE: &str = "Tu résumes fidèlement des conversations d'agent, sans rien inventer.";

/// Erreurs remontées à l'interface utilisateur.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// L'agent a échoué (ex. serveur Ollama indisponible, modèle absent).
    #[error("échec de l'agent (Ollama est-il démarré ?) : {0}")]
    Agent(Box<PromptError>),
    /// La boucle d'agent a atteint son plafond d'appels au modèle.
    #[error("limite de {0} appels au modèle atteinte ; reformulez ou découpez la tâche")]
    MaxTurns(usize),
    /// Un garde-fou a arrêté l'exécution (ex. boucle détectée).
    #[error("exécution arrêtée : {0}")]
    Stopped(String),
    /// L'agent n'a pas répondu dans le délai imparti.
    #[error("délai de réponse dépassé")]
    Timeout,
    /// Le journal de session n'a pas pu être écrit.
    #[error("journal de session : {0}")]
    Session(#[from] std::io::Error),
    /// L'acteur s'est arrêté avant de répondre.
    #[error("acteur agent indisponible")]
    Mailbox(#[from] MailboxError),
}

impl From<PromptError> for AgentError {
    /// Distingue les arrêts de la boucle (plafond, garde-fou) des vraies pannes.
    fn from(e: PromptError) -> Self {
        match e {
            PromptError::MaxTurns { max_turns, .. } => Self::MaxTurns(max_turns),
            PromptError::Cancelled { reason, .. } => Self::Stopped(reason),
            e => Self::Agent(Box::new(e)),
        }
    }
}

/// Message envoyé à l'acteur : une demande de l'utilisateur.
#[derive(Message)]
#[rtype(result = "Result<String, AgentError>")]
pub struct Ask(pub String);

/// Efface l'historique (journalisé) et reconstruit l'agent avec la configuration fournie.
#[derive(Message)]
#[rtype(result = "()")]
pub struct Reset(pub AppConfig);

/// Acteur détenant l'agent, son journal de session et l'historique de la conversation.
pub struct AgentActor {
    agent: Agent,
    summarizer: Agent,
    timeout: Duration,
    context_tokens: usize,
    session: SessionStore,
    approvals: UnboundedSender<Approval>,
    history: Vec<Message>,
}

impl AgentActor {
    /// Construit l'agent et rejoue le journal de session pour reprendre la conversation.
    /// Les demandes d'autorisation des garde-fous partent sur `approvals`.
    pub fn new(cfg: &AppConfig, approvals: UnboundedSender<Approval>) -> std::io::Result<Self> {
        let mut actor = Self::build(cfg, approvals);
        actor.history = actor.session.replay()?;
        Ok(actor)
    }

    /// Construit un agent neuf, historique vide : prompt assemblé, outils, garde-fous, plafond de boucle.
    fn build(cfg: &AppConfig, approvals: UnboundedSender<Approval>) -> Self {
        let ollama = OllamaConfig::new().with_base_url(&cfg.ollama_url).client();
        let cwd = std::env::current_dir().unwrap_or_default();
        let agent = AgentBuilder::new(ollama.completion(&cfg.model))
            .preamble(prompt::assemble(&cfg.preamble, &cwd))
            .default_max_turns(cfg.max_turns)
            .add_hook(Guard::new(approvals.clone(), cfg.auto_approve))
            .tool(ReadFile)
            .tool(WriteFile)
            .tool(RunBash)
            .build();
        let summarizer = AgentBuilder::new(ollama.completion(&cfg.model)).preamble(SUMMARIZER_PREAMBLE).build();
        Self {
            agent,
            summarizer,
            timeout: Duration::from_secs(cfg.answer_timeout_secs),
            context_tokens: cfg.context_tokens,
            session: SessionStore::new(&cfg.session_file),
            approvals,
            history: Vec::new(),
        }
    }
}

/// Compacte `history` si le contexte approche sa limite ; renvoie le nouvel historique à journaliser.
/// Un résumé en échec laisse l'historique intact : on retentera au tour suivant.
async fn compact_if_needed(summarizer: &Agent, history: &mut Vec<Message>, context_tokens: usize) -> Option<Vec<Message>> {
    let split = compact::split_point(history, context_tokens)?;
    let request = compact::summary_request(&history[..split]);
    let summary = summarizer.chat(request, &mut Vec::new()).await.ok()?.output();
    *history = compact::compacted(&summary, &history[split..]);
    Some(history.clone())
}

impl Actor for AgentActor {
    type Context = Context<Self>;
}

impl Handler<Ask> for AgentActor {
    type Result = AtomicResponse<Self, Result<String, AgentError>>;

    /// Compacte le contexte si besoin, lance la boucle d'agent (outils compris) puis journalise
    /// les nouveaux messages. `AtomicResponse` sérialise les tours et garde l'historique cohérent.
    fn handle(&mut self, Ask(prompt): Ask, _ctx: &mut Self::Context) -> Self::Result {
        let (agent, summarizer) = (self.agent.clone(), self.summarizer.clone());
        let (timeout, context_tokens) = (self.timeout, self.context_tokens);
        let mut history = std::mem::take(&mut self.history);
        let fut = async move {
            let compaction = compact_if_needed(&summarizer, &mut history, context_tokens).await;
            let before = history.len();
            // En cas d'erreur, rig laisse `history` inchangé.
            let res = match tokio::time::timeout(timeout, agent.chat(prompt, &mut history)).await {
                Ok(r) => r.map(|r| r.output()).map_err(AgentError::from),
                Err(_) => Err(AgentError::Timeout),
            };
            (compaction, before, res, history)
        };
        AtomicResponse::new(Box::pin(fut.into_actor(self).map(
            |(compaction, before, res, history), act, _| {
                let events = compaction.map(|messages| Event::Compaction { messages }).into_iter();
                let mut events = events.chain(history[before..].iter().map(|m| Event::Message { message: m.clone() }));
                let logged = events.try_for_each(|e| act.session.append(e));
                act.history = history;
                let reply = res?;
                logged?;
                Ok(reply)
            },
        )))
    }
}

impl Handler<Reset> for AgentActor {
    type Result = ();

    /// Repart d'un agent neuf : nouvelle configuration, historique vide, remise à zéro journalisée.
    fn handle(&mut self, Reset(cfg): Reset, _ctx: &mut Self::Context) {
        *self = Self::build(&cfg, self.approvals.clone());
        // Échec d'écriture non bloquant : au pire, l'ancienne conversation sera rejouée au redémarrage.
        let _ = self.session.append(Event::Reset);
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::harness::tools::Permission;
    use crate::test_support::{start_agent, test_config};

    /// Raccourci : envoie `content` à l'agent et aplatit les erreurs.
    async fn say(agent: &Addr<AgentActor>, content: &str) -> Result<String, AgentError> {
        agent.send(Ask(content.into())).await?
    }

    /// Démarre un agent sur `cfg` et répond `answer` à toutes ses demandes d'autorisation.
    fn agent_answering(cfg: &AppConfig, answer: bool) -> Addr<AgentActor> {
        let (tx, mut rx) = unbounded_channel::<Approval>();
        actix::spawn(async move {
            while let Some(approval) = rx.recv().await {
                let _ = approval.reply.send(answer);
            }
        });
        AgentActor::new(cfg, tx).unwrap().start()
    }

    /// L'historique s'accumule (préambule + tours précédents).
    #[actix_web::test]
    async fn keeps_history() {
        let (agent, _) = start_agent();
        assert_eq!(say(&agent, "un").await.unwrap(), "2 messages : un");
        assert_eq!(say(&agent, "deux").await.unwrap(), "4 messages : deux");
    }

    /// Un échec d'Ollama remonte en `Agent` et ne pollue pas l'historique.
    #[actix_web::test]
    async fn agent_failure_is_reported() {
        let (agent, _) = start_agent();
        assert!(matches!(say(&agent, "ERREUR").await, Err(AgentError::Agent(_))));
        assert_eq!(say(&agent, "un").await.unwrap(), "2 messages : un");
    }

    /// Une réponse trop lente remonte en `Timeout`.
    #[actix_web::test]
    async fn slow_answer_times_out() {
        let (agent, _) = start_agent();
        assert!(matches!(say(&agent, "LENT").await, Err(AgentError::Timeout)));
    }

    /// `Reset` vide l'historique et applique le nouveau modèle et préambule.
    #[actix_web::test]
    async fn reset_clears_history_and_applies_config() {
        let (agent, cfg) = start_agent();
        say(&agent, "un").await.unwrap();
        let new = AppConfig { model: "qwen".into(), preamble: "autre".into(), ..cfg };
        agent.send(Reset(new)).await.unwrap();
        assert_eq!(say(&agent, "un").await.unwrap(), "2 messages : un");
        assert_eq!(say(&agent, "MODELE").await.unwrap(), "qwen");
        assert!(say(&agent, "PREAMBULE").await.unwrap().starts_with("autre"));
    }

    /// Un acteur arrêté remonte `Mailbox`, sans panique.
    #[actix_web::test]
    async fn stopped_actor_gives_mailbox_error() {
        let cfg = test_config();
        let agent = AgentActor::create(|ctx| {
            ctx.stop();
            AgentActor::build(&cfg, unbounded_channel().0)
        });
        assert!(matches!(say(&agent, "un").await, Err(AgentError::Mailbox(_))));
    }

    /// La boucle exécute l'outil demandé par le modèle puis renvoie sa réponse finale.
    #[actix_web::test]
    async fn runs_tool_in_loop() {
        let (agent, _) = start_agent();
        let reply = say(&agent, "OUTIL echo bonjour").await.unwrap();
        assert_eq!(reply, "outil : code de sortie : 0\nbonjour\n");
    }

    /// Au-delà de `auto_approve`, l'appel attend l'accord : accepté il s'exécute, refusé le modèle en est informé.
    #[actix_web::test]
    async fn permission_gate_asks_user() {
        let cfg = test_config();
        let marker = format!("{}.marque", cfg.session_file);
        let allowed = agent_answering(&cfg, true);
        say(&allowed, &format!("OUTIL touch {marker}")).await.unwrap();
        assert!(std::path::Path::new(&marker).exists());

        let denied = agent_answering(&test_config(), false);
        let reply = say(&denied, "OUTIL touch fichier-interdit").await.unwrap();
        assert!(reply.contains("refusé par l'utilisateur"), "{reply}");
        assert!(!std::path::Path::new("fichier-interdit").exists());

        let workspace = AppConfig { auto_approve: Permission::Workspace, ..test_config() };
        let agent = AgentActor::new(&workspace, unbounded_channel().0).unwrap().start();
        assert!(say(&agent, "OUTIL true").await.unwrap().contains("code de sortie : 0"));
    }

    /// Le même appel répété arrête l'exécution ; le plafond d'itérations aussi.
    #[actix_web::test]
    async fn stops_stuck_loop_and_max_turns() {
        let (agent, cfg) = start_agent();
        let err = say(&agent, "BOUCLE").await.unwrap_err();
        assert!(matches!(&err, AgentError::Stopped(r) if r.contains("boucle détectée")), "{err}");

        let capped = AgentActor::new(&AppConfig { max_turns: 2, ..cfg }, unbounded_channel().0).unwrap().start();
        assert!(matches!(say(&capped, "BOUCLE").await, Err(AgentError::MaxTurns(2))));
    }

    /// Le journal permet de reprendre la conversation après redémarrage ; `Reset` y est journalisé.
    #[actix_web::test]
    async fn session_is_replayed_on_restart() {
        let (agent, cfg) = start_agent();
        say(&agent, "un").await.unwrap();
        let restarted = AgentActor::new(&cfg, unbounded_channel().0).unwrap().start();
        assert_eq!(say(&restarted, "deux").await.unwrap(), "4 messages : deux");
        restarted.send(Reset(cfg.clone())).await.unwrap();
        assert!(SessionStore::new(&cfg.session_file).replay().unwrap().is_empty());
    }

    /// Fenêtre de contexte saturée : les anciens messages sont résumés et la compaction journalisée.
    #[actix_web::test]
    async fn compacts_context_when_full() {
        let cfg = AppConfig { context_tokens: 1, ..test_config() };
        let agent = AgentActor::new(&cfg, unbounded_channel().0).unwrap().start();
        for msg in ["un", "deux", "trois", "quatre"] {
            say(&agent, msg).await.unwrap();
        }
        // Sans compaction, le modèle recevrait préambule + 8 messages + 1 = 10 messages.
        assert_ne!(say(&agent, "cinq").await.unwrap(), "10 messages : cinq");
        // Historique rejoué : résumé + 6 messages récents + le dernier tour (2 messages).
        let history = SessionStore::new(&cfg.session_file).replay().unwrap();
        assert_eq!(history.len(), 9);
        assert!(serde_json::to_string(&history[0]).unwrap().contains("Résumé de la conversation précédente"));
        assert!(std::fs::read_to_string(&cfg.session_file).unwrap().contains("\"type\":\"compaction\""));
    }
}
