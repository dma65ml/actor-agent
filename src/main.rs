//! Point d'entrée : démarre le `ProxyAgent` puis l'interface choisie.
//!
//! Usage : `actor-agent [--cli] [--web]` (défaut : `--cli` ; les deux drapeaux = les deux en parallèle).
//! Variables : `OLLAMA_MODEL` (défaut `llama3.2`), `WEB_ADDR` (défaut `127.0.0.1:8080`).

mod agent_actor;
mod cli_ui;
mod proxy;
mod web_ui;

use actix::Actor;
use proxy::ProxyAgent;

/// Modèle Ollama utilisé par défaut.
const DEFAULT_MODEL: &str = "llama3.2";
/// Adresse d'écoute par défaut de l'API Web.
const DEFAULT_WEB_ADDR: &str = "127.0.0.1:8080";
/// Consigne système commune à tous les agents.
const PREAMBLE: &str = "Tu es un assistant concis. Réponds en français.";

/// Lit les drapeaux, démarre le proxy et lance la ou les interfaces.
#[actix::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let web = args.iter().any(|a| a == "--web");
    let cli = args.iter().any(|a| a == "--cli") || !web;

    let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
    let proxy = ProxyAgent::new(model, PREAMBLE).start();

    if !web {
        return cli_ui::run(proxy).await;
    }
    let addr = std::env::var("WEB_ADDR").unwrap_or_else(|_| DEFAULT_WEB_ADDR.into());
    let server = web_ui::server(proxy.clone(), &addr)?;
    if !cli {
        println!("API Web sur http://{addr} (POST /chat, DELETE /sessions/{{id}})");
        return server.await;
    }
    // Mode mixte : le serveur tourne en fond, quitter le TUI arrête le programme.
    actix::spawn(server);
    cli_ui::run(proxy).await
}
