//! Choix du mode d'appel d'outils adapté au modèle. En mode `auto`, un appel d'outil d'essai
//! dont l'argument contient guillemets et saut de ligne doit revenir intact pour garder
//! l'appel natif ; sinon (troncature, appel écrit en texte, outils non pris en charge) : mode texte.

use std::fmt;
use std::sync::{Arc, Mutex};

use rig_agent::AgentBuilder;
use rig_core::providers::ollama::OllamaConfig;
use rig_core::tool::PortableTool;
use serde::Deserialize;
use serde_json::{Value, json};

/// Façon dont le modèle appelle les outils.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolMode {
    /// À détecter (valeur de configuration).
    Auto,
    /// Appel d'outils natif de l'API (arguments JSON).
    Native,
    /// Balises dans le texte de la réponse (voir `tools::text`).
    Text,
}

impl fmt::Display for ToolMode {
    /// Nom affiché à l'utilisateur.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "à détecter",
            Self::Native => "appel natif",
            Self::Text => "balises texte",
        })
    }
}

/// Texte que le modèle doit transmettre intact.
const SAMPLE: &str = "print(\"a\")\nprint('b')";

/// Outil d'essai : retient le texte reçu.
struct Probe(Arc<Mutex<Option<String>>>);

/// Arguments de l'outil d'essai.
#[derive(Deserialize)]
struct ProbeArgs {
    text: String,
}

impl PortableTool for Probe {
    const NAME: &'static str = "save_text";
    type Args = ProbeArgs;
    type Output = String;
    type Error = std::io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Enregistre un texte.".into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }

    /// Retient le texte reçu.
    async fn call(&self, args: ProbeArgs) -> std::io::Result<String> {
        *self.0.lock().unwrap() = Some(args.text);
        Ok("enregistré".into())
    }
}

/// Résout `mode` : `auto` devient `native` si le modèle passe l'essai, `text` sinon.
pub async fn resolve(mode: ToolMode, ollama_url: String, model: String) -> ToolMode {
    if mode != ToolMode::Auto {
        return mode;
    }
    let received = Arc::new(Mutex::new(None));
    let client = OllamaConfig::new().with_base_url(&ollama_url).client();
    let agent = AgentBuilder::new(client.completion(&model)).default_max_turns(2).tool(Probe(received.clone())).build();
    let prompt = format!("Appelle l'outil save_text avec exactement ce texte, sans rien changer :\n{SAMPLE}");
    // Seul compte ce que l'outil a reçu : la réponse finale (ou l'erreur) est ignorée.
    let _ = agent.chat(prompt, &mut Vec::new()).await;
    let intact = received.lock().unwrap().as_deref() == Some(SAMPLE);
    if intact { ToolMode::Native } else { ToolMode::Text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fake_ollama;

    /// Le faux serveur transmet l'essai intact sauf pour le modèle `tronque` ; un mode fixé est gardé.
    #[actix_web::test]
    async fn detects_mode() {
        let url = fake_ollama();
        assert_eq!(resolve(ToolMode::Auto, url.clone(), "bon".into()).await, ToolMode::Native);
        assert_eq!(resolve(ToolMode::Auto, url.clone(), "tronque".into()).await, ToolMode::Text);
        assert_eq!(resolve(ToolMode::Auto, "http://127.0.0.1:1".into(), "bon".into()).await, ToolMode::Text);
        assert_eq!(resolve(ToolMode::Native, url, "tronque".into()).await, ToolMode::Native);
    }
}
