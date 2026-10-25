//! Répertoire de travail de l'agent : tout chemin donné par le modèle y est résolu et doit y rester,
//! y compris à travers les liens symboliques.

use std::io;
use std::path::{Path, PathBuf};

/// Racine canonique (liens résolus) dans laquelle les outils sont confinés.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Crée le répertoire s'il manque et retient sa forme canonique.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root: root.as_ref().canonicalize()? })
    }

    /// Racine du répertoire de travail.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Chemin réel de `path` (relatif à la racine), refusé s'il en sort.
    /// La partie existante est canonicalisée (liens résolus) ; la partie à créer ne peut contenir
    /// que des noms simples, sinon le chemin est refusé.
    pub fn resolve(&self, path: &str) -> io::Result<PathBuf> {
        let full = self.root.join(path);
        let mut existing = full.as_path();
        let mut missing = Vec::new();
        // `symlink_metadata` voit aussi les liens cassés : `canonicalize` les refusera.
        while existing.symlink_metadata().is_err() {
            match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    missing.push(name);
                    existing = parent;
                }
                _ => return Err(outside(path)),
            }
        }
        let mut real = existing.canonicalize()?;
        real.extend(missing.iter().rev());
        if real.starts_with(&self.root) { Ok(real) } else { Err(outside(path)) }
    }
}

/// Erreur renvoyée au modèle pour un chemin hors du répertoire de travail.
fn outside(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, format!("« {path} » sort du répertoire de travail"))
}

#[cfg(test)]
mod tests {
    use super::super::temp_workspace;

    /// Chemins internes acceptés, existants ou à créer, `..` compris tant qu'il reste dedans.
    #[test]
    fn resolves_inside() {
        let ws = temp_workspace("inside");
        std::fs::create_dir(ws.root().join("src")).unwrap();
        assert_eq!(ws.resolve(".").unwrap(), ws.root());
        assert_eq!(ws.resolve("a/b/c.txt").unwrap(), ws.root().join("a/b/c.txt"));
        assert_eq!(ws.resolve("src/../x").unwrap(), ws.root().join("x"));
    }

    /// Chemins absolus, remontées et liens symboliques vers l'extérieur refusés.
    #[test]
    fn refuses_outside() {
        let ws = temp_workspace("outside");
        std::os::unix::fs::symlink("/", ws.root().join("lien")).unwrap();
        std::os::unix::fs::symlink("/absent/cible", ws.root().join("casse")).unwrap();
        for path in ["/etc/hosts", "../x", "nouveau/../../x", "lien/etc/hosts", "casse"] {
            assert!(ws.resolve(path).is_err(), "{path}");
        }
    }
}
