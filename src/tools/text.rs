//! Appel d'outils en mode texte, pour les modèles dont l'appel natif est peu fiable :
//! le modèle écrit des balises dans sa réponse, au contenu brut (rien à échapper),
//! et le harness les exécute puis lui renvoie les résultats dans des balises `<result>`.

use rig_core::tool::PortableTool;
use serde_json::{Map, Value};

use super::{EditFile, ListDir, ReadFile, RunBash, Workspace, WriteFile};

/// Outils reconnus dans le texte.
const TOOLS: [&str; 5] = [ListDir::NAME, ReadFile::NAME, WriteFile::NAME, EditFile::NAME, RunBash::NAME];

/// Mode d'emploi ajouté au prompt système.
pub const INSTRUCTIONS: &str = r#"## Outils
Pour agir, écris une ou plusieurs de ces balises dans ta réponse puis arrête-toi : les résultats te seront renvoyés dans des balises <result>. Le contenu des balises est brut : n'échappe rien.

<list_dir path="src"/>  liste un répertoire (path facultatif)
<read_file path="src/main.rs"/>  lit un fichier
<write_file path="main.py">
contenu complet du fichier
</write_file>
<edit_file path="main.py">
<old>passage exact, présent une seule fois</old>
<new>texte de remplacement</new>
</edit_file>
<run_bash>commande shell</run_bash>

Les chemins sont relatifs au répertoire de travail. Quand la tâche est terminée, réponds sans balise."#;

/// Appels d'outils trouvés dans `reply`, dans l'ordre : nom et arguments JSON.
pub fn parse(reply: &str) -> Vec<(&'static str, Value)> {
    let mut calls = Vec::new();
    let mut rest = reply;
    while let Some((name, args, after)) = next_call(rest) {
        calls.push((name, args));
        rest = after;
    }
    calls
}

/// Premier appel de `text` : nom, arguments et texte qui suit. Une balise non fermée est ignorée.
// ponytail: le contenu ne peut pas contenir la balise fermante de son propre outil.
fn next_call(text: &str) -> Option<(&'static str, Value, &str)> {
    let (start, name) = TOOLS.iter().filter_map(|&n| text.find(&format!("<{n}")).map(|i| (i, n))).min()?;
    let open = &text[start..];
    let tag_end = open.find('>')?;
    let attrs = &open[1 + name.len()..tag_end];
    let mut args = attributes(attrs.trim_end_matches('/'));
    let after_tag = &open[tag_end + 1..];
    if attrs.ends_with('/') {
        return Some((name, Value::Object(args), after_tag));
    }
    let close = format!("</{name}>");
    let body_len = after_tag.find(&close)?;
    let body = &after_tag[..body_len];
    let mut set = |key: &str, value: Option<&str>| {
        if let Some(v) = value {
            args.insert(key.into(), Value::String(v.into()));
        }
    };
    match name {
        WriteFile::NAME => set("content", Some(strip_newline(body))),
        RunBash::NAME => set("command", Some(body.trim())),
        EditFile::NAME => {
            set("old_str", inner(body, "old"));
            set("new_str", inner(body, "new"));
        }
        _ => {}
    }
    Some((name, Value::Object(args), &after_tag[body_len + close.len()..]))
}

/// Attributs `clé="valeur"` d'une balise.
fn attributes(text: &str) -> Map<String, Value> {
    let parts: Vec<&str> = text.split('"').collect();
    let (pairs, _) = parts.as_chunks::<2>();
    pairs.iter().map(|[k, v]| (k.trim().trim_end_matches('=').trim().to_owned(), Value::String((*v).to_owned()))).collect()
}

/// Contenu de la sous-balise `<tag>…</tag>` de `body`.
fn inner<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let start = body.find(&format!("<{tag}>"))? + tag.len() + 2;
    let len = body[start..].find(&format!("</{tag}>"))?;
    Some(strip_newline(&body[start..start + len]))
}

/// Retire le saut de ligne qui suit d'habitude la balise ouvrante.
fn strip_newline(text: &str) -> &str {
    text.strip_prefix("\r\n").or_else(|| text.strip_prefix('\n')).unwrap_or(text)
}

/// Exécute un appel ; une erreur (arguments invalides, échec de l'outil) devient le résultat renvoyé au modèle.
pub async fn run(ws: &Workspace, name: &str, args: Value) -> String {
    let res = match name {
        ListDir::NAME => invoke(ListDir(ws.clone()), args).await,
        ReadFile::NAME => invoke(ReadFile(ws.clone()), args).await,
        WriteFile::NAME => invoke(WriteFile(ws.clone()), args).await,
        EditFile::NAME => invoke(EditFile(ws.clone()), args).await,
        RunBash::NAME => invoke(RunBash(ws.clone()), args).await,
        _ => Err(format!("outil inconnu : {name}")),
    };
    res.unwrap_or_else(|e| format!("erreur : {e}"))
}

/// Désérialise les arguments puis appelle l'outil.
async fn invoke<T: PortableTool<Output = String>>(tool: T, args: Value) -> Result<String, String> {
    let args = serde_json::from_value(args).map_err(|e| format!("arguments invalides : {e}"))?;
    tool.call(args).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::temp_workspace;

    /// Toutes les formes de balises, guillemets compris, dans l'ordre du texte.
    #[test]
    fn parses_all_tags() {
        let reply = r#"Je commence.
<write_file path="main.py">
print("hello world")
</write_file>
<read_file path="a.txt"/> puis <list_dir/>
<edit_file path="b.rs"><old>let a = "x";</old><new>
let a = "y";</new></edit_file>
<run_bash> cargo test </run_bash>
<run_bash>non fermée"#;
        assert_eq!(
            parse(reply),
            vec![
                ("write_file", json!({"path": "main.py", "content": "print(\"hello world\")\n"})),
                ("read_file", json!({"path": "a.txt"})),
                ("list_dir", json!({})),
                ("edit_file", json!({"path": "b.rs", "old_str": "let a = \"x\";", "new_str": "let a = \"y\";"})),
                ("run_bash", json!({"command": "cargo test"})),
            ]
        );
        assert!(parse("Réponse finale, sans outil.").is_empty());
    }

    /// Exécution : succès, puis arguments invalides renvoyés comme erreur au modèle.
    #[tokio::test]
    async fn runs_calls() {
        let ws = temp_workspace("text");
        let (name, args) = parse("<write_file path=\"a.txt\">\nbonjour</write_file>").remove(0);
        assert_eq!(run(&ws, name, args).await, "7 octets écrits dans a.txt");
        assert!(run(&ws, "edit_file", json!({"path": "a.txt"})).await.starts_with("erreur : arguments invalides"));
        assert_eq!(run(&ws, "inconnu", json!({})).await, "erreur : outil inconnu : inconnu");
    }
}
