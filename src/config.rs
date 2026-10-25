//! Configuration de l'application.
//!
//! Priorité de chargement : valeurs par défaut < fichier `config.toml` (facultatif)
//! < variables d'environnement (`OLLAMA_API_BASE_URL`, `OLLAMA_MODEL`, `APP_PREAMBLE`).

use std::path::Path;

use serde::Deserialize;

use crate::harness::tools::Permission;

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
}

/// Configuration complète ; les champs absents du fichier prennent leur valeur par défaut.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// URL du serveur Ollama.
    pub ollama_url: String,
    /// Modèle Ollama de l'agent.
    pub model: String,
    /// Base statique du prompt système (les fichiers d'instructions s'y ajoutent).
    pub preamble: String,
    /// Délai maximal accordé à l'agent pour répondre, en secondes (attente d'accord comprise).
    pub answer_timeout_secs: u64,
    /// Nombre maximal d'appels au modèle par message (plafond de la boucle d'agent).
    pub max_turns: usize,
    /// Taille de la fenêtre de contexte du modèle, en jetons (compaction à 80 %).
    pub context_tokens: usize,
    /// Journal de session JSON Lines, rejoué au démarrage.
    pub session_file: String,
    /// Niveau de permission accordé sans confirmation (`read`, `workspace` ou `full`).
    pub auto_approve: Permission,
}

impl Default for AppConfig {
    /// Valeurs par défaut : serveur Ollama local.
    fn default() -> Self {
        Self {
            ollama_url: "http://localhost:11434".into(),
            model: "llama3.2".into(),
            preamble: "Tu es un agent de développement. Tu disposes des outils read_file, write_file \
                       et run_bash. Réponds en français, de façon concise."
                .into(),
            answer_timeout_secs: 600,
            max_turns: 50,
            context_tokens: 8192,
            session_file: ".actor-agent-session.jsonl".into(),
            auto_approve: Permission::Read,
        }
    }
}

impl AppConfig {
    /// Charge la configuration depuis `config.toml` et les variables d'environnement.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(Path::new(CONFIG_FILE), |key| std::env::var(key).ok())
    }

    /// Charge depuis `path` (défauts s'il manque) puis applique `env`.
    /// Les sources sont injectées pour pouvoir tester sans toucher l'environnement réel.
    fn load_from(path: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut cfg: Self = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
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
        Ok(cfg)
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

    /// Fichier absent : défauts, puis variables d'environnement ; aucun fichier n'est créé.
    #[test]
    fn missing_file_uses_defaults_and_env() {
        let path = temp_dir("missing").join(CONFIG_FILE);
        let vars = [
            ("OLLAMA_API_BASE_URL", "http://distant:11434"),
            ("OLLAMA_MODEL", "qwen"),
            ("APP_PREAMBLE", "Sois bref."),
        ];
        let cfg = AppConfig::load_from(&path, env(&vars)).unwrap();
        assert_eq!(
            (cfg.ollama_url.as_str(), cfg.model.as_str(), cfg.preamble.as_str()),
            ("http://distant:11434", "qwen", "Sois bref.")
        );
        assert!(!path.exists());
    }

    /// Fichier présent et partiel : ses valeurs priment, les champs absents gardent leur défaut.
    #[test]
    fn existing_file_is_read() {
        let path = temp_dir("existing").join(CONFIG_FILE);
        std::fs::write(&path, "model = \"qwen\"\nauto_approve = \"workspace\"").unwrap();
        let cfg = AppConfig::load_from(&path, env(&[])).unwrap();
        assert_eq!((cfg.model.as_str(), cfg.auto_approve), ("qwen", Permission::Workspace));
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
}
