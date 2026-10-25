//! Outils de test partagés : faux serveur Ollama et configuration pointant dessus.
//!
//! Le faux serveur imite l'API OpenAI-compatible d'Ollama (`POST /v1/chat/completions`) :
//! - message contenant `ERREUR` → réponse 500 (serveur en échec) ;
//! - message contenant `LENT` → répond après 3 s (pour provoquer un dépassement de délai) ;
//! - message `MODELE` / `PREAMBULE` → renvoie le modèle / la consigne système reçus ;
//! - message `OUTIL <commande>` → demande l'outil `run_bash`, puis répond `outil : <résultat>` ;
//! - message `BOUCLE` → redemande sans fin le même appel d'outil ;
//! - sinon → `"<n> messages : <dernier message>"`, `n` révélant la taille de l'historique.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use actix::{Actor, Addr};
use actix_web::{App, HttpResponse, HttpServer, web};
use serde_json::{Value, json};

use crate::agent_actor::AgentActor;
use tokio::sync::mpsc::unbounded_channel;

use crate::config::AppConfig;

/// Extrait le texte d'un message OpenAI (chaîne simple ou liste de parties `text`).
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        parts => parts.as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect(),
    }
}

/// Message d'assistant demandant `run_bash` avec `command`.
fn bash_call(id: usize, command: &str) -> Value {
    let arguments = json!({ "command": command }).to_string();
    json!({ "role": "assistant", "content": null, "tool_calls": [{
        "id": format!("appel-{id}"), "type": "function",
        "function": { "name": "run_bash", "arguments": arguments }
    }]})
}

/// Répond comme Ollama selon les messages reçus (voir l'en-tête du module).
async fn chat_completions(body: web::Json<Value>) -> HttpResponse {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last = messages.last().map(|m| text_of(&m["content"])).unwrap_or_default();
    let last_role = messages.last().map(|m| m["role"].clone()).unwrap_or_default();
    let last_user = messages.iter().rev().find(|m| m["role"] == "user").map(|m| text_of(&m["content"]));
    if last.contains("ERREUR") {
        return HttpResponse::InternalServerError().body("panne simulée");
    }
    if last.contains("LENT") {
        actix_web::rt::time::sleep(Duration::from_secs(3)).await;
    }
    let message = if last_user.as_deref() == Some("BOUCLE") {
        bash_call(messages.len(), "echo boucle")
    } else if last_role == "tool" {
        json!({ "role": "assistant", "content": format!("outil : {last}") })
    } else if let Some(command) = last.strip_prefix("OUTIL ") {
        bash_call(messages.len(), command)
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

/// Configuration de test : faux serveur Ollama, délai d'une seconde et journal de session propre au test.
pub fn test_config() -> AppConfig {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let session = std::env::temp_dir().join(format!("actor-agent-test-{}-{n}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&session);
    AppConfig {
        ollama_url: fake_ollama(),
        answer_timeout_secs: 1,
        session_file: session.to_string_lossy().into_owned(),
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
