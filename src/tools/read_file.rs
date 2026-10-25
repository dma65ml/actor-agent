//! Outil `read_file` : lit un fichier texte du répertoire de travail.

use rig_core::tool::PortableTool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Workspace, truncate};

/// Arguments de `read_file`.
#[derive(Deserialize)]
pub struct ReadArgs {
    path: String,
}

/// Lit un fichier texte.
pub struct ReadFile(pub Workspace);

impl PortableTool for ReadFile {
    const NAME: &'static str = "read_file";
    type Args = ReadArgs;
    type Output = String;
    type Error = std::io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Lit un fichier texte du répertoire de travail et renvoie son contenu.".into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string", "description": "chemin relatif"}}, "required": ["path"]})
    }

    /// Renvoie le contenu du fichier (tronqué s'il est trop long).
    async fn call(&self, args: ReadArgs) -> std::io::Result<String> {
        Ok(truncate(tokio::fs::read_to_string(self.0.resolve(&args.path)?).await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::temp_workspace;

    /// Lit un fichier interne ; un fichier absent ou externe est une erreur.
    #[tokio::test]
    async fn reads_inside_only() {
        let ws = temp_workspace("read");
        std::fs::write(ws.root().join("a.txt"), "bonjour").unwrap();
        let tool = ReadFile(ws);
        let read = |path: &str| tool.call(serde_json::from_value(json!({"path": path})).unwrap());
        assert_eq!(read("a.txt").await.unwrap(), "bonjour");
        assert!(read("absent").await.is_err());
        assert!(read("/etc/hosts").await.is_err());
    }
}
