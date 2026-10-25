//! Rendu de l'interface : historique défilant, zone de saisie et barre de statut.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::app::{App, Role};

/// Hauteur maximale du texte de la zone de saisie, en lignes (au-delà, elle défile).
const MAX_INPUT_LINES: usize = 8;

impl App {
    /// Dessine l'historique, la saisie (hauteur ajustée au nombre de lignes) et le statut.
    pub(super) fn draw(&mut self, f: &mut Frame) {
        let input_lines = self.input.line_count().min(MAX_INPUT_LINES);
        let [hist_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(input_lines as u16 + 2),
            Constraint::Length(1),
        ])
        .areas(f.area());

        let mut lines = Vec::new();
        for (role, text) in &self.history {
            let (label, color) = match role {
                Role::User => ("Vous", Color::Cyan),
                Role::Agent => ("Agent", Color::Green),
                Role::Error => ("Erreur", Color::Red),
                Role::System => ("Système", Color::DarkGray),
            };
            lines.push(Line::styled(label, Style::new().fg(color).bold()));
            lines.extend(text.lines().map(Line::raw));
            lines.push(Line::default());
        }
        let history = Paragraph::new(lines)
            .block(Block::bordered().title(" Historique "))
            .wrap(Wrap { trim: false });
        // Défilement ancré en bas : on borne le décalage puis on le convertit depuis le haut.
        let max_top = history
            .line_count(hist_area.width)
            .saturating_sub(hist_area.height as usize);
        self.scroll_up = self.scroll_up.min(max_top);
        let top = u16::try_from(max_top - self.scroll_up).unwrap_or(u16::MAX);
        f.render_widget(history.scroll((top, 0)), hist_area);

        // Sans retour à la ligne automatique : la saisie défile pour garder le curseur visible.
        let (line, col) = self.input.position();
        let top = line.saturating_sub(input_lines - 1);
        let left = col.saturating_sub(usize::from(input_area.width.saturating_sub(3)));
        let title = " Entrée : envoyer · Alt+Entrée / Ctrl+J : nouvelle ligne · /copy · /clear · Échap : quitter ";
        let input = Paragraph::new(self.input.text())
            .block(Block::bordered().title(title))
            .scroll((u16::try_from(top).unwrap_or(u16::MAX), u16::try_from(left).unwrap_or(u16::MAX)));
        f.render_widget(input, input_area);
        // ponytail: curseur placé au nombre de caractères, faux pour les glyphes larges (CJK, emoji).
        let x = u16::try_from(col - left).unwrap_or(u16::MAX);
        let y = u16::try_from(line - top).unwrap_or(u16::MAX);
        f.set_cursor_position((input_area.x.saturating_add(1).saturating_add(x), input_area.y + 1 + y));

        let status = if self.pending.is_some() {
            Line::styled(" ⚠ Autoriser cet appel d'outil ? o = oui, autre touche = non", Style::new().fg(Color::Magenta).bold())
        } else if self.busy {
            Line::styled(" ⏳ Génération en cours...", Style::new().fg(Color::Yellow))
        } else {
            Line::styled(" ✔ En attente...", Style::new().fg(Color::Green))
        };
        f.render_widget(status, status_area);
    }
}
