//! Acteur actix encapsulant le harness d'agent : agent rig outillé, garde-fous,
//! journal de session et compaction du contexte. L'historique vit dans l'état de l'acteur.
//! Le mode d'appel d'outils (natif ou texte) est choisi au démarrage et à chaque `Reset`.

use std::time::Duration;

use actix::prelude::*;
use rig_agent::completion::{Message, PromptError};
use rig_agent::{Agent, AgentBuilder};
use rig_core::providers::ollama::OllamaConfig;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{AppConfig, CONFIG_FILE};
use crate::harness::guard::{Approval, Guard};
use crate::harness::probe::{self, ToolMode};
use crate::harness::session::{Event, SessionStore};
use crate::harness::{compact, prompt, text_loop};
use crate::tools::{EditFile, ListDir, ReadFile, RunBash, Workspace, WriteFile, text};

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

/// Demande le mode d'appel d'outils retenu (après détection éventuelle).
#[derive(Message)]
#[rtype(result = "ToolMode")]
pub struct GetToolMode;

/// Acteur détenant l'agent, son journal de session et l'historique de la conversation.
pub struct AgentActor {
    /// Agent à appel d'outils natif (outils et garde-fous branchés sur rig).
    agent: Agent,
    /// Agent du mode texte : sans outils déclarés, mode d'emploi des balises dans le prompt.
    text_agent: Agent,
    summarizer: Agent,
    guard: Guard,
    max_turns: usize,
    /// Mode retenu ; `Auto` jusqu'à la fin de la détection.
    tool_mode: ToolMode,
    /// Serveur et modèle, pour la détection du mode.
    ollama_url: String,
    model: String,
    timeout: Duration,
    context_tokens: usize,
    session: SessionStore,
    approvals: UnboundedSender<Approval>,
    workspace: Workspace,
    history: Vec<Message>,
}

impl AgentActor {
    /// Prépare le répertoire de travail, construit l'agent et rejoue le journal de session.
    /// Les demandes d'autorisation des garde-fous partent sur `approvals`.
    pub fn new(cfg: &AppConfig, approvals: UnboundedSender<Approval>) -> std::io::Result<Self> {
        let protected = [CONFIG_FILE, cfg.session_file.as_str(), cfg.history_file.as_str()];
        let workspace = Workspace::new(&cfg.workspace)?.protecting(protected).sandboxed(cfg.sandbox);
        let mut actor = Self::build(cfg, approvals, workspace);
        actor.history = actor.session.replay()?;
        Ok(actor)
    }

