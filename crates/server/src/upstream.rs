//! One function that sends a tool call to the upstream API. The MCP endpoint and the Test route share it.

use std::time::{Duration, Instant};

use axum::http::{
    HeaderName, HeaderValue, StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
};
use reqwest::{Method, Url};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use serde::Serialize;
use toolport_core::{HttpMethod, RequestTemplate, UpstreamRequest, build_request};
use ts_rs::TS;

use crate::{
    calls::CallResult,
    error::error_chain,
    mcp::CallError,
    secrets::{SecretsKey, open},
    upstream_guard::check_url,
};

/// A larger upstream response is cut, so one call cannot fill the memory or the client context.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// The trace shows this instead of a secret value.
const MASK: &str = "••••";

pub struct UpstreamCtx<'a> {
    pub http: &'a reqwest::Client,
    pub secrets: &'a SecretsKey,
    pub timeout: Duration,
    /// `check_url` must run here, because reqwest skips the guarded resolver for IP-literal hosts.
    pub allow_loopback: bool,
}

pub struct ApiTarget {
    pub id: i64,
    pub base_url: String,
    pub auth: UpstreamAuth,
}

/// The sealed bytes come from the `apis.auth_secret` column.
pub enum UpstreamAuth {
    None,
    ApiKey { header: String, sealed: Vec<u8> },
    Bearer { sealed: Vec<u8> },
}

impl UpstreamAuth {
    /// Builds the auth from the three `apis` columns. A row that breaks the table check gives `None`.
    pub fn from_columns(
        auth_type: &str,
        header: Option<String>,
        sealed: Option<Vec<u8>>,
    ) -> Option<Self> {
        match (auth_type, header, sealed) {
            ("none", _, _) => Some(Self::None),
            ("bearer", _, Some(sealed)) => Some(Self::Bearer { sealed }),
            ("api_key", Some(header), Some(sealed)) => Some(Self::ApiKey { header, sealed }),
            _ => None,
        }
    }
}

