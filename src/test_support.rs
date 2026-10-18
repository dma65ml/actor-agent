//! Outils de test partagés : faux serveur Ollama et configuration pointant dessus.
//!
//! Le faux serveur imite l'API OpenAI-compatible d'Ollama (`POST /v1/chat/completions`) :
//! - message contenant `ERREUR` → réponse 500 (serveur en échec) ;
//! - message contenant `LENT` → répond après 3 s (pour provoquer un dépassement de délai) ;
//! - message `MODELE` / `PREAMBULE` → renvoie le modèle / la consigne système reçus ;
//! - sinon → `"<n> messages : <dernier message>"`, `n` révélant la taille de l'historique.

use std::time::Duration;

use actix::{Actor, Addr};
use actix_web::{App, HttpResponse, HttpServer, web};
use serde_json::{Value, json};

use crate::config::{AppConfig, ConfigActor};
use crate::proxy::ProxyAgent;

/// Extrait le texte d'un message OpenAI (chaîne simple ou liste de parties `text`).
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        parts => parts.as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect(),
    }
}

/// Répond comme Ollama selon le dernier message reçu (voir l'en-tête du module).
async fn chat_completions(body: web::Json<Value>) -> HttpResponse {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last = messages.last().map(|m| text_of(&m["content"])).unwrap_or_default();
    if last.contains("ERREUR") {
        return HttpResponse::InternalServerError().body("panne simulée");
    }
    if last.contains("LENT") {
        actix_web::rt::time::sleep(Duration::from_secs(3)).await;
    }
    let reply = match last.as_str() {
        "MODELE" => body["model"].as_str().unwrap_or_default().to_owned(),
        "PREAMBULE" => messages.first().map(|m| text_of(&m["content"])).unwrap_or_default(),
        _ => format!("{} messages : {last}", messages.len()),
    };
    HttpResponse::Ok().json(json!({
        "id": "test",
        "object": "chat.completion",
        "created": 0,
        "model": body["model"],
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": reply },
            "finish_reason": "stop"
        }],
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

/// Configuration de test : faux serveur Ollama et délai de réponse d'une seconde.
pub fn test_config() -> AppConfig {
    AppConfig { ollama_url: fake_ollama(), answer_timeout_secs: 1, ..AppConfig::default() }
}

/// Démarre `ConfigActor` et `ProxyAgent` branchés sur le faux serveur.
pub fn start_actors() -> (Addr<ProxyAgent>, Addr<ConfigActor>) {
    let config = ConfigActor::new(test_config()).start();
    (ProxyAgent::new(config.clone()).start(), config)
}
