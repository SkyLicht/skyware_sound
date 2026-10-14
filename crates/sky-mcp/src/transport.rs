// SPDX-License-Identifier: GPL-3.0-or-later
//! Serving the tools over stdio, or over Streamable HTTP on localhost.
//!
//! The HTTP server only listens on loopback, rejects any browser `Origin`
//! (so web pages cannot drive it), checks the `Host` header against DNS
//! rebinding, and needs the per-install bearer token on every request.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::SkywareMcp;

/// Serves one client over stdin and stdout until it disconnects.
pub async fn serve_stdio(server: SkywareMcp) -> anyhow::Result<()> {
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// A running HTTP server. Dropping it does not stop it; call `shutdown`.
pub struct HttpServer {
    pub addr: SocketAddr,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl HttpServer {
    /// The URL MCP clients connect to.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

fn token_matches(given: &str, expected: &str) -> bool {
    // Compare every byte so timing does not reveal a matching prefix.
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

async fn require_token(State(token): State<Arc<String>>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| token_matches(t.trim(), &token));
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            "Skyware Sound needs its access token: send \"Authorization: Bearer <token>\". \
             Run `skyware token` to print it.",
        )
            .into_response()
    }
}

/// Starts the Streamable HTTP server on `addr`. `make` builds the server
/// for each client connection; all of them share one session.
pub async fn serve_http(
    make: impl Fn() -> SkywareMcp + Send + Sync + 'static,
    addr: SocketAddr,
    token: String,
) -> anyhow::Result<HttpServer> {
    if !addr.ip().is_loopback() {
        anyhow::bail!("the MCP server only listens on localhost, not {addr}");
    }
    let cancel = CancellationToken::new();
    let config = StreamableHttpServerConfig::default()
        .with_cancellation_token(cancel.clone())
        .enforce_origin_validation();
    let service: StreamableHttpService<SkywareMcp, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(make()), Default::default(), config);
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(Arc::new(token), require_token));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
    let ct = cancel.clone();
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await
        {
            tracing::error!("MCP server stopped: {e}");
        }
    });
    tracing::info!("MCP server listening on http://{addr}/mcp");
    Ok(HttpServer { addr, cancel, task })
}

#[cfg(test)]
mod tests {
    use super::token_matches;

    #[test]
    fn tokens_compare_exactly() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abd", "abc"));
        assert!(!token_matches("ab", "abc"));
    }
}
