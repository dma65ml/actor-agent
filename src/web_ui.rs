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

/// Construit le serveur HTTP lié à `addr` ; l'appelant l'attend ou le lance en tâche de fond.
pub fn server(proxy: Addr<ProxyAgent>, config: Addr<ConfigActor>, addr: &str) -> std::io::Result<Server> {
    let (proxy, config) = (web::Data::new(proxy), web::Data::new(config));
    Ok(HttpServer::new(move || {
        App::new()
            .app_data(proxy.clone())
            .app_data(config.clone())
            .route("/chat", web::post().to(chat))
            .route("/sessions/{session_id}", web::delete().to(clear))
            .route("/config", web::get().to(get_config))
            .route("/config", web::patch().to(update_config))
    })
    .bind(addr)?
    .run())
}
