//! Bac à sable de `run_bash` : la commande ne peut écrire que dans le répertoire de travail
//! (et les répertoires temporaires) et, en mode `strict`, n'a pas accès au réseau.
//! macOS : `sandbox-exec` ; Linux : `bwrap` (bubblewrap).
//! La lecture n'est pas restreinte (les chaînes d'outils vivent souvent dans le dossier
//! personnel) : le modèle étant local, une lecture ne peut sortir de la machine que par le réseau.

use std::path::Path;

use serde::Deserialize;
use tokio::process::Command;

/// Niveau d'isolation des commandes shell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sandbox {
    /// Écriture limitée au répertoire de travail, réseau coupé.
    #[default]
    Strict,
    /// Écriture limitée au répertoire de travail, réseau autorisé (installation de paquets…).
    Network,
    /// Aucune isolation : la commande tourne avec tous les droits de l'utilisateur.
    Off,
}

/// Commande `sh -c command` à la racine `root`, isolée selon `mode` ; tuée si elle est abandonnée.
pub fn command(mode: Sandbox, root: &Path, command: &str) -> Command {
    let mut cmd = match mode {
        Sandbox::Off => Command::new("sh"),
        _ if cfg!(target_os = "macos") => {
            let mut cmd = Command::new("sandbox-exec");
            cmd.arg("-p").arg(macos_profile(mode, root)).arg("sh");
            cmd
        }
        // ponytail: variante Linux non testée ici (développement sous macOS).
        _ => {
            let mut cmd = Command::new("bwrap");
            cmd.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--die-with-parent"]);
            cmd.arg("--bind").arg(root).arg(root);
            if mode == Sandbox::Strict {
                cmd.arg("--unshare-net");
            }
            cmd.arg("sh");
            cmd
        }
    };
    cmd.arg("-c").arg(command).current_dir(root).kill_on_drop(true);
    cmd
}

/// Profil `sandbox-exec` : tout est permis sauf l'écriture hors du répertoire de travail,
/// des périphériques et des répertoires temporaires, et le réseau en mode `strict`.
fn macos_profile(mode: Sandbox, root: &Path) -> String {
    let root = root.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
    let network = if mode == Sandbox::Strict { "(deny network*)" } else { "" };
    format!(
        "(version 1)(allow default){network}(deny file-write*)(allow file-write* (subpath \"{root}\") \
         (subpath \"/dev\") (subpath \"/private/tmp\") (subpath \"/private/var/folders\"))"
    )
}
