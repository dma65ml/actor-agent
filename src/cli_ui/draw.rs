//! Rendu de l'interface : historique défilant, zone de saisie et barre de statut.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::app::{App, Role};

impl App {
    /// Dessine l'historique, la saisie et le statut.
    pub(super) fn draw(&mut self, f: &mut Frame) {
        let [hist_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(3),
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

        let input =
            Paragraph::new(self.input.as_str()).block(Block::bordered().title(
                " Message — Entrée : envoyer · /clear · /model · /preamble · Échap : quitter ",
            ));
        f.render_widget(input, input_area);
        // ponytail: curseur placé au nombre de caractères, faux pour les glyphes larges (CJK, emoji).
        let col = u16::try_from(self.input.chars().count()).unwrap_or(u16::MAX);
        f.set_cursor_position((
            input_area.x.saturating_add(1).saturating_add(col),
            input_area.y + 1,
        ));

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