/// A secret that the Test route got in its body. It wins over the sealed one.
pub enum DraftSecret<'a> {
    Saved,
    New(&'a str),
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct HttpTrace {
    pub request: TracedRequest,
    pub response: Option<TracedResponse>,
    #[ts(type = "number")]
    pub latency_ms: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct TracedRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct TracedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// True when the body was longer than the limit.
    pub cut: bool,
}

pub struct CallOutput {
    pub result: CallToolResult,
    pub outcome: Outcome,
    pub trace: HttpTrace,
}

/// Sends the request for one tool call. The secret is decrypted right before the send.
pub async fn call_upstream(
    ctx: &UpstreamCtx<'_>,
    api: &ApiTarget,
    secret: DraftSecret<'_>,
    template: &RequestTemplate,
    args: &JsonObject,
) -> Result<CallOutput, CallError> {
    let upstream = build_request(template, args)?;
    let url = upstream_url(&api.base_url, &upstream)?;

    let (auth_name, sealed, prefix) = match &api.auth {
        UpstreamAuth::None => (None, None, ""),
        UpstreamAuth::ApiKey { header, sealed } => (Some(header.as_str()), Some(sealed), ""),
        UpstreamAuth::Bearer { sealed } => (Some(AUTHORIZATION.as_str()), Some(sealed), "Bearer "),
    };

    let mut trace_headers = vec![("accept".to_owned(), "application/json".to_owned())];
    let mut builder = ctx
        .http
        .request(method(upstream.method), url.clone())
        .timeout(ctx.timeout)
        .header(ACCEPT, "application/json");
    for (name, value) in &upstream.headers {
        // The API auth replaces a tool header with the same name.
        if auth_name.is_some_and(|auth| auth.eq_ignore_ascii_case(name)) {
            continue;
        }
        let invalid = || CallError::InvalidHeader(name.clone());
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
        let header_value = HeaderValue::from_str(value).map_err(|_| invalid())?;
        builder = builder.header(header_name, header_value);
        trace_headers.push((name.clone(), value.clone()));
    }
    let body = upstream.body.as_ref().map(|(_, body)| body.to_string());
    if let Some((content_type, _)) = &upstream.body {
        builder = builder
            .header(CONTENT_TYPE, content_type)
            .body(body.clone().unwrap_or_default());
        trace_headers.push(("content-type".to_owned(), content_type.clone()));
    }

    // `opened` keeps the plain text until the end of the function, so we can mask the response.
    let opened;
    let mut plain_secret = "";
    if let Some(name) = auth_name {
        plain_secret = match (&secret, sealed) {
            (DraftSecret::New(plain), _) => plain,
            (DraftSecret::Saved, Some(sealed)) => {
                opened = open(ctx.secrets, api.id, sealed)?;
                opened.expose()
            }
            (DraftSecret::Saved, None) => "",
        };
        let invalid = || CallError::InvalidHeader(name.to_owned());
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
        let mut value =
            HeaderValue::from_str(&format!("{prefix}{plain_secret}")).map_err(|_| invalid())?;
        value.set_sensitive(true);
        builder = builder.header(header_name, value);
        trace_headers.push((name.to_owned(), format!("{prefix}{MASK}")));
    }

    let traced_request = TracedRequest {
        method: upstream.method.to_string(),
        url: url.to_string(),
        headers: trace_headers,
        body,
    };
    // reqwest does not check IP-literal hosts in its resolver, so we check the final URL here.
    if check_url(&url, ctx.allow_loopback).is_err() {
        return Ok(CallOutput {
            result: blocked_result(),
            outcome: Outcome::Failed,
            trace: HttpTrace {
                request: traced_request,
                response: None,
                latency_ms: 0,
            },
        });
    }
    let started = Instant::now();
    let (mut outcome, mut response) = send(builder).await;
    let latency_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    // An upstream can echo the secret back. Neither the trace nor the tool result may show it.
    if !plain_secret.is_empty() {
        if let Outcome::Answered { text, .. } = &mut outcome {
            *text = text.replace(plain_secret, MASK);
        }
        if let Some(response) = &mut response {
            response.body = response.body.replace(plain_secret, MASK);
            for (_, value) in &mut response.headers {
                *value = value.replace(plain_secret, MASK);
            }
        }
    }
    Ok(CallOutput {
        result: outcome.to_result(ctx.timeout),
        outcome,
        trace: HttpTrace {
            request: traced_request,
            response,
            latency_ms,
        },
    })
}

fn method(method: HttpMethod) -> Method {
    match method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Post => Method::POST,
        HttpMethod::Delete => Method::DELETE,
        HttpMethod::Options => Method::OPTIONS,
        HttpMethod::Head => Method::HEAD,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Trace => Method::TRACE,
    }
}

/// Adds the path segments to the base URL path, for example `/api/v3` + `pet/7`.
/// The URL encoder escapes each segment, so a `/` inside a value stays inside its segment.
fn upstream_url(base: &str, request: &UpstreamRequest) -> Result<Url, CallError> {
    let invalid = || CallError::BaseUrl(base.to_owned());
    let mut url = Url::parse(base).map_err(|_| invalid())?;
    url.path_segments_mut()
        .map_err(|()| invalid())?
        .pop_if_empty()
        .extend(&request.path_segments);
    if !request.query.is_empty() {
        url.query_pairs_mut().extend_pairs(&request.query);
    }
    Ok(url)
}

