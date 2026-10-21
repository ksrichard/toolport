use reqwest::{Url, header::ACCEPT};
use serde::Deserialize;
use serde_json::json;

use crate::state::{AppState, AuthSettings};

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("the request to GitHub failed")]
    Request(#[from] reqwest::Error),
    #[error("GitHub rejected the login code: {0}")]
    Rejected(String),
}

#[derive(Debug, Deserialize)]
pub struct GithubUser {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// GitHub answers 200 for a bad code too. The body then has `error` and no `access_token`.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
}

pub fn callback_url(settings: &AuthSettings) -> Url {
    join(&settings.public_url, "auth/github/callback")
}

pub fn authorize_url(settings: &AuthSettings, state: &str) -> Url {
    let mut url = join(&settings.github_oauth_url, "login/oauth/authorize");
    url.query_pairs_mut()
        .append_pair("client_id", &settings.github_client_id)
        .append_pair("redirect_uri", callback_url(settings).as_str())
        .append_pair("scope", "read:user")
        .append_pair("state", state);
    url
}

pub async fn exchange_code(state: &AppState, code: &str) -> Result<String, GithubError> {
    let settings = &state.auth;
    let response: TokenResponse = state
        .http
        .post(join(&settings.github_oauth_url, "login/oauth/access_token"))
        .header(ACCEPT, "application/json")
        .json(&json!({
            "client_id": settings.github_client_id,
            "client_secret": settings.github_client_secret,
            "code": code,
            "redirect_uri": callback_url(settings).as_str(),
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    response
        .access_token
        .ok_or_else(|| GithubError::Rejected(response.error.unwrap_or_default()))
}

pub async fn fetch_user(state: &AppState, access_token: &str) -> Result<GithubUser, GithubError> {
    let user = state
        .http
        .get(join(&state.auth.github_api_url, "user"))
        .header(ACCEPT, "application/vnd.github+json")
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(user)
}

/// `Url::join` drops the last path segment when the base has no trailing slash.
/// This keeps a base path such as `https://ghe.example.com/api/v3`.
fn join(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    url.set_path(&format!("{}/{}", base.path().trim_end_matches('/'), path));
    url
}
