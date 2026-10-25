//! Briques du harness d'agent (d'après « Build a minimal agent harness ») :
//! en mode natif, la boucle est celle de rig (`max_turns`) ; en mode texte, `text_loop`.

pub mod compact;
pub mod guard;
pub mod probe;
pub mod prompt;
pub mod session;
pub mod text_loop;
