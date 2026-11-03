use std::error::Error;

use axum::{Router, http::header, routing::get};
use reqwest::Url;
use server::upstream_guard::{check_url, upstream_client};
use tokio::net::TcpListener;

fn chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        source = inner.source();
    }
    text
}

async fn fake_upstream() -> u16 {
    let app = Router::new().route("/", get(|| async { "ok" })).route(
        "/redirect",
        get(|| async {
            (
                axum::http::StatusCode::FOUND,
                [(header::LOCATION, "http://169.254.169.254/")],
            )
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

const BLOCKED: &str = "the upstream address is not allowed";

#[tokio::test]
async fn loopback_is_refused_without_the_setting() {
    // IP literals skip the resolver, so callers must run `check_url` first.
    let port = fake_upstream().await;
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    let error = check_url(&url, false).unwrap_err();
    assert_eq!(error.to_string(), BLOCKED);
    assert!(check_url(&url, true).is_ok());
}

#[tokio::test]
async fn loopback_works_with_the_setting() {
    let port = fake_upstream().await;
    let client = upstream_client(true).unwrap();
    let response = client
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn name_that_resolves_to_loopback_is_refused() {
    let port = fake_upstream().await;
    let client = upstream_client(false).unwrap();
    let error = client
        .get(format!("http://localhost:{port}/"))
        .send()
        .await
        .unwrap_err();
    assert!(chain(&error).contains(BLOCKED), "{}", chain(&error));
}

#[tokio::test]
async fn redirect_to_metadata_address_is_refused() {
    let port = fake_upstream().await;
    let client = upstream_client(true).unwrap();
    let error = client
        .get(format!("http://127.0.0.1:{port}/redirect"))
        .send()
        .await
        .unwrap_err();
    assert!(chain(&error).contains(BLOCKED), "{}", chain(&error));
}
