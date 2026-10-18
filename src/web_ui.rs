//! Interface Web : API HTTP JSON branchée sur le `ProxyAgent`.
//!
//! - `POST /chat` `{ "session_id": "...", "content": "..." }` → `{ "session_id": "...", "response": "..." }`
//! - `DELETE /sessions/{session_id}` → 204, efface l'historique de la session.
//! - `GET /config` → configuration courante ; `PATCH /config` `{"model": "..."}` ou `{"preamble": "..."}`.

use actix::Addr;
use actix_web::{App, HttpResponse, HttpServer, ResponseError, dev::Server, http::StatusCode, web};
use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, ConfigActor, GetConfig, UpdateConfig};
use crate::proxy::{ClearSession, ProxyAgent, ProxyError, ask};

/// Corps de la requête `POST /chat`.
#[derive(Deserialize)]
struct ChatRequest {
    session_id: String,
    content: String,
}

/// Corps de la réponse `POST /chat`.
#[derive(Serialize)]
struct ChatResponse {
    session_id: String,
    response: String,
}

impl ResponseError for ProxyError {
    /// Traduit l'erreur métier en code HTTP (le message part dans le corps).
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::Agent(_) | Self::Mailbox => StatusCode::BAD_GATEWAY,
        }
    }
}

/// Envoie le prompt au proxy et renvoie la réponse de l'agent en JSON.
async fn chat(
    proxy: web::Data<Addr<ProxyAgent>>,
    req: web::Json<ChatRequest>,
) -> Result<web::Json<ChatResponse>, ProxyError> {
    let ChatRequest { session_id, content } = req.into_inner();
    let response = ask(&proxy, session_id.clone(), content).await?;
    Ok(web::Json(ChatResponse { session_id, response }))
}

/// Efface la session indiquée dans l'URL.
async fn clear(
    proxy: web::Data<Addr<ProxyAgent>>,
    session_id: web::Path<String>,
) -> Result<HttpResponse, ProxyError> {
    proxy.send(ClearSession { session_id: session_id.into_inner() }).await?;
    Ok(HttpResponse::NoContent().finish())
}

/// Renvoie la configuration courante.
async fn get_config(config: web::Data<Addr<ConfigActor>>) -> Result<web::Json<AppConfig>, ProxyError> {
    Ok(web::Json(config.send(GetConfig).await?))
}

/// Modifie un paramètre à chaud (pris en compte par les sessions créées ensuite).
async fn update_config(
    config: web::Data<Addr<ConfigActor>>,
    change: web::Json<UpdateConfig>,
) -> Result<web::Json<AppConfig>, ProxyError> {
    Ok(web::Json(config.send(change.into_inner()).await?))
}

/// Déclare les routes de l'API (partagé par le serveur et les tests).
fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/chat", web::post().to(chat))
        .route("/sessions/{session_id}", web::delete().to(clear))
        .route("/config", web::get().to(get_config))
        .route("/config", web::patch().to(update_config));
}

/// Construit le serveur HTTP lié à `addr` ; l'appelant l'attend ou le lance en tâche de fond.
pub fn server(proxy: Addr<ProxyAgent>, config: Addr<ConfigActor>, addr: &str) -> std::io::Result<Server> {
    let (proxy, config) = (web::Data::new(proxy), web::Data::new(config));
    Ok(HttpServer::new(move || App::new().app_data(proxy.clone()).app_data(config.clone()).configure(routes))
        .bind(addr)?
        .run())
}

#[cfg(test)]
mod tests {
    use actix_web::test::{TestRequest, call_service, init_service, read_body_json};
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::start_actors;

    /// Envoie `req` à une application de test branchée sur le faux Ollama ; renvoie statut et corps.
    async fn call(req: TestRequest) -> (StatusCode, Value) {
        let (proxy, config) = start_actors();
        let app = init_service(
            App::new().app_data(web::Data::new(proxy)).app_data(web::Data::new(config)).configure(routes),
        )
        .await;
        let resp = call_service(&app, req.to_request()).await;
        let status = resp.status();
        let body = if status.is_success() && status != StatusCode::NO_CONTENT { read_body_json(resp).await } else { Value::Null };
        (status, body)
    }

    /// Construit un `POST /chat` pour la session `t` avec ce contenu.
    fn chat_request(content: &str) -> TestRequest {
        TestRequest::post().uri("/chat").set_json(json!({ "session_id": "t", "content": content }))
    }

    /// `POST /chat` renvoie la réponse de l'agent avec l'identifiant de session.
    #[actix_web::test]
    async fn chat_returns_json_answer() {
        let (status, body) = call(chat_request("salut")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "session_id": "t", "response": "2 messages : salut" }));
    }

    /// Ollama en échec → 502.
    #[actix_web::test]
    async fn chat_agent_failure_is_bad_gateway() {
        assert_eq!(call(chat_request("ERREUR")).await.0, StatusCode::BAD_GATEWAY);
    }

    /// Réponse trop lente → 504.
    #[actix_web::test]
    async fn chat_timeout_is_gateway_timeout() {
        assert_eq!(call(chat_request("LENT")).await.0, StatusCode::GATEWAY_TIMEOUT);
    }

    /// `DELETE /sessions/{id}` → 204.
    #[actix_web::test]
    async fn clear_returns_no_content() {
        assert_eq!(call(TestRequest::delete().uri("/sessions/t")).await.0, StatusCode::NO_CONTENT);
    }

    /// `GET /config` renvoie la configuration courante.
    #[actix_web::test]
    async fn get_config_returns_current_config() {
        let (status, body) = call(TestRequest::get().uri("/config")).await;
        assert_eq!((status, body["model"].as_str()), (StatusCode::OK, Some("llama3.2")));
    }

    /// `PATCH /config` applique la modification et renvoie la configuration résultante.
    #[actix_web::test]
    async fn patch_config_updates_model() {
        let (status, body) = call(TestRequest::patch().uri("/config").set_json(json!({ "model": "qwen" }))).await;
        assert_eq!((status, body["model"].as_str()), (StatusCode::OK, Some("qwen")));
    }

    /// Un acteur arrêté → 502 (Mailbox).
    #[test]
    fn mailbox_error_is_bad_gateway() {
        assert_eq!(ProxyError::Mailbox.status_code(), StatusCode::BAD_GATEWAY);
    }

    /// `server` se lie à une adresse valide et refuse une adresse invalide.
    #[actix_web::test]
    async fn server_binds_or_reports_error() {
        let (proxy, config) = start_actors();
        assert!(server(proxy.clone(), config.clone(), "127.0.0.1:0").is_ok());
        assert!(server(proxy, config, "pas-une-adresse").is_err());
    }
}