pub enum Outcome {
    Answered {
        status: StatusCode,
        text: String,
        cut: bool,
    },
    TimedOut,
    Failed,
    /// The resolver or a redirect hit a blocked address.
    Blocked,
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Answered { status, .. } if status.is_success() => "ok",
            Self::Answered { .. } => "upstream_error",
            Self::TimedOut => "timed_out",
            Self::Failed | Self::Blocked => "request_failed",
        }
    }

    pub fn call_result(&self) -> CallResult {
        match self {
            Self::Answered { status, .. } if status.is_success() => CallResult::Ok,
            Self::Answered { status, .. } if *status == StatusCode::NOT_FOUND => {
                CallResult::NotFound
            }
            Self::Answered { .. } | Self::Failed | Self::Blocked => CallResult::UpstreamError,
            Self::TimedOut => CallResult::TimedOut,
        }
    }

    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Answered { status, .. } => Some(*status),
            Self::TimedOut | Self::Failed | Self::Blocked => None,
        }
    }

    fn to_result(&self, timeout: Duration) -> CallToolResult {
        match self {
            Self::Answered { status, text, cut } => {
                let mut message = if status.is_success() {
                    text.clone()
                } else {
                    format!("the upstream API answered HTTP {status}\n\n{text}")
                };
                if message.is_empty() {
                    message = format!("the upstream API answered HTTP {status} with an empty body");
                }
                if *cut {
                    message.push_str("\n\n[ToolPort cut this response at 1 MiB]");
                }
                let content = vec![ContentBlock::text(message)];
                if status.is_success() {
                    CallToolResult::success(content)
                } else {
                    CallToolResult::error(content)
                }
            }
            Self::TimedOut => CallToolResult::error(vec![ContentBlock::text(format!(
                "the upstream API did not answer within {} ms",
                timeout.as_millis()
            ))]),
            Self::Failed => CallToolResult::error(vec![ContentBlock::text(
                "the upstream API cannot be reached",
            )]),
            Self::Blocked => blocked_result(),
        }
    }
}

/// The tool result for a call that ToolPort did not send, because the address is blocked.
pub fn blocked_result() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        "the upstream address is not allowed",
    )])
}

async fn send(builder: reqwest::RequestBuilder) -> (Outcome, Option<TracedResponse>) {
    let result = match builder.send().await {
        Ok(response) => {
            let status = response.status();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.to_string(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect();
            read_body(response).await.map(|(text, cut)| {
                let traced = TracedResponse {
                    status: status.as_u16(),
                    headers,
                    body: text.clone(),
                    cut,
                };
                (Outcome::Answered { status, text, cut }, Some(traced))
            })
        }
        Err(error) => Err(error),
    };
    result.unwrap_or_else(|error| {
        if is_blocked(&error) {
            (Outcome::Blocked, None)
        } else if error.is_timeout() {
            (Outcome::TimedOut, None)
        } else {
            tracing::warn!(error = %error_chain(&error), "upstream request failed");
            (Outcome::Failed, None)
        }
    })
}

/// The guarded resolver and the redirect policy report a blocked address as the error source.
fn is_blocked(error: &reqwest::Error) -> bool {
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        if inner.is::<crate::upstream_guard::BlockedAddress>() {
            return true;
        }
        source = inner.source();
    }
    false
}

