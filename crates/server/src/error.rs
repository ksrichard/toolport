use std::net::SocketAddr;

use axum::{
    Json,
    extract::{FromRequest, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::WWW_AUTHENTICATE},
    response::{IntoResponse, Redirect, Response},
};
use serde::Serialize;

use crate::auth::github::GithubError;

/// Errors that stop the server before or while it serves requests.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("cannot connect to the database")]
    DatabaseConnect(#[source] sqlx::Error),
    #[error("cannot run the database migrations")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("cannot create the HTTP client")]
    HttpClient(#[source] reqwest::Error),
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
    #[error("authentication is required")]
    Unauthorized,
    #[error("the request body is not valid: {0}")]
    InvalidBody(#[from] JsonRejection),
    #[error("the route does not exist")]
    NotFound,
    #[error("the MCP server does not exist")]
    McpServerNotFound,
    #[error("the database is not available")]
    DatabaseUnavailable(#[source] sqlx::Error),
    #[error("an internal error happened")]
    Database(#[source] sqlx::Error),
    #[error("an internal error happened")]
    TokenSigning(#[source] jsonwebtoken::errors::Error),
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
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::InvalidBody(_) => StatusCode::BAD_REQUEST,
            Self::NotFound | Self::McpServerNotFound => StatusCode::NOT_FOUND,
            Self::DatabaseUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Database(_) | Self::TokenSigning(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::InvalidBody(_) => "invalid_body",
            Self::NotFound => "not_found",
            Self::McpServerNotFound => "mcp_server_not_found",
            Self::DatabaseUnavailable(_) => "database_unavailable",
            Self::Database(_) | Self::TokenSigning(_) => "internal_error",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        // The client gets only the top-level message. The log keeps the full cause chain.
        if status.is_server_error() {
            tracing::error!(error = %error_chain(&self), code = self.code(), "API request failed");
        } else {
            tracing::debug!(error = %error_chain(&self), code = self.code(), "API request rejected");
        }
        let body = ErrorBody {
            error: self.code(),
            message: self.to_string(),
        };
        let mut response = (status, Json(body)).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

/// Same as `axum::Json`, but a bad body gives the standard `ApiError` JSON instead of plain text.
#[derive(FromRequest)]
#[from_request(via(Json), rejection(ApiError))]
pub struct ApiJson<T>(pub T);

/// Errors in the GitHub sign-in callback. The browser is on a full page load there,
/// so the response is a redirect to `/?login_error=<code>` and not JSON.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("GitHub sign-in was cancelled or denied: {0}")]
    GithubDenied(String),
    #[error("the sign-in state does not match")]
    InvalidState,
    #[error("GitHub did not send a login code")]
    MissingCode,
    #[error("the GitHub sign-in failed")]
    Github(#[from] GithubError),
    #[error("an internal error happened")]
    Database(#[from] sqlx::Error),
}

impl AuthError {
    fn code(&self) -> &'static str {
        match self {
            Self::GithubDenied(_) => "github_denied",
            Self::InvalidState => "invalid_state",
            Self::MissingCode => "missing_code",
            Self::Github(GithubError::Rejected(_)) => "github_rejected",
            Self::Github(GithubError::Request(_)) => "github_unavailable",
            Self::Database(_) => "internal_error",
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        tracing::warn!(error = %error_chain(&self), code = self.code(), "GitHub sign-in failed");
        Redirect::to(&format!("/?login_error={}", self.code())).into_response()
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
