//! Journal de session en JSON Lines, en ajout seul : chaque événement est une ligne horodatée.
//! Rien n'est jamais réécrit, donc un arrêt brutal ne peut corrompre que la dernière ligne ;
//! au démarrage, le rejeu reconstruit l'historique.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rig_agent::completion::Message;
use serde::{Deserialize, Serialize};

/// Événement du journal.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Message ajouté à l'historique (utilisateur, assistant ou résultat d'outil).
    Message { message: Message },
    /// Compaction : l'historique est remplacé par ces messages (résumé + messages récents).
    Compaction { messages: Vec<Message> },
    /// `/clear` : l'historique repart de zéro.
    Reset,
}

/// Ligne du fichier : l'événement et son horodatage (secondes Unix).
#[derive(Serialize, Deserialize)]
struct Line {
    ts: u64,
    #[serde(flatten)]
    event: Event,
}

/// Journal d'une session, stocké dans un fichier.
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    /// Journal stocké dans `path` (créé à la première écriture).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Ajoute un événement en fin de fichier. `File` n'a pas de tampon applicatif :
    /// la ligne est remise au système dès le retour de `write_all`.
    pub fn append(&self, event: Event) -> std::io::Result<()> {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut line = serde_json::to_string(&Line { ts, event })?;
        line.push('\n');
        OpenOptions::new().create(true).append(true).open(&self.path)?.write_all(line.as_bytes())
    }

    /// Rejoue le journal et renvoie l'historique courant (vide si le fichier n'existe pas).
    pub fn replay(&self) -> std::io::Result<Vec<Message>> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut history = Vec::new();
        for line in BufReader::new(file).lines() {
            // Ligne vide ou tronquée (arrêt brutal pendant l'écriture) : ignorée.
            let Ok(Line { event, .. }) = serde_json::from_str(&line?) else { continue };
            match event {
                Event::Message { message } => history.push(message),
                Event::Compaction { messages } => history = messages,
                Event::Reset => history.clear(),
            }
        }
        Ok(history)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Journal vierge dans le répertoire temporaire.
    fn store(name: &str) -> SessionStore {
        let path = std::env::temp_dir().join(format!("actor-agent-session-{name}-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        SessionStore::new(path)
    }

    /// Fichier absent : historique vide.
    #[test]
    fn missing_file_replays_empty() {
        assert!(store("absent").replay().unwrap().is_empty());
    }

    /// Messages, compaction et remise à zéro se rejouent dans l'ordre ; une ligne tronquée est ignorée.
    #[test]
    fn replays_events_in_order() {
        let s = store("rejeu");
        let msg = |t: &str| Event::Message { message: Message::user(t) };
        s.append(msg("ancien")).unwrap();
        s.append(Event::Reset).unwrap();
        s.append(msg("a")).unwrap();
        s.append(msg("b")).unwrap();
        s.append(Event::Compaction { messages: vec![Message::user("résumé")] }).unwrap();
        s.append(msg("c")).unwrap();
        OpenOptions::new().append(true).open(&s.path).unwrap().write_all(b"\n{\"ts\":1,\"type\":\"mes").unwrap();

        assert_eq!(s.replay().unwrap(), vec![Message::user("résumé"), Message::user("c")]);
        let first = std::fs::read_to_string(&s.path).unwrap();
        assert!(first.lines().next().unwrap().contains("\"ts\":"));
    }
}
