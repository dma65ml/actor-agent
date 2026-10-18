//! Interface Web : API HTTP JSON branchée sur le `ProxyAgent`.
//!
//! - `POST /chat` `{ "session_id": "...", "content": "..." }` → `{ "session_id": "...", "response": "..." }`
//! - `DELETE /sessions/{session_id}` → 204, efface l'historique de la session.

use actix::Addr;
use actix_web::{App, HttpResponse, HttpServer, ResponseError, dev::Server, http::StatusCode, web};
use serde::{Deserialize, Serialize};

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

/// Construit le serveur HTTP lié à `addr` ; l'appelant l'attend ou le lance en tâche de fond.
pub fn server(proxy: Addr<ProxyAgent>, addr: &str) -> std::io::Result<Server> {
    let proxy = web::Data::new(proxy);
    Ok(HttpServer::new(move || {
        App::new()
            .app_data(proxy.clone())
            .route("/chat", web::post().to(chat))
            .route("/sessions/{session_id}", web::delete().to(clear))
    })
    .bind(addr)?
    .run())
}
