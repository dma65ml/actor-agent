//! Outils de test partagés : faux serveur Ollama et configuration pointant dessus.
//!
//! Le faux serveur imite l'API OpenAI-compatible d'Ollama (`POST /v1/chat/completions`) :
//! - message contenant `ERREUR` → réponse 500 (serveur en échec) ;
//! - message contenant `LENT` → répond après 3 s (pour provoquer un dépassement de délai) ;
//! - message `MODELE` / `PREAMBULE` → renvoie le modèle / la consigne système reçus ;
//! - message `OUTIL <commande>` → demande l'outil `run_bash` (appel natif, ou balise si la requête
//!   ne déclare pas d'outils), puis répond `outil : <résultat>` ;
//! - essai de détection du mode (outil `save_text`) → renvoie le texte intact, sauf pour le
//!   modèle `tronque` qui le coupe au premier guillemet, comme llama3.2 ;
//! - conversation commencée par `BOUCLE` → redemande sans fin le même appel d'outil (natif ou balise) ;
//! - modèle `lent` → répond après 10 s, quel que soit le message ;
//! - sinon → `"<n> messages : <dernier message>"`, `n` révélant la taille de l'historique.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use actix::{Actor, Addr};
use actix_web::{App, HttpResponse, HttpServer, web};
use serde_json::{Value, json};

use crate::agent_actor::AgentActor;
use tokio::sync::mpsc::unbounded_channel;

use crate::config::AppConfig;
use crate::harness::probe::ToolMode;

/// Extrait le texte d'un message OpenAI (chaîne simple ou liste de parties `text`).
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        parts => parts.as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect(),
    }
}

/// Message d'assistant demandant l'outil `name` avec `args`.
fn tool_call(id: usize, name: &str, args: Value) -> Value {
    json!({ "role": "assistant", "content": null, "tool_calls": [{
        "id": format!("appel-{id}"), "type": "function",
        "function": { "name": name, "arguments": args.to_string() }
    }]})
}

/// Message d'assistant demandant `run_bash` avec `command`.
fn bash_call(id: usize, command: &str) -> Value {
    tool_call(id, "run_bash", json!({ "command": command }))
}

/// Répond comme Ollama selon les messages reçus (voir l'en-tête du module).
async fn chat_completions(body: web::Json<Value>) -> HttpResponse {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last = messages.last().map(|m| text_of(&m["content"])).unwrap_or_default();
    let last_role = messages.last().map(|m| m["role"].clone()).unwrap_or_default();
    let has_tools = body["tools"].as_array().is_some_and(|t| !t.is_empty());
    let looping = messages.iter().any(|m| m["role"] == "user" && text_of(&m["content"]) == "BOUCLE");
    if last.contains("ERREUR") {
        return HttpResponse::InternalServerError().body("panne simulée");
    }
    if last.contains("LENT") {
        actix_web::rt::time::sleep(Duration::from_secs(3)).await;
    }
    if body["model"] == "lent" {
        actix_web::rt::time::sleep(Duration::from_secs(10)).await;
    }
    let message = if looping && has_tools {
        bash_call(messages.len(), "echo boucle")
    } else if looping {
        json!({ "role": "assistant", "content": "<run_bash>echo boucle</run_bash>" })
    } else if last_role == "tool" || last.starts_with("<result") {
        json!({ "role": "assistant", "content": format!("outil : {last}") })
    } else if let Some((_, sample)) = last.split_once("sans rien changer :\n") {
        let sample = if body["model"] == "tronque" { sample.split('"').next().unwrap() } else { sample };
        tool_call(messages.len(), "save_text", json!({ "text": sample }))
    } else if let Some(command) = last.strip_prefix("OUTIL ") {
        if has_tools {
            bash_call(messages.len(), command)
        } else {
            json!({ "role": "assistant", "content": format!("<run_bash>{command}</run_bash>") })
        }
    } else {
        let reply = match last.as_str() {
            "MODELE" => body["model"].as_str().unwrap_or_default().to_owned(),
            "PREAMBULE" => messages.first().map(|m| text_of(&m["content"])).unwrap_or_default(),
            _ => format!("{} messages : {last}", messages.len()),
        };
        json!({ "role": "assistant", "content": reply })
    };
    let finish_reason = if message["tool_calls"].is_null() { "stop" } else { "tool_calls" };
    HttpResponse::Ok().json(json!({
        "id": "test",
        "object": "chat.completion",
        "created": 0,
        "model": body["model"],
        "choices": [{ "index": 0, "message": message, "finish_reason": finish_reason }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    }))
}

/// Démarre le faux serveur sur un port libre et renvoie son URL de base.
pub fn fake_ollama() -> String {
    let server = HttpServer::new(|| App::new().route("/v1/chat/completions", web::post().to(chat_completions)))
        .workers(1)
        .bind("127.0.0.1:0")
        .expect("liaison du faux serveur");
    let url = format!("http://{}", server.addrs()[0]);
    actix_web::rt::spawn(server.run());
    url
}

/// Configuration de test : faux serveur Ollama, délai d'une seconde, journal de session et
/// répertoire de travail propres au test, appel d'outils natif (sans détection).
pub fn test_config() -> AppConfig {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let session = std::env::temp_dir().join(format!("actor-agent-test-{}-{n}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&session);
    AppConfig {
        ollama_url: fake_ollama(),
        answer_timeout_secs: 1,
        session_file: session.to_string_lossy().into_owned(),
        history_file: format!("{}.historique", session.to_string_lossy()),
        workspace: format!("{}.ws", session.to_string_lossy()),
        tool_mode: ToolMode::Native,
        ..AppConfig::default()
    }
}

/// Démarre un `AgentActor` branché sur le faux serveur et renvoie sa configuration.
/// Personne ne répond aux demandes d'autorisation : elles sont refusées.
pub fn start_agent() -> (Addr<AgentActor>, AppConfig) {
    let cfg = test_config();
    let agent = AgentActor::new(&cfg, unbounded_channel().0).expect("journal de test lisible");
    (agent.start(), cfg)
}
