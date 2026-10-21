use std::net::SocketAddr;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Errors that stop the server before or while it serves requests.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("cannot connect to the database")]
    DatabaseConnect(#[source] sqlx::Error),
    #[error("cannot run the database migrations")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("cannot bind to {addr}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("the HTTP server stopped with an error")]
    Serve(#[source] std::io::Error),
}

/// Errors that an API handler returns. Each variant maps to one HTTP status and one stable code.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("the database is not available")]
    DatabaseUnavailable(#[source] sqlx::Error),
}

/// The JSON body of every API error. Clients match on `error`, not on `message`.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: &'static str,
    pub message: String,
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::DatabaseUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::DatabaseUnavailable(_) => "database_unavailable",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // The client gets only the top-level message. The log keeps the full cause chain.
        tracing::error!(error = %error_chain(&self), code = self.code(), "API request failed");
        let body = ErrorBody {
            error: self.code(),
            message: self.to_string(),
        };
        (self.status(), Json(body)).into_response()
    }
}

/// Formats an error and all its sources as "outer: inner: root".
pub fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
