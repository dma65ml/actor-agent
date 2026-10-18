//! Point d'entrée : charge la configuration, démarre `ConfigActor` et `ProxyAgent`,
//! puis lance l'interface choisie.
//!
//! Usage : `actor-agent [--cli] [--web]` ; les drapeaux priment sur `APP_UI` / `config.toml`
//! (les deux drapeaux = les deux interfaces en parallèle).

mod agent_actor;
mod cli_ui;
mod config;
mod proxy;
mod web_ui;

use actix::Actor;
use config::{AppConfig, ConfigActor, Ui};
use proxy::ProxyAgent;

/// Déduit l'interface des drapeaux CLI, sinon garde celle de la configuration.
fn ui_from_args(default: Ui) -> Ui {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag| args.iter().any(|a| a == flag);
    match (has("--cli"), has("--web")) {
        (true, true) => Ui::Both,
        (true, false) => Ui::Cli,
        (false, true) => Ui::Web,
        (false, false) => default,
    }
}

/// Démarre les acteurs et lance la ou les interfaces.
#[actix::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Configuration : {e}");
        std::process::exit(1)
    });
    let ui = ui_from_args(cfg.ui);
    let addr = cfg.web_addr.clone();
    let config = ConfigActor::new(cfg).start();
    let proxy = ProxyAgent::new(config.clone()).start();

    match ui {
        Ui::Cli => cli_ui::run(proxy, config).await?,
        Ui::Web => {
            println!("API Web sur http://{addr} (POST /chat, DELETE /sessions/{{id}}, GET|PATCH /config)");
            web_ui::server(proxy, config, &addr)?.await?
        }
        Ui::Both => {
            // Le serveur tourne en fond ; quitter le TUI arrête le programme.
            actix::spawn(web_ui::server(proxy.clone(), config.clone(), &addr)?);
            cli_ui::run(proxy, config).await?
        }
    }
    Ok(())
}
