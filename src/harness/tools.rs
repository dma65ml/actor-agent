//! Outils de base d'un agent de développement (`read_file`, `write_file`, `run_bash`)
//! et niveau de permission requis par chaque appel.

use std::path::{Component, Path};

use rig_core::tool::PortableTool;
use serde::Deserialize;
use serde_json::{Value, json};

/// Niveau de permission, du moins au plus risqué (l'ordre sert aux comparaisons).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Lecture seule.
    Read,
    /// Modification du répertoire de travail.
    Workspace,
    /// Tout le reste : destructif, hors du répertoire de travail ou non analysable.
    Full,
}

/// Commandes shell en lecture seule (classées d'après leur premier mot).
/// `find` n'y figure pas : `-delete` / `-exec` le rendent destructif.
const READ_ONLY: [&str; 7] = ["ls", "cat", "grep", "echo", "pwd", "head", "tail"];
/// Commandes shell destructives.
const DESTRUCTIVE: [&str; 6] = ["rm", "sudo", "shutdown", "reboot", "mkfs", "dd"];
/// Caractères de composition shell : leur présence rend la commande non analysable par son premier mot.
const SHELL_META: &[char] = &[';', '|', '&', '$', '`', '<', '>', '(', ')', '{', '}', '\n'];
/// Taille maximale d'une sortie d'outil renvoyée au modèle, en caractères.
const MAX_OUTPUT: usize = 20_000;

/// Permission requise par un appel d'outil (nom et arguments JSON bruts), vérifiée avant exécution.
pub fn permission(tool: &str, args: &str) -> Permission {
    match tool {
        ReadFile::NAME => Permission::Read,
        WriteFile::NAME => serde_json::from_str::<PathArgs>(args).map_or(Permission::Full, |a| path_permission(&a.path)),
        RunBash::NAME => serde_json::from_str::<BashArgs>(args).map_or(Permission::Full, |a| classify(&a.command)),
        _ => Permission::Full,
    }
}

/// Classe une commande shell d'après son premier mot ; toute composition exige `Full`.
pub fn classify(command: &str) -> Permission {
    if command.contains(SHELL_META) {
        return Permission::Full;
    }
    match command.split_whitespace().next() {
        Some(cmd) if READ_ONLY.contains(&cmd) => Permission::Read,
        Some(cmd) if DESTRUCTIVE.contains(&cmd) => Permission::Full,
        Some(_) => Permission::Workspace,
        None => Permission::Read,
    }
}

/// Écrire dans le répertoire de travail relève de `Workspace` ; ailleurs (chemin absolu, `..`), de `Full`.
fn path_permission(path: &str) -> Permission {
    let inside = Path::new(path).components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if inside { Permission::Workspace } else { Permission::Full }
}

/// Tronque une sortie trop longue pour préserver le contexte du modèle.
fn truncate(text: String) -> String {
    match text.char_indices().nth(MAX_OUTPUT) {
        Some((cut, _)) => format!("{}\n[… sortie tronquée]", &text[..cut]),
        None => text,
    }
}

/// Arguments d'un outil travaillant sur un fichier.
#[derive(Deserialize)]
pub struct PathArgs {
    path: String,
}

/// Arguments de `write_file`.
#[derive(Deserialize)]
pub struct WriteArgs {
    path: String,
    content: String,
}

/// Arguments de `run_bash`.
#[derive(Deserialize)]
pub struct BashArgs {
    command: String,
}

/// Lit un fichier texte.
pub struct ReadFile;

impl PortableTool for ReadFile {
    const NAME: &'static str = "read_file";
    type Args = PathArgs;
    type Output = String;
    type Error = std::io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Lit un fichier texte et renvoie son contenu.".into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string", "description": "chemin du fichier"}}, "required": ["path"]})
    }

    /// Renvoie le contenu du fichier (tronqué s'il est trop long).
    async fn call(&self, args: PathArgs) -> std::io::Result<String> {
        Ok(truncate(tokio::fs::read_to_string(args.path).await?))
    }
}

/// Crée ou remplace un fichier texte.
pub struct WriteFile;