    /// Construit un agent neuf, historique vide : prompt assemblé, outils, garde-fous, plafond de boucle.
    /// Le répertoire de travail, fixé au démarrage, est conservé tel quel.
    fn build(cfg: &AppConfig, approvals: UnboundedSender<Approval>, workspace: Workspace) -> Self {
        let ollama = OllamaConfig::new().with_base_url(&cfg.ollama_url).client();
        let preamble = prompt::assemble(&cfg.preamble, workspace.root());
        let guard = Guard::new(approvals.clone(), cfg.auto_approve);
        let text_agent =
            AgentBuilder::new(ollama.completion(&cfg.model)).preamble(format!("{preamble}\n\n{}", text::INSTRUCTIONS)).build();
        let agent = AgentBuilder::new(ollama.completion(&cfg.model))
            .preamble(preamble)
            .default_max_turns(cfg.max_turns)
            .add_hook(guard.clone())
            .tool(ListDir(workspace.clone()))
            .tool(ReadFile(workspace.clone()))
            .tool(EditFile(workspace.clone()))
            .tool(WriteFile(workspace.clone()))
            .tool(RunBash(workspace.clone()))
            .build();
        let summarizer = AgentBuilder::new(ollama.completion(&cfg.model)).preamble(SUMMARIZER_PREAMBLE).build();
        Self {
            agent,
            text_agent,
            summarizer,
            guard,
            max_turns: cfg.max_turns,
            tool_mode: cfg.tool_mode,
            ollama_url: cfg.ollama_url.clone(),
            model: cfg.model.clone(),
            timeout: Duration::from_secs(cfg.answer_timeout_secs),
            context_tokens: cfg.context_tokens,
            session: SessionStore::new(&cfg.session_file),
            approvals,
            workspace,
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

impl AgentActor {
    /// Résout le mode d'appel d'outils avant de traiter tout autre message
    /// (`ctx.wait` suspend la boîte aux lettres pendant la détection). Un modèle muet au-delà
    /// du délai de réponse ne bloque pas l'agent : mode texte.
    fn detect_tool_mode(&mut self, ctx: &mut Context<Self>) {
        let detection = probe::resolve(self.tool_mode, self.ollama_url.clone(), self.model.clone());
        let detection = tokio::time::timeout(self.timeout, detection);
        ctx.wait(detection.into_actor(self).map(|mode, act, _| act.tool_mode = mode.unwrap_or(ToolMode::Text)));
    }
}

impl Actor for AgentActor {
    type Context = Context<Self>;

    /// Au démarrage : détection du mode d'appel d'outils.
    fn started(&mut self, ctx: &mut Self::Context) {
        self.detect_tool_mode(ctx);
    }
}

impl Handler<GetToolMode> for AgentActor {
    type Result = MessageResult<GetToolMode>;

    /// Renvoie le mode retenu.
    fn handle(&mut self, _: GetToolMode, _ctx: &mut Self::Context) -> Self::Result {
        MessageResult(self.tool_mode)
    }
}

impl Handler<Ask> for AgentActor {
    type Result = AtomicResponse<Self, Result<String, AgentError>>;

    /// Compacte le contexte si besoin, lance la boucle d'agent (outils compris) puis journalise
    /// les nouveaux messages. `AtomicResponse` sérialise les tours et garde l'historique cohérent.
    /// Compaction et boucle sont chacune bornées par le délai de réponse.
    fn handle(&mut self, Ask(prompt): Ask, _ctx: &mut Self::Context) -> Self::Result {
        let (agent, text_agent, summarizer) = (self.agent.clone(), self.text_agent.clone(), self.summarizer.clone());
        let (guard, workspace, max_turns) = (self.guard.clone(), self.workspace.clone(), self.max_turns);
        let (timeout, context_tokens, native) = (self.timeout, self.context_tokens, self.tool_mode == ToolMode::Native);
        let mut history = std::mem::take(&mut self.history);
        let fut = async move {
            // Délai dépassé : historique intact (il n'est remplacé qu'une fois le résumé obtenu).
            let compaction = tokio::time::timeout(timeout, compact_if_needed(&summarizer, &mut history, context_tokens));
            let compaction = compaction.await.ok().flatten();
            let before = history.len();
            // En cas d'erreur, rig laisse `history` inchangé.
            let run = async {
                if native {
                    agent.chat(prompt, &mut history).await.map(|r| r.output()).map_err(AgentError::from)
                } else {
                    text_loop::chat(&text_agent, &guard, &workspace, max_turns, prompt, &mut history).await
                }
            };
            let res = tokio::time::timeout(timeout, run).await.unwrap_or(Err(AgentError::Timeout));
            (compaction, before, res, history)
        };
        AtomicResponse::new(Box::pin(fut.into_actor(self).map(
            |(compaction, before, res, history), act, _| {
                let events = compaction.map(|messages| Event::Compaction { messages }).into_iter();
                let mut events = events.chain(history[before..].iter().map(|m| Event::Message { message: m.clone() }));
                let logged = events.try_for_each(|e| act.session.append(e));
                act.history = history;
                // Le tour a eu lieu (outils compris) : la réponse est rendue, avec un avertissement.
                match logged {
                    Err(e) => res.map(|reply| format!("{reply}\n\n⚠ journal de session non écrit : {e}")),
                    Ok(()) => res,
                }
            },
        )))
    }
}

impl Handler<Reset> for AgentActor {
    type Result = ();

    /// Repart d'un agent neuf : nouvelle configuration, historique vide, remise à zéro journalisée,
    /// puis nouvelle détection du mode d'appel d'outils (le modèle a pu changer).
    fn handle(&mut self, Reset(cfg): Reset, ctx: &mut Self::Context) {
        *self = Self::build(&cfg, self.approvals.clone(), self.workspace.clone());
        // Échec d'écriture non bloquant : au pire, l'ancienne conversation sera rejouée au redémarrage.
        let _ = self.session.append(Event::Reset);
        self.detect_tool_mode(ctx);
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::tools::Permission;
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
            AgentActor::build(&cfg, unbounded_channel().0, Workspace::new(&cfg.workspace).unwrap())
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

    /// Mode texte : la balise du modèle est exécutée et son résultat lui est renvoyé ;
    /// les garde-fous s'appliquent (un appel non autorisé est refusé).
    #[actix_web::test]
    async fn text_mode_runs_tags_through_guard() {
        let cfg = AppConfig { tool_mode: ToolMode::Text, ..test_config() };
        let agent = AgentActor::new(&cfg, unbounded_channel().0).unwrap().start();
        let reply = say(&agent, "OUTIL echo bonjour").await.unwrap();
        assert!(reply.starts_with("outil : <result tool=\"run_bash\">\ncode de sortie : 0\nbonjour"), "{reply}");
        let reply = say(&agent, "OUTIL touch fichier-interdit").await.unwrap();
        assert!(reply.contains("refusé par l'utilisateur"), "{reply}");
        assert!(!std::path::Path::new("fichier-interdit").exists());
    }

    /// Mode `auto` : détecté au démarrage puis à chaque `Reset` (le modèle a pu changer).
    #[actix_web::test]
    async fn tool_mode_is_detected_on_start_and_reset() {
        let cfg = AppConfig { tool_mode: ToolMode::Auto, ..test_config() };
        let agent = AgentActor::new(&cfg, unbounded_channel().0).unwrap().start();
        assert_eq!(agent.send(GetToolMode).await.unwrap(), ToolMode::Native);
        agent.send(Reset(AppConfig { model: "tronque".into(), ..cfg })).await.unwrap();
        assert_eq!(agent.send(GetToolMode).await.unwrap(), ToolMode::Text);
        let reply = say(&agent, "OUTIL echo bonjour").await.unwrap();
        assert!(reply.contains("<result tool=\"run_bash\">"), "{reply}");
    }

    /// Mode texte : le même appel répété arrête l'exécution ; le plafond d'itérations aussi.
    #[actix_web::test]
    async fn text_mode_stops_stuck_loop_and_max_turns() {
        let cfg = AppConfig { tool_mode: ToolMode::Text, ..test_config() };
        let agent = AgentActor::new(&cfg, unbounded_channel().0).unwrap().start();
        let err = say(&agent, "BOUCLE").await.unwrap_err();
        assert!(matches!(&err, AgentError::Stopped(r) if r.contains("boucle détectée")), "{err}");
        let capped = AgentActor::new(&AppConfig { max_turns: 2, ..cfg }, unbounded_channel().0).unwrap().start();
        assert!(matches!(say(&capped, "BOUCLE").await, Err(AgentError::MaxTurns(2))));
    }

    /// Un modèle muet ne bloque ni la détection du mode (repli sur le texte) ni la compaction :
    /// chacune est bornée par le délai de réponse (1 s en test, le faux modèle répond en 10 s).
    #[actix_web::test]
    async fn silent_model_never_blocks() {
        let cfg = AppConfig { model: "lent".into(), tool_mode: ToolMode::Auto, context_tokens: 1, ..test_config() };
        for i in 0..4 {
            let message = Event::Message { message: Message::user(format!("ancien {i}")) };
            SessionStore::new(&cfg.session_file).append(message).unwrap();
        }
        let start = std::time::Instant::now();
        let agent = AgentActor::new(&cfg, unbounded_channel().0).unwrap().start();
        assert_eq!(agent.send(GetToolMode).await.unwrap(), ToolMode::Text);
        assert!(matches!(say(&agent, "un").await, Err(AgentError::Timeout)));
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
    }

    /// Journal impossible à écrire : la réponse est quand même rendue, avec un avertissement.
    #[actix_web::test]
    async fn reply_survives_session_write_failure() {
        let (agent, cfg) = start_agent();
        std::fs::create_dir(&cfg.session_file).unwrap();
        let reply = say(&agent, "un").await.unwrap();
        assert!(reply.starts_with("2 messages : un") && reply.contains("journal de session non écrit"), "{reply}");
    }

    /// Les fichiers du harness situés dans le répertoire de travail sont interdits aux outils.
    #[actix_web::test]
    async fn harness_files_are_protected() {
        let cfg = test_config();
        let cfg = AppConfig { session_file: format!("{}/journal.jsonl", cfg.workspace), ..cfg };
        let actor = AgentActor::new(&cfg, unbounded_channel().0).unwrap();
        let args = serde_json::json!({"path": "journal.jsonl", "content": "faux"});
        let out = text::run(&actor.workspace, "write_file", args).await;
        assert!(out.contains("fichier protégé"), "{out}");
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
