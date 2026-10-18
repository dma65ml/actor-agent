//! Point d'entrée : charge la configuration, démarre `ConfigActor` et `ProxyAgent`,
//! puis lance l'interface choisie.
//!
//! Usage : `actor-agent [--cli] [--web]` ; les drapeaux priment sur `APP_UI` / `config.toml`
//! (les deux drapeaux = les deux interfaces en parallèle).

mod agent_actor;
mod cli_ui;
mod config;
mod proxy;
#[cfg(test)]
mod test_support;
mod web_ui;

use actix::Actor;
use config::{AppConfig, ConfigActor, Ui};
use proxy::ProxyAgent;

/// Déduit l'interface des drapeaux CLI, sinon garde celle de la configuration.
fn ui_from_args(args: &[String], default: Ui) -> Ui {
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ui = ui_from_args(&args, cfg.ui);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Lit les drapeaux d'une ligne de commande simulée.
    fn ui(args: &[&str], default: Ui) -> Ui {
        ui_from_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>(), default)
    }

    /// Les drapeaux priment sur la configuration ; sans drapeau, elle s'applique.
    #[test]
    fn flags_override_config() {
        assert_eq!(ui(&["--cli"], Ui::Web), Ui::Cli);
        assert_eq!(ui(&["--web"], Ui::Cli), Ui::Web);
        assert_eq!(ui(&["--web", "--cli"], Ui::Cli), Ui::Both);
        assert_eq!(ui(&[], Ui::Web), Ui::Web);
    }
}
