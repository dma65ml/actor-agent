//! Configuration de l'application et acteur `ConfigActor` qui la détient.
//!
//! Priorité de chargement : valeurs par défaut < fichier `config.toml` (créé avec les défauts s'il manque)
//! < variables d'environnement (`OLLAMA_MODEL`, `APP_PREAMBLE`, `APP_UI`, `WEB_ADDR`).

use std::str::FromStr;

use actix::prelude::*;
use serde::{Deserialize, Serialize};

/// Fichier de configuration lu (s'il existe) dans le répertoire courant.
const CONFIG_FILE: &str = "config.toml";

/// Erreurs de chargement de la configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Le fichier existe mais n'a pas pu être lu.
    #[error("lecture de {CONFIG_FILE} impossible : {0}")]
    Io(#[from] std::io::Error),
    /// Le fichier n'est pas un TOML valide pour `AppConfig`.
    #[error("{CONFIG_FILE} invalide : {0}")]
    Toml(#[from] toml::de::Error),
    /// Valeur inconnue pour le choix d'interface.
    #[error("interface inconnue « {0} » (attendu : cli, web ou both)")]
    Ui(String),
}

/// Interface(s) lancée(s) au démarrage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ui {
    #[default]
    Cli,
    Web,
    Both,
}

impl FromStr for Ui {
    type Err = ConfigError;

    /// Lit `cli`, `web` ou `both` (insensible à la casse).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cli" => Ok(Self::Cli),
            "web" => Ok(Self::Web),
            "both" => Ok(Self::Both),
            _ => Err(ConfigError::Ui(s.into())),
        }
    }
}

/// Configuration complète ; les champs absents du fichier prennent leur valeur par défaut.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Modèle Ollama des nouveaux agents.
    pub model: String,
    /// Consigne système des nouveaux agents.
    pub preamble: String,
    /// Interface(s) à lancer.
    pub ui: Ui,
    /// Adresse d'écoute de l'API Web.
    pub web_addr: String,
}

impl Default for AppConfig {
    /// Valeurs par défaut sûres : serveur Web local uniquement.
    fn default() -> Self {
        Self {
            model: "llama3.2".into(),
            preamble: "Tu es un assistant concis. Réponds en français.".into(),
            ui: Ui::Cli,
            web_addr: "127.0.0.1:8080".into(),
        }
    }
}

impl AppConfig {
    /// Charge la configuration (défauts, puis fichier facultatif, puis environnement).
    pub fn load() -> Result<Self, ConfigError> {
        let mut cfg: Self = match std::fs::read_to_string(CONFIG_FILE) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::default();
                // Non bloquant : un répertoire en lecture seule ne doit pas empêcher le démarrage.
                if let Err(e) = toml::to_string_pretty(&cfg)
                    .map_err(std::io::Error::other)
                    .and_then(|text| std::fs::write(CONFIG_FILE, text))
                {
                    eprintln!("Avertissement : création de {CONFIG_FILE} impossible : {e}");
                }
                cfg
            }
            Err(e) => return Err(e.into()),
        };
        let env = |key| std::env::var(key).ok();
        if let Some(v) = env("OLLAMA_MODEL") {
            cfg.model = v;
        }
        if let Some(v) = env("APP_PREAMBLE") {
            cfg.preamble = v;
        }
        if let Some(v) = env("APP_UI") {
            cfg.ui = v.parse()?;
        }
        if let Some(v) = env("WEB_ADDR") {
            cfg.web_addr = v;
        }
        Ok(cfg)
    }
}

/// Demande une copie de la configuration courante.
#[derive(Message)]
#[rtype(result = "AppConfig")]
pub struct GetConfig;

/// Modifie un paramètre à chaud et renvoie la configuration résultante.
/// Seuls les paramètres relus à l'exécution sont modifiables : l'UI et l'adresse Web
/// ne servent qu'au démarrage. Format JSON : `{"model": "..."}` ou `{"preamble": "..."}`.
#[derive(Message, Deserialize)]
#[rtype(result = "AppConfig")]
#[serde(rename_all = "lowercase")]
pub enum UpdateConfig {
    Model(String),
    Preamble(String),
}

/// Acteur détenant la configuration : la boîte aux lettres sérialise lectures et écritures.
pub struct ConfigActor(AppConfig);

impl ConfigActor {
    /// Crée l'acteur à partir d'une configuration déjà chargée.
    pub fn new(config: AppConfig) -> Self {
        Self(config)
    }
}

impl Actor for ConfigActor {
    type Context = Context<Self>;
}

impl Handler<GetConfig> for ConfigActor {
    type Result = MessageResult<GetConfig>;

    /// Renvoie une copie de la configuration.
    fn handle(&mut self, _: GetConfig, _ctx: &mut Self::Context) -> Self::Result {
        MessageResult(self.0.clone())
    }
}

impl Handler<UpdateConfig> for ConfigActor {
    type Result = MessageResult<UpdateConfig>;

    /// Applique la modification puis renvoie la nouvelle configuration.
    fn handle(&mut self, msg: UpdateConfig, _ctx: &mut Self::Context) -> Self::Result {
        match msg {
            UpdateConfig::Model(m) => self.0.model = m,
            UpdateConfig::Preamble(p) => self.0.preamble = p,
        }
        MessageResult(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un fichier partiel garde les valeurs par défaut des champs absents.
    #[test]
    fn partial_toml_keeps_defaults() {
        let cfg: AppConfig = toml::from_str("model = \"qwen\"\nui = \"both\"").unwrap();
        assert_eq!(cfg.model, "qwen");
        assert_eq!(cfg.ui, Ui::Both);
        assert_eq!(cfg.web_addr, AppConfig::default().web_addr);
    }

    /// `APP_UI` accepte les trois valeurs et rejette le reste.
    #[test]
    fn ui_parsing() {
        assert_eq!("WEB".parse::<Ui>().unwrap(), Ui::Web);
        assert!("tui".parse::<Ui>().is_err());
    }
}
