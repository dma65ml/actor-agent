//! Outil `edit_file` : remplace un passage précis d'un fichier du répertoire de travail.
//! Moins coûteux en jetons et moins risqué que de réécrire tout le fichier.

use std::io;

use rig_core::tool::PortableTool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Workspace;

/// Arguments de `edit_file`.
#[derive(Deserialize)]
pub struct EditArgs {
    path: String,
    old_str: String,
    new_str: String,
}

/// Remplace l'unique occurrence d'un passage dans un fichier.
pub struct EditFile(pub Workspace);

impl PortableTool for EditFile {
    const NAME: &'static str = "edit_file";
    type Args = EditArgs;
    type Output = String;
    type Error = io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Remplace dans un fichier le passage old_str, qui doit y apparaître exactement une fois, par new_str.".into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "chemin relatif"},
            "old_str": {"type": "string", "description": "passage exact à remplacer"},
            "new_str": {"type": "string", "description": "texte de remplacement"}
        }, "required": ["path", "old_str", "new_str"]})
    }

    /// Remplace le passage ; absent ou ambigu, le fichier reste intact et le modèle reçoit l'erreur.
    async fn call(&self, args: EditArgs) -> io::Result<String> {
        let path = self.0.resolve(&args.path)?;
        let text = tokio::fs::read_to_string(&path).await?;
        match text.matches(&args.old_str).count() {
            1 => tokio::fs::write(&path, text.replacen(&args.old_str, &args.new_str, 1)).await?,
            n => {
                let msg = format!("old_str trouvé {n} fois dans {} : il doit l'être exactement une fois", args.path);
                return Err(io::Error::new(io::ErrorKind::InvalidInput, msg));
            }
        }
        Ok(format!("{} modifié", args.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::temp_workspace;

    /// Remplace une occurrence unique ; absente ou multiple, le fichier n'est pas touché.
    #[tokio::test]
    async fn edits_unique_occurrence() {
        let ws = temp_workspace("edit");
        let file = ws.root().join("a.txt");
        std::fs::write(&file, "un deux deux").unwrap();
        let tool = EditFile(ws);
        let edit = |old: &str| {
            tool.call(serde_json::from_value(json!({"path": "a.txt", "old_str": old, "new_str": "1"})).unwrap())
        };
        assert_eq!(edit("un").await.unwrap(), "a.txt modifié");
        assert!(edit("deux").await.unwrap_err().to_string().contains("2 fois"));
        assert!(edit("trois").await.unwrap_err().to_string().contains("0 fois"));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "1 deux deux");
    }
}
