//! Point d'entrée : charge la configuration, démarre `AgentActor` (en reprenant la session) puis lance le TUI.

use actix::Actor;
use actor_agent::agent_actor::AgentActor;
use actor_agent::cli_ui;
use actor_agent::config::AppConfig;
use tokio::sync::mpsc::unbounded_channel;

/// Démarre l'agent et lance le TUI.
#[actix::main]
async fn main() -> std::io::Result<()> {
    let cfg = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Configuration : {e}");
        std::process::exit(1)
    });
    // Canal des demandes d'autorisation : des garde-fous de l'agent vers le TUI.
    let (approvals, approval_rx) = unbounded_channel();
    let agent = AgentActor::new(&cfg, approvals).unwrap_or_else(|e| {
        eprintln!("Session ({}) : {e}", cfg.session_file);
        std::process::exit(1)
    });
    cli_ui::run(agent.start(), cfg, approval_rx).await
}
