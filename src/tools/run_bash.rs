//! Outil `run_bash` : exécute une commande shell à la racine du répertoire de travail,
//! et classe chaque commande par niveau de permission.

use rig_core::tool::PortableTool;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Permission, Workspace, truncate};

/// Commandes en lecture seule (classées d'après leur premier mot).
/// `find` n'y figure pas : `-delete` / `-exec` le rendent destructif.
const READ_ONLY: [&str; 7] = ["ls", "cat", "grep", "echo", "pwd", "head", "tail"];
/// Commandes destructives.
const DESTRUCTIVE: [&str; 6] = ["rm", "sudo", "shutdown", "reboot", "mkfs", "dd"];
/// Caractères de composition shell : leur présence rend la commande non analysable par son premier mot.
const SHELL_META: &[char] = &[';', '|', '&', '$', '`', '<', '>', '(', ')', '{', '}', '\n'];

/// Arguments de `run_bash`.
#[derive(Deserialize)]
pub struct BashArgs {
    command: String,
}

/// Permission requise par des arguments JSON bruts ; illisibles, `Full`.
pub(super) fn permission(args: &str) -> Permission {
    serde_json::from_str::<BashArgs>(args).map_or(Permission::Full, |a| classify(&a.command))
}

/// Classe une commande d'après son premier mot. Toute composition, ou tout argument
/// pouvant viser l'extérieur du répertoire de travail (chemin absolu, `~`, `..`), exige `Full`.
// ponytail: filtre lexical, contournable (ex. `cd` dans un script) ; un vrai bac à sable
// (sandbox-exec, bwrap, conteneur) est la seule garantie.
fn classify(command: &str) -> Permission {
    let escapes = |w: &str| w.starts_with(['/', '~']) || w.contains("..");
    if command.contains(SHELL_META) || command.split_whitespace().any(escapes) {
        return Permission::Full;
    }
    match command.split_whitespace().next() {
        Some(cmd) if READ_ONLY.contains(&cmd) => Permission::Read,
        Some(cmd) if DESTRUCTIVE.contains(&cmd) => Permission::Full,
        Some(_) => Permission::Workspace,
        None => Permission::Read,
    }
}

/// Exécute une commande shell.
pub struct RunBash(pub Workspace);

impl PortableTool for RunBash {
    const NAME: &'static str = "run_bash";
    type Args = BashArgs;
    type Output = String;
    type Error = std::io::Error;

    /// Description montrée au modèle.
    fn description(&self) -> String {
        "Exécute une commande avec `sh -c` à la racine du répertoire de travail et renvoie son code et sa sortie.".into()
    }

    /// Schéma JSON des arguments.
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string", "description": "commande shell"}}, "required": ["command"]})
    }

    /// Lance la commande ; elle est tuée si l'exécution de l'agent est abandonnée (délai dépassé).
    async fn call(&self, args: BashArgs) -> std::io::Result<String> {
        let out = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&args.command)
            .current_dir(self.0.root())
            .kill_on_drop(true)
            .output()
            .await?;
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
    use crate::tools::temp_workspace;

    /// Classement des commandes : lecture, écriture locale, destruction, composition, sortie du répertoire.
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
        assert_eq!(classify("cat /etc/passwd"), Permission::Full);
        assert_eq!(classify("ls ~"), Permission::Full);
        assert_eq!(classify("cat ../secret"), Permission::Full);
    }

    /// La commande s'exécute à la racine du répertoire de travail et renvoie code et sorties.
    #[tokio::test]
    async fn runs_in_workspace_root() {
        let ws = temp_workspace("bash");
        let root = ws.root().display().to_string();
        let tool = RunBash(ws);
        let run = |command: &str| tool.call(serde_json::from_value(json!({"command": command})).unwrap());
        assert_eq!(run("pwd").await.unwrap(), format!("code de sortie : 0\n{root}\n"));
        assert_eq!(run("echo ok; exit 3").await.unwrap(), "code de sortie : 3\nok\n");
    }
}
