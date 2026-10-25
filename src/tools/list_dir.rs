//! Outil `list_dir` : liste le contenu d'un répertoire du répertoire de travail.

use rig_core::tool::PortableTool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Workspace, truncate};

/// Arguments de `list_dir` ; sans chemin, la racine du répertoire de travail.
#[derive(Deserialize)]
pub struct ListArgs {
    #[serde(default = "root")]
    path: String,
}

/// Chemin par défaut : la racine.
fn root() -> String {
    ".".into()
}

/// Liste un répertoire (non récursif).
pub struct ListDir(pub Workspace);

impl PortableTool for ListDir {
    const NAME: &'static str = "list_dir";
    type Args = ListArgs;
    type Output = String;
    type Error = std::io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Liste les entrées d'un répertoire, une par ligne, les répertoires suffixés par /.".into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string", "description": "chemin relatif (défaut : racine)"}}})
    }

    /// Renvoie les entrées triées par nom.
    async fn call(&self, args: ListArgs) -> std::io::Result<String> {
        let mut entries = tokio::fs::read_dir(self.0.resolve(&args.path)?).await?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await?.is_dir() {
                name.push('/');
            }
            names.push(name);
        }
        names.sort();
        Ok(truncate(names.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::temp_workspace;

    /// Liste la racine par défaut ; refuse un répertoire externe.
    #[tokio::test]
    async fn lists_inside_only() {
        let ws = temp_workspace("list");
        std::fs::create_dir(ws.root().join("src")).unwrap();
        std::fs::write(ws.root().join("b.txt"), "").unwrap();
        let tool = ListDir(ws);
        assert_eq!(tool.call(serde_json::from_value(json!({})).unwrap()).await.unwrap(), "b.txt\nsrc/");
        assert!(tool.call(serde_json::from_value(json!({"path": "/"})).unwrap()).await.is_err());
    }
}
