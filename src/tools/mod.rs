//! Outils de l'agent, un fichier par outil, tous confinés dans le répertoire de travail
//! (`Workspace`). Ce module classe aussi chaque appel par niveau de permission.

mod edit_file;
mod list_dir;
mod read_file;
mod run_bash;
mod sandbox;
pub mod text;
mod workspace;
mod write_file;

use rig_core::tool::PortableTool;
use serde::Deserialize;

pub use edit_file::EditFile;
pub use list_dir::ListDir;
pub use read_file::ReadFile;
pub use run_bash::RunBash;
pub use sandbox::Sandbox;
pub use workspace::Workspace;
pub use write_file::WriteFile;

/// Niveau de permission, du moins au plus risqué (l'ordre sert aux comparaisons).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Lecture seule.
    Read,
    /// Modification du répertoire de travail.
    Workspace,
    /// Tout le reste : destructif, hors du répertoire de travail ou non analysable.
    Full,
}

/// Taille maximale d'une sortie d'outil renvoyée au modèle, en caractères.
const MAX_OUTPUT: usize = 20_000;

/// Permission requise par un appel d'outil (nom et arguments JSON bruts), vérifiée avant exécution.
/// Les outils de fichiers ne peuvent pas sortir du répertoire de travail : seul `run_bash` dépend des arguments.
pub fn permission(tool: &str, args: &str) -> Permission {
    match tool {
        ReadFile::NAME | ListDir::NAME => Permission::Read,
        WriteFile::NAME | EditFile::NAME => Permission::Workspace,
        RunBash::NAME => run_bash::permission(args),
        _ => Permission::Full,
    }
}

/// Tronque une sortie trop longue pour préserver le contexte du modèle.
fn truncate(text: String) -> String {
    match text.char_indices().nth(MAX_OUTPUT) {
        Some((cut, _)) => format!("{}\n[… sortie tronquée]", &text[..cut]),
        None => text,
    }
}

/// Répertoire de travail temporaire et vide, propre à un test.
#[cfg(test)]
fn temp_workspace(name: &str) -> Workspace {
    let dir = std::env::temp_dir().join(format!("actor-agent-ws-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Workspace::new(dir).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Permission par outil ; arguments illisibles ou outil inconnu : `Full`.
    #[test]
    fn permission_per_tool() {
        assert_eq!(permission("read_file", r#"{"path": "a"}"#), Permission::Read);
        assert_eq!(permission("list_dir", "{}"), Permission::Read);
        assert_eq!(permission("write_file", r#"{"path": "a", "content": ""}"#), Permission::Workspace);
        assert_eq!(permission("edit_file", "{}"), Permission::Workspace);
        assert_eq!(permission("run_bash", r#"{"command": "pwd"}"#), Permission::Read);
        assert_eq!(permission("run_bash", "pas du json"), Permission::Full);
        assert_eq!(permission("inconnu", "{}"), Permission::Full);
    }

    /// Une sortie trop longue est tronquée et signalée.
    #[test]
    fn long_output_is_truncated() {
        let text = truncate("é".repeat(MAX_OUTPUT + 1));
        assert!(text.ends_with("[… sortie tronquée]"));
        assert_eq!(text.split('\n').next().unwrap().chars().count(), MAX_OUTPUT);
    }
}
