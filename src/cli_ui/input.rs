//! Zone de saisie multiligne : texte et curseur, avec les déplacements et éditions d'un éditeur simple.

/// Texte saisi et position du curseur (index d'octet, toujours sur une frontière de caractère).
#[derive(Default)]
pub(super) struct Input {
    text: String,
    cursor: usize,
}

impl Input {
    /// Texte saisi.
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// Remplace le texte (rappel d'historique) ; le curseur passe à la fin.
    pub(super) fn set(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
    }

    /// Vide la saisie et renvoie son contenu.
    pub(super) fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Insère du texte au curseur ; les fins de ligne Windows ou Mac (collage) deviennent `\n`.
    pub(super) fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    /// Supprime le caractère avant le curseur.
    pub(super) fn backspace(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
            self.text.remove(self.cursor);
        }
    }

    /// Supprime le caractère sous le curseur.
    pub(super) fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    /// Curseur d'un caractère vers la gauche.
    pub(super) fn left(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    /// Curseur d'un caractère vers la droite.
    pub(super) fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// Curseur au début de la ligne courante.
    pub(super) fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    /// Curseur à la fin de la ligne courante.
    pub(super) fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    /// Ligne précédente, même colonne si possible ; `false` sur la première ligne.
    pub(super) fn up(&mut self) -> bool {
        let start = self.line_start(self.cursor);
        if start == 0 {
            return false;
        }
        let col = self.position().1;
        self.cursor = self.at_column(self.line_start(start - 1), col);
        true
    }

    /// Ligne suivante, même colonne si possible ; `false` sur la dernière ligne.
    pub(super) fn down(&mut self) -> bool {
        let end = self.line_end(self.cursor);
        if end == self.text.len() {
            return false;
        }
        let col = self.position().1;
        self.cursor = self.at_column(end + 1, col);
        true
    }

    /// Ligne et colonne du curseur, en caractères (pour l'affichage).
    pub(super) fn position(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.matches('\n').count();
        (line, before[self.line_start(self.cursor)..].chars().count())
    }

    /// Nombre de lignes du texte.
    pub(super) fn line_count(&self) -> usize {
        self.text.matches('\n').count() + 1
    }

    /// Début de la ligne contenant `pos`.
    fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    /// Fin de la ligne contenant `pos` (avant le `\n`).
    fn line_end(&self, pos: usize) -> usize {
        self.text[pos..].find('\n').map_or(self.text.len(), |i| pos + i)
    }

    /// Position de la colonne `col` dans la ligne qui commence à `start`, bornée à sa fin.
    fn at_column(&self, start: usize, col: usize) -> usize {
        let end = self.line_end(start);
        self.text[start..end].char_indices().nth(col).map_or(end, |(i, _)| start + i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Saisie `text` avec le curseur au repère `|`.
    fn at(text: &str) -> Input {
        let cursor = text.find('|').unwrap();
        Input { text: text.replace('|', ""), cursor }
    }

    /// Rend le texte avec le curseur marqué `|`.
    fn show(input: &Input) -> String {
        format!("{}|{}", &input.text[..input.cursor], &input.text[input.cursor..])
    }

    /// Insertion, effacements et déplacements horizontaux au milieu du texte, accents compris.
    #[test]
    fn edits_at_cursor() {
        let mut input = at("éa|c");
        input.insert("b");
        assert_eq!(show(&input), "éab|c");
        input.left();
        input.left();
        input.left();
        input.left();
        input.backspace();
        assert_eq!(show(&input), "|éabc");
        input.delete();
        input.right();
        input.end();
        assert_eq!(show(&input), "abc|");
        input.home();
        input.insert("x\r\ny");
        assert_eq!(show(&input), "x\ny|abc");
        assert_eq!((input.line_count(), input.position()), (2, (1, 1)));
    }

    /// ↑ / ↓ changent de ligne en gardant la colonne (bornée à la longueur de la ligne).
    #[test]
    fn moves_between_lines() {
        let mut input = at("un\ndeux|\ntrois");
        assert!(input.up());
        assert_eq!(show(&input), "un|\ndeux\ntrois");
        assert!(!input.up());
        assert!(input.down() && input.down());
        assert_eq!(show(&input), "un\ndeux\ntr|ois");
        assert!(!input.down());
        assert_eq!(input.take(), "un\ndeux\ntrois");
        assert_eq!(show(&input), "|");
    }
}
