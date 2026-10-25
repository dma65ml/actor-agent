//! Assemblage du prompt système : la base statique d'abord (préserve le cache de préfixe),
//! puis les fichiers d'instructions `AGENTS.md` / `CLAUDE.md` trouvés de la racine jusqu'au
//! répertoire courant, le plus spécifique en dernier pour qu'il l'emporte.

use std::path::{Path, PathBuf};

/// Noms des fichiers d'instructions, comparés sans tenir compte de la casse.
const INSTRUCTION_FILES: [&str; 2] = ["agents.md", "claude.md"];

/// Construit le prompt système : `base`, puis chaque fichier d'instructions non vide
/// de la racine jusqu'à `cwd`, sous un titre donnant son chemin.
pub fn assemble(base: &str, cwd: &Path) -> String {
    let mut dirs: Vec<&Path> = cwd.ancestors().collect();
    dirs.reverse();
    let files = dirs.into_iter().flat_map(instruction_files);
    let mut parts = vec![base.to_owned()];
    for path in files {
        // Fichier illisible (droits, répertoire homonyme) : ignoré, comme un fichier vide.
        if let Ok(text) = std::fs::read_to_string(&path)
            && !text.trim().is_empty()
        {
            parts.push(format!("## Instructions de {}\n\n{}", path.display(), text.trim()));
        }
    }
    parts.join("\n\n")
}

/// Fichiers d'instructions d'un répertoire, triés. La comparaison sans casse évite de lire
/// deux fois le même fichier sur un système de fichiers insensible à la casse (macOS).
fn instruction_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| INSTRUCTION_FILES.contains(&e.file_name().to_string_lossy().to_lowercase().as_str()))
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les instructions s'ajoutent après la base, du parent vers l'enfant ; les fichiers vides sont ignorés.
    #[test]
    fn assembles_base_then_instructions_from_root_to_cwd() {
        let root = std::env::temp_dir().join(format!("actor-agent-prompt-{}", std::process::id()));
        let child = root.join("projet");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(root.join("AGENTS.md"), "règle générale").unwrap();
        std::fs::write(child.join("claude.md"), "règle du projet").unwrap();
        std::fs::write(child.join("AGENTS.md"), "  \n").unwrap();

        let prompt = assemble("BASE", &child);
        assert!(prompt.starts_with("BASE\n\n"));
        let general = prompt.find("règle générale").unwrap();
        let project = prompt.find("règle du projet").unwrap();
        assert!(general < project, "le plus spécifique doit venir en dernier");
        assert_eq!(prompt.matches("## Instructions de").count(), 2);
    }
}