impl PortableTool for WriteFile {
    const NAME: &'static str = "write_file";
    type Args = WriteArgs;
    type Output = String;
    type Error = std::io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Crée ou remplace un fichier texte (répertoires parents créés au besoin).".into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "chemin du fichier"},
            "content": {"type": "string", "description": "contenu complet du fichier"}
        }, "required": ["path", "content"]})
    }

    /// Écrit le fichier et confirme le nombre d'octets écrits.
    async fn call(&self, args: WriteArgs) -> std::io::Result<String> {
        if let Some(parent) = Path::new(&args.path).parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&args.path, &args.content).await?;
        Ok(format!("{} octets écrits dans {}", args.content.len(), args.path))
    }
}

/// Exécute une commande shell.
pub struct RunBash;

impl PortableTool for RunBash {
    const NAME: &'static str = "run_bash";
    type Args = BashArgs;
    type Output = String;
    type Error = std::io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Exécute une commande avec `sh -c` dans le répertoire courant et renvoie son code et sa sortie.".into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string", "description": "commande shell"}}, "required": ["command"]})
    }

    /// Lance la commande ; elle est tuée si l'exécution de l'agent est abandonnée (délai dépassé).
    async fn call(&self, args: BashArgs) -> std::io::Result<String> {
        let out = tokio::process::Command::new("sh").arg("-c").arg(&args.command).kill_on_drop(true).output().await?;
        let code = out.status.code().map_or("aucun (signal)".into(), |c| c.to_string());
        let text = format!(
            "code de sortie : {code}\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(truncate(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Classement des commandes shell, composition comprise.
    #[test]
    fn classifies_commands() {
        assert_eq!(classify("ls -la"), Permission::Read);
        assert_eq!(classify("  "), Permission::Read);
        assert_eq!(classify("cargo test"), Permission::Workspace);
        assert_eq!(classify("find . -delete"), Permission::Workspace);
        assert_eq!(classify("rm -rf cible"), Permission::Full);
        assert_eq!(classify("ls; rm -rf /"), Permission::Full);
        assert_eq!(classify("cat a > b"), Permission::Full);
        assert_eq!(classify("echo $(rm x)"), Permission::Full);
    }

    /// Permission par outil ; arguments illisibles ou outil inconnu : `Full`.
    #[test]
    fn permission_per_tool() {
        assert_eq!(permission("read_file", r#"{"path": "/etc/hosts"}"#), Permission::Read);
        assert_eq!(permission("write_file", r#"{"path": "src/a.rs", "content": ""}"#), Permission::Workspace);
        assert_eq!(permission("write_file", r#"{"path": "../a.rs", "content": ""}"#), Permission::Full);
        assert_eq!(permission("write_file", r#"{"path": "/tmp/a", "content": ""}"#), Permission::Full);
        assert_eq!(permission("run_bash", r#"{"command": "pwd"}"#), Permission::Read);
        assert_eq!(permission("run_bash", "pas du json"), Permission::Full);
        assert_eq!(permission("inconnu", "{}"), Permission::Full);
    }

    /// Les outils fonctionnent de bout en bout et respectent leur schéma (les champs requis suffisent).
    #[tokio::test]
    async fn tools_write_read_and_run() {
        let dir = std::env::temp_dir().join(format!("actor-agent-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sous/fichier.txt").to_string_lossy().into_owned();

        let args = json!({"path": path, "content": "bonjour"});
        assert_eq!(WriteFile.call(serde_json::from_value(args).unwrap()).await.unwrap(), format!("7 octets écrits dans {path}"));
        let read = ReadFile.call(serde_json::from_value(json!({"path": path})).unwrap()).await.unwrap();
        assert_eq!(read, "bonjour");
        let run = RunBash.call(serde_json::from_value(json!({"command": "echo ok; exit 3"})).unwrap()).await.unwrap();
        assert_eq!(run, "code de sortie : 3\nok\n");
        assert!(ReadFile.call(serde_json::from_value(json!({"path": dir.join("absent")})).unwrap()).await.is_err());
    }

    /// Une sortie trop longue est tronquée et signalée.
    #[test]
    fn long_output_is_truncated() {
        let text = truncate("é".repeat(MAX_OUTPUT + 1));
        assert!(text.ends_with("[… sortie tronquée]"));
        let kept = text.split('\n').next().unwrap();
        assert_eq!(kept.chars().count(), MAX_OUTPUT);
    }
}
