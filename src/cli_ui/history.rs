//! Historique des messages envoyés, rappelables avec ↑ / ↓ et conservés d'une séance à l'autre
//! dans un fichier JSON Lines (une chaîne JSON par ligne, sauts de ligne compris).

use std::io::Write;
use std::path::PathBuf;

/// Nombre maximal d'entrées chargées au démarrage.
const MAX_LOADED: usize = 1000;

/// Messages envoyés et position de rappel.
pub(super) struct History {
    entries: Vec<String>,
    path: PathBuf,
    /// Position pendant le rappel (`None` = saisie libre).
    recall: Option<usize>,
}

impl History {
    /// Charge les `MAX_LOADED` dernières entrées de `path` ; fichier absent ou lignes illisibles ignorés.
    // ponytail: le fichier n'est jamais tronqué ; à purger s'il devient encombrant.
    pub(super) fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut entries: Vec<String> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        entries.drain(..entries.len().saturating_sub(MAX_LOADED));
        Self { entries, path, recall: None }
    }

    /// Ajoute un message envoyé (sauf doublon du précédent) et l'enregistre ; un échec d'écriture est ignoré.
    pub(super) fn push(&mut self, entry: &str) {
        self.recall = None;
        if self.entries.last().is_some_and(|last| last == entry) {
            return;
        }
        self.entries.push(entry.to_owned());
        let line = serde_json::Value::from(entry).to_string();
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path);
        let _ = file.and_then(|mut f| writeln!(f, "{line}"));
    }

    /// Entrée précédente (s'arrête à la plus ancienne) ; `None` si l'historique est vide.
    pub(super) fn previous(&mut self) -> Option<&str> {
        let last = self.entries.len().checked_sub(1)?;
        let i = self.recall.map_or(last, |i| i.saturating_sub(1));
        self.recall = Some(i);
        Some(&self.entries[i])
    }

    /// Entrée suivante ; après la plus récente, une saisie vide. `None` hors rappel.
    pub(super) fn next(&mut self) -> Option<&str> {
        let i = self.recall?;
        self.recall = (i + 1 < self.entries.len()).then_some(i + 1);
        Some(self.recall.map_or("", |i| &self.entries[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rappel borné aux deux extrémités, doublons consécutifs ignorés, et reprise à la séance suivante.
    #[test]
    fn recalls_and_persists() {
        let path = std::env::temp_dir().join(format!("actor-agent-history-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut history = History::load(&path);
        assert_eq!(history.previous(), None);
        assert_eq!(history.next(), None);
        for entry in ["un", "deux\nlignes", "deux\nlignes"] {
            history.push(entry);
        }
        std::fs::write(&path, format!("{}pas du json\n", std::fs::read_to_string(&path).unwrap())).unwrap();

        let mut history = History::load(&path);
        assert_eq!(history.previous(), Some("deux\nlignes"));
        assert_eq!(history.previous(), Some("un"));
        assert_eq!(history.previous(), Some("un"));
        assert_eq!(history.next(), Some("deux\nlignes"));
        assert_eq!(history.next(), Some(""));
        assert_eq!(history.next(), None, "rappel terminé");
    }
}
