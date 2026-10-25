//! Outil `write_file` : crée ou remplace un fichier texte du répertoire de travail.

use rig_core::tool::PortableTool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Workspace;

/// Arguments de `write_file`.
#[derive(Deserialize)]
pub struct WriteArgs {
    path: String,
    content: String,
}

/// Crée ou remplace un fichier texte.
pub struct WriteFile(pub Workspace);

impl PortableTool for WriteFile {
    const NAME: &'static str = "write_file";
    type Args = WriteArgs;
    type Output = String;
    type Error = std::io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Crée ou remplace entièrement un fichier texte (répertoires parents créés au besoin). \
         Pour modifier un passage, préférer edit_file."
            .into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "chemin relatif"},
            "content": {"type": "string", "description": "contenu complet du fichier"}
        }, "required": ["path", "content"]})
    }

    /// Écrit le fichier et confirme le nombre d'octets écrits.
    async fn call(&self, args: WriteArgs) -> std::io::Result<String> {
        let path = self.0.resolve(&args.path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, &args.content).await?;
        Ok(format!("{} octets écrits dans {}", args.content.len(), args.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::temp_workspace;

    /// Écrit dans un sous-répertoire créé au besoin ; refuse d'écrire dehors.
    #[tokio::test]
    async fn writes_inside_only() {
        let ws = temp_workspace("write");
        let root = ws.root().to_path_buf();
        let tool = WriteFile(ws);
        let write = |path: &str| tool.call(serde_json::from_value(json!({"path": path, "content": "bonjour"})).unwrap());
        assert_eq!(write("sous/a.txt").await.unwrap(), "7 octets écrits dans sous/a.txt");
        assert_eq!(std::fs::read_to_string(root.join("sous/a.txt")).unwrap(), "bonjour");
        assert!(write("../dehors.txt").await.is_err());
        assert!(!root.join("../dehors.txt").exists());
    }
}
