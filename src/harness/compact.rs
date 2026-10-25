//! Compaction du contexte : au-delà de 80 % de la fenêtre du modèle, les anciens messages
//! sont remplacés par un résumé et seuls les `KEEP_RECENT` derniers sont gardés tels quels.

use rig_agent::completion::Message;
use rig_core::completion::message::UserContent;

/// Nombre minimal de messages récents conservés tels quels.
const KEEP_RECENT: usize = 6;
/// Seuil de déclenchement, en pourcentage de la fenêtre de contexte.
const THRESHOLD_PERCENT: usize = 80;

/// Estimation grossière du nombre de jetons : environ 4 caractères par jeton.
pub fn estimate_tokens(history: &[Message]) -> usize {
    serde_json::to_string(history).map_or(0, |s| s.len() / 4)
}

/// Index du premier message à garder tel quel, ou `None` si la compaction est inutile ou impossible.
/// La coupure tombe sur un message utilisateur textuel : un appel d'outil n'est jamais séparé de son résultat.
pub fn split_point(history: &[Message], context_tokens: usize) -> Option<usize> {
    if estimate_tokens(history) * 100 <= context_tokens * THRESHOLD_PERCENT {
        return None;
    }
    let last = history.len().checked_sub(KEEP_RECENT)?;
    (1..=last).rev().find(|&i| is_user_text(&history[i]))
}

/// Vrai pour un message utilisateur qui ne contient que du texte (pas de résultat d'outil).
fn is_user_text(message: &Message) -> bool {
    matches!(message, Message::User { content } if content.iter().all(|c| matches!(c, UserContent::Text(_))))
}

/// Consigne envoyée au modèle pour résumer les anciens messages.
pub fn summary_request(old: &[Message]) -> String {
    let transcript = serde_json::to_string_pretty(old).unwrap_or_default();
    format!(
        "Résume cette conversation pour pouvoir la poursuivre : objectif, décisions, fichiers \
         touchés, travail en cours et conventions à respecter.\n\n{transcript}"
    )
}

/// Nouvel historique : le résumé, puis les messages récents inchangés.
pub fn compacted(summary: &str, recent: &[Message]) -> Vec<Message> {
    let mut history = vec![Message::user(format!("Résumé de la conversation précédente :\n{summary}"))];
    history.extend_from_slice(recent);
    history
}

#[cfg(test)]
mod tests {
    use rig_core::completion::message::{CallId, ToolName, ToolResultContent};

    use super::*;

    /// Historique alterné utilisateur / assistant de `n` tours.
    fn turns(n: usize) -> Vec<Message> {
        (0..n).flat_map(|i| [Message::user(format!("q{i}")), Message::assistant(format!("r{i}"))]).collect()
    }

    /// Sous le seuil, ou trop peu de messages : pas de compaction.
    #[test]
    fn no_split_below_threshold_or_when_too_short() {
        assert_eq!(split_point(&turns(10), 1_000_000), None);
        assert_eq!(split_point(&turns(3), 1), None);
    }

    /// Au-delà du seuil, on garde au moins `KEEP_RECENT` messages à partir d'un message utilisateur.
    #[test]
    fn splits_on_user_text_message() {
        let history = turns(5);
        assert_eq!(split_point(&history, 1), Some(4));
        let compacted = compacted("résumé", &history[4..]);
        assert_eq!(compacted.len(), 7);
        assert!(is_user_text(&compacted[1]));
    }

    /// Un résultat d'outil n'est jamais choisi comme point de coupure.
    #[test]
    fn never_splits_on_tool_result() {
        let mut history = turns(5);
        let name = ToolName::new("run_bash").unwrap();
        let result = UserContent::tool_result(CallId::from_wire("id"), name, vec![ToolResultContent::text("ok")]);
        history[4] = Message::User { content: vec![result] };
        assert_eq!(split_point(&history, 1), Some(2));
        assert!(summary_request(&history[..2]).contains("q0"));
    }
}