/// Reads at most `MAX_RESPONSE_BYTES`. The bool is true when the body was longer.
async fn read_body(mut response: reqwest::Response) -> Result<(String, bool), reqwest::Error> {
    let mut bytes = Vec::new();
    let mut cut = false;
    while let Some(chunk) = response.chunk().await? {
        let room = MAX_RESPONSE_BYTES - bytes.len();
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            cut = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), cut))
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, http::HeaderMap, routing::get};
    use serde_json::json;
    use tokio::net::TcpListener;
    use toolport_core::RequestTemplate;

    use super::*;
    use crate::secrets::seal;

    fn key() -> SecretsKey {
        SecretsKey::from_hex(&"1".repeat(64)).unwrap()
    }

    fn template() -> RequestTemplate {
        serde_json::from_value(json!({
            "method": "GET", "path": "/who", "params": [], "body": null
        }))
        .unwrap()
    }

    /// A fake upstream that tells only whether the header had the expected value.
    /// It must not echo the secret, because the trace holds the response body.
    async fn fake(header: &'static str, expected: &'static str) -> String {
        let app = Router::new().route(
            "/who",
            get(move |headers: HeaderMap| async move {
                let got = headers.get(header).and_then(|v| v.to_str().ok());
                Json(json!({ "match": got == Some(expected) }))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn trace_for(
        auth: UpstreamAuth,
        secret: DraftSecret<'_>,
        header: &'static str,
        expected: &'static str,
    ) -> (CallOutput, String) {
        let http = reqwest::Client::new();
        let secrets = key();
        let ctx = UpstreamCtx {
            http: &http,
            secrets: &secrets,
            timeout: Duration::from_secs(5),
            allow_loopback: true,
        };
        let api = ApiTarget {
            id: 7,
            base_url: fake(header, expected).await,
            auth,
        };
        let output = call_upstream(&ctx, &api, secret, &template(), &JsonObject::new())
            .await
            .unwrap();
        let dump = serde_json::to_string(&output.trace).unwrap();
        (output, dump)
    }

    fn matched(output: &CallOutput) -> bool {
        output.trace.response.as_ref().unwrap().body == r#"{"match":true}"#
    }

    #[tokio::test]
    async fn trace_masks_bearer_secret() {
        let sealed = seal(&key(), 7, "s3cret-bearer").unwrap();
        let auth = UpstreamAuth::Bearer { sealed };
        let (output, dump) = trace_for(
            auth,
            DraftSecret::Saved,
            "authorization",
            "Bearer s3cret-bearer",
        )
        .await;
        assert!(matched(&output));
        assert!(!dump.contains("s3cret-bearer"));
        let headers = &output.trace.request.headers;
        assert!(headers.contains(&("authorization".to_owned(), "Bearer ••••".to_owned())));
    }

    #[tokio::test]
    async fn trace_masks_api_key_secret() {
        let sealed = seal(&key(), 7, "s3cret-key").unwrap();
        let auth = UpstreamAuth::ApiKey {
            header: "X-Api-Key".into(),
            sealed,
        };
        let (output, dump) = trace_for(auth, DraftSecret::Saved, "x-api-key", "s3cret-key").await;
        assert!(matched(&output));
        assert!(!dump.contains("s3cret-key"));
        let headers = &output.trace.request.headers;
        assert!(headers.contains(&("X-Api-Key".to_owned(), "••••".to_owned())));
    }

    #[tokio::test]
    async fn new_secret_wins_over_the_sealed_one() {
        let sealed = seal(&key(), 7, "old-secret").unwrap();
        let auth = UpstreamAuth::Bearer { sealed };
        let (output, dump) = trace_for(
            auth,
            DraftSecret::New("new-secret"),
            "authorization",
            "Bearer new-secret",
        )
        .await;
        assert!(matched(&output));
        assert!(!dump.contains("new-secret") && !dump.contains("old-secret"));
    }

    #[tokio::test]
    async fn echoed_secret_is_masked_in_trace_and_result() {
        let app = Router::new().route(
            "/who",
            get(|headers: HeaderMap| async move {
                let auth = headers["authorization"].to_str().unwrap().to_owned();
                ([("x-echo", auth.clone())], auth)
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();
        let secrets = key();
        let ctx = UpstreamCtx {
            http: &http,
            secrets: &secrets,
            timeout: Duration::from_secs(5),
            allow_loopback: true,
        };
        let api = ApiTarget {
            id: 7,
            base_url: format!("http://{addr}"),
            auth: UpstreamAuth::Bearer {
                sealed: seal(&key(), 7, "echo-secret").unwrap(),
            },
        };
        let output = call_upstream(
            &ctx,
            &api,
            DraftSecret::Saved,
            &template(),
            &JsonObject::new(),
        )
        .await
        .unwrap();
        let dump = serde_json::to_string(&output.trace).unwrap();
        let result = serde_json::to_string(&output.result).unwrap();
        assert!(!dump.contains("echo-secret"));
        assert!(!result.contains("echo-secret"));
        assert!(dump.contains("Bearer ••••"));
    }

    #[tokio::test]
    async fn auth_replaces_a_tool_header_with_the_same_name() {
        let mut template = template();
        template.fixed_headers = vec![("Authorization".into(), "Bearer from-tool".into())];
        let http = reqwest::Client::new();
        let secrets = key();
        let ctx = UpstreamCtx {
            http: &http,
            secrets: &secrets,
            timeout: Duration::from_secs(5),
            allow_loopback: true,
        };
        let api = ApiTarget {
            id: 7,
            base_url: fake("authorization", "Bearer from-api").await,
            auth: UpstreamAuth::Bearer {
                sealed: seal(&key(), 7, "from-api").unwrap(),
            },
        };
        let output = call_upstream(
            &ctx,
            &api,
            DraftSecret::Saved,
            &template,
            &JsonObject::new(),
        )
        .await
        .unwrap();
        assert!(matched(&output));
        let auth_headers: Vec<_> = output
            .trace
            .request
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .collect();
        assert_eq!(auth_headers.len(), 1);
    }
}
