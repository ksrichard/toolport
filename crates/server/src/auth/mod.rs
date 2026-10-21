//! GitHub sign-in and the dashboard session.
//!
//! The browser keeps two tokens:
//! - The access JWT lives in memory only. The frontend sends it as `Authorization: Bearer`.
//! - The refresh token lives in an HttpOnly cookie. Its path is `/auth`, so the browser sends it
//!   only to `/auth/refresh` and `/auth/logout`. JavaScript cannot read it.

pub mod github;
pub mod jwt;
pub mod refresh;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Query, State},
    http::{StatusCode, header::AUTHORIZATION, request::Parts},
    response::Redirect,
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};

use crate::{
    error::{ApiError, AuthError},
    state::{AppState, AuthSettings},
    users,
};

const STATE_COOKIE: &str = "tp_oauth_state";
const STATE_COOKIE_PATH: &str = "/auth/github";
const REFRESH_COOKIE: &str = "tp_refresh";
const REFRESH_COOKIE_PATH: &str = "/auth";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/github/login", get(login))
        .route("/auth/github/callback", get(callback))
        .route("/auth/refresh", post(refresh_session))
        .route("/auth/logout", post(logout))
}

/// The signed-in user of an `/api` request. The handler fails with 401 when the
/// Bearer token is missing or not valid.
pub struct AuthUser {
    pub user_id: i64,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;
        let user_id = state.auth.jwt.verify_access(token).map_err(|error| {
            tracing::debug!(%error, "access token rejected");
            ApiError::Unauthorized
        })?;
        Ok(Self { user_id })
    }
}

/// The `state` value ties the GitHub callback to this browser. This blocks login CSRF.
async fn login(State(state): State<AppState>, jar: CookieJar) -> (CookieJar, Redirect) {
    let oauth_state = refresh::random_hex(16);
    let cookie = Cookie::build((STATE_COOKIE, oauth_state.clone()))
        .path(STATE_COOKIE_PATH)
        .http_only(true)
        .secure(state.auth.secure_cookies)
        // Lax, because the browser must send it on the redirect back from github.com.
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::minutes(10));
    let url = github::authorize_url(&state.auth, &oauth_state);
    (jar.add(cookie), Redirect::to(url.as_str()))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<(CookieJar, Redirect), AuthError> {
    let expected_state = jar.get(STATE_COOKIE).map(|c| c.value().to_owned());
    let jar = jar.remove(Cookie::build(STATE_COOKIE).path(STATE_COOKIE_PATH));

    if let Some(error) = query.error {
        return Err(AuthError::GithubDenied(error));
    }
    match (expected_state, query.state) {
        (Some(expected), Some(got)) if expected == got => {}
        _ => return Err(AuthError::InvalidState),
    }
    let code = query.code.ok_or(AuthError::MissingCode)?;

    let github_token = github::exchange_code(&state, &code).await?;
    let github_user = github::fetch_user(&state, &github_token).await?;
    let user_id = users::upsert_github_user(&state.pool, &github_user).await?;
    let refresh_token = refresh::create(&state.pool, user_id).await?;
    tracing::info!(user_id, github_login = %github_user.login, "user signed in");

    let jar = jar.add(refresh_cookie(&state.auth, refresh_token));
    Ok((jar, Redirect::to("/")))
}

#[derive(Serialize)]
struct AccessTokenResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
}

/// Gives a new access JWT and rotates the refresh token. The frontend calls this on page
/// load and whenever an API call returns 401.
async fn refresh_session(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Json<AccessTokenResponse>), ApiError> {
    let value = jar
        .get(REFRESH_COOKIE)
        .map(|c| c.value().to_owned())
        .ok_or(ApiError::Unauthorized)?;
    let (user_id, new_value) = refresh::rotate(&state.pool, &value)
        .await
        .map_err(ApiError::Database)?
        .ok_or(ApiError::Unauthorized)?;
    let access_token = state
        .auth
        .jwt
        .issue_access(user_id)
        .map_err(ApiError::TokenSigning)?;
    let body = AccessTokenResponse {
        access_token,
        token_type: "Bearer",
        expires_in: jwt::ACCESS_TOKEN_TTL.as_secs(),
    };
    Ok((jar.add(refresh_cookie(&state.auth, new_value)), Json(body)))
}

async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), ApiError> {
    if let Some(cookie) = jar.get(REFRESH_COOKIE) {
        refresh::revoke(&state.pool, cookie.value())
            .await
            .map_err(ApiError::Database)?;
    }
    let jar = jar.remove(Cookie::build(REFRESH_COOKIE).path(REFRESH_COOKIE_PATH));
    Ok((jar, StatusCode::NO_CONTENT))
}

fn refresh_cookie(settings: &AuthSettings, value: String) -> Cookie<'static> {
    Cookie::build((REFRESH_COOKIE, value))
        .path(REFRESH_COOKIE_PATH)
        .http_only(true)
        .secure(settings.secure_cookies)
        // Strict, because only the app itself calls /auth/refresh and /auth/logout.
        .same_site(SameSite::Strict)
        .max_age(cookie::time::Duration::seconds(
            refresh::REFRESH_TOKEN_TTL.as_secs() as i64,
        ))
        .build()
}
