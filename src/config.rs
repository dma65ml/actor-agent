//! Configuration de l'application et acteur `ConfigActor` qui la détient.
//!
//! Priorité de chargement : valeurs par défaut < fichier `config.toml` (créé avec les défauts s'il manque)
//! < variables d'environnement (`OLLAMA_API_BASE_URL`, `OLLAMA_MODEL`, `APP_PREAMBLE`, `APP_UI`, `WEB_ADDR`).

use std::path::Path;
use std::str::FromStr;

use actix::prelude::*;
use serde::{Deserialize, Serialize};

/// Fichier de configuration lu (s'il existe) dans le répertoire courant.
const CONFIG_FILE: &str = "config.toml";

/// Erreurs de chargement de la configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Le fichier existe mais n'a pas pu être lu.
    #[error("lecture du fichier de configuration impossible : {0}")]
    Io(#[from] std::io::Error),
    /// Le fichier n'est pas un TOML valide pour `AppConfig`.
    #[error("fichier de configuration invalide : {0}")]
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
    /// URL du serveur Ollama.
    pub ollama_url: String,
    /// Modèle Ollama des nouveaux agents.
    pub model: String,
    /// Consigne système des nouveaux agents.
    pub preamble: String,
    /// Interface(s) à lancer.
    pub ui: Ui,
    /// Adresse d'écoute de l'API Web.
    pub web_addr: String,
    /// Délai maximal accordé à l'agent pour répondre, en secondes.
    pub answer_timeout_secs: u64,
}

impl Default for AppConfig {
    /// Valeurs par défaut sûres : serveur Web local uniquement.
    fn default() -> Self {
        Self {
            ollama_url: "http://localhost:11434".into(),
            model: "llama3.2".into(),
            preamble: "Tu es un assistant concis. Réponds en français.".into(),
            ui: Ui::Cli,
            web_addr: "127.0.0.1:8080".into(),
            answer_timeout_secs: 120,
        }
    }
}

impl AppConfig {
    /// Charge la configuration depuis `config.toml` et les variables d'environnement.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(Path::new(CONFIG_FILE), |key| std::env::var(key).ok())
    }

    /// Charge depuis `path` (créé avec les défauts s'il manque) puis applique `env`.
    /// Les sources sont injectées pour pouvoir tester sans toucher l'environnement réel.
    fn load_from(path: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut cfg: Self = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::default();
                // Non bloquant : un répertoire en lecture seule ne doit pas empêcher le démarrage.
                if let Err(e) = toml::to_string_pretty(&cfg)
                    .map_err(std::io::Error::other)
                    .and_then(|text| std::fs::write(path, text))
                {
                    eprintln!("Avertissement : création de {} impossible : {e}", path.display());
                }
                cfg
            }
            Err(e) => return Err(e.into()),
        };
        if let Some(v) = env("OLLAMA_API_BASE_URL") {
            cfg.ollama_url = v;
        }
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
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;

    /// Répertoire temporaire propre à un test (vidé s'il existait).
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("actor-agent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Environnement simulé à partir de paires clé/valeur.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |key| map.get(key).cloned()
    }

    /// Fichier absent : il est créé avec les défauts, l'environnement ne s'applique qu'en mémoire.
    #[test]
    fn missing_file_is_created_with_defaults() {
        let path = temp_dir("missing").join(CONFIG_FILE);
        let vars = [
            ("OLLAMA_API_BASE_URL", "http://distant:11434"),
            ("OLLAMA_MODEL", "qwen"),
            ("APP_PREAMBLE", "Sois bref."),
            ("APP_UI", "web"),
            ("WEB_ADDR", "0.0.0.0:9000"),
        ];
        let cfg = AppConfig::load_from(&path, env(&vars)).unwrap();
        assert_eq!(
            (cfg.ollama_url.as_str(), cfg.model.as_str(), cfg.preamble.as_str(), cfg.ui, cfg.web_addr.as_str()),
            ("http://distant:11434", "qwen", "Sois bref.", Ui::Web, "0.0.0.0:9000")
        );
        let written: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written.model, AppConfig::default().model);
    }

    /// Fichier présent et partiel : ses valeurs priment, les champs absents gardent leur défaut.
    #[test]
    fn existing_file_is_read() {
        let path = temp_dir("existing").join(CONFIG_FILE);
        std::fs::write(&path, "model = \"qwen\"\nui = \"both\"").unwrap();
        let cfg = AppConfig::load_from(&path, env(&[])).unwrap();
        assert_eq!((cfg.model.as_str(), cfg.ui), ("qwen", Ui::Both));
        assert_eq!(cfg.answer_timeout_secs, AppConfig::default().answer_timeout_secs);
    }

    /// TOML invalide : erreur explicite plutôt que valeurs par défaut silencieuses.
    #[test]
    fn invalid_file_is_rejected() {
        let path = temp_dir("invalid").join(CONFIG_FILE);
        std::fs::write(&path, "model = 42").unwrap();
        assert!(matches!(AppConfig::load_from(&path, env(&[])), Err(ConfigError::Toml(_))));
    }

    /// Chemin illisible (ici un répertoire) : erreur d'entrée/sortie remontée.
    #[test]
    fn unreadable_file_is_rejected() {
        let dir = temp_dir("unreadable");
        assert!(matches!(AppConfig::load_from(&dir, env(&[])), Err(ConfigError::Io(_))));
    }

    /// Création impossible (répertoire parent absent) : simple avertissement, défauts utilisés.
    #[test]
    fn creation_failure_is_not_fatal() {
        let path = temp_dir("nocreate").join("absent").join(CONFIG_FILE);
        assert_eq!(AppConfig::load_from(&path, env(&[])).unwrap().model, AppConfig::default().model);
    }

    /// `APP_UI` invalide : erreur explicite.
    #[test]
    fn invalid_ui_env_is_rejected() {
        let path = temp_dir("badui").join(CONFIG_FILE);
        let err = AppConfig::load_from(&path, env(&[("APP_UI", "tui")])).unwrap_err();
        assert!(err.to_string().contains("tui"));
    }

    /// Les trois interfaces sont reconnues sans tenir compte de la casse.
    #[test]
    fn ui_parsing() {
        assert_eq!("CLI".parse::<Ui>().unwrap(), Ui::Cli);
        assert_eq!("Web".parse::<Ui>().unwrap(), Ui::Web);
        assert_eq!("both".parse::<Ui>().unwrap(), Ui::Both);
    }

    /// `UpdateConfig` modifie le paramètre et renvoie la configuration résultante.
    #[actix_web::test]
    async fn actor_updates_and_returns_config() {
        let config = ConfigActor::new(AppConfig::default()).start();
        let updated = config.send(UpdateConfig::Model("qwen".into())).await.unwrap();
        assert_eq!(updated.model, "qwen");
        let updated = config.send(UpdateConfig::Preamble("Sois bref.".into())).await.unwrap();
        assert_eq!((updated.model.as_str(), updated.preamble.as_str()), ("qwen", "Sois bref."));
        assert_eq!(config.send(GetConfig).await.unwrap().preamble, "Sois bref.");
    }
}
