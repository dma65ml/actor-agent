//! Test d'intégration : lance le vrai binaire pour vérifier la sortie sur configuration invalide.

use std::path::PathBuf;
use std::process::Command;

/// Commande du binaire exécutée dans un répertoire temporaire vierge (pour `config.toml`).
fn binary(name: &str) -> (Command, PathBuf) {
    let dir = std::env::temp_dir().join(format!("actor-agent-bin-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_actor-agent"));
    cmd.current_dir(&dir).env_remove("OLLAMA_MODEL");
    (cmd, dir)
}

/// Une configuration invalide arrête le programme avec un message lisible.
#[test]
fn invalid_config_exits_with_message() {
    let (mut cmd, dir) = binary("badconfig");
    std::fs::write(dir.join("config.toml"), "model = 42").unwrap();
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fichier de configuration invalide"));
}
