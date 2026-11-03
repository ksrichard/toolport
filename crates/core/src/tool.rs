use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

/// The name of the tool argument that holds the JSON request body.
pub const BODY_ARGUMENT: &str = "body";

/// One API operation, served as one MCP tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// A JSON Schema object. It has one property per parameter, plus `body` for the request body.
    pub input_schema: Map<String, Value>,
    pub request: RequestTemplate,
}

/// What ToolPort needs to turn the tool arguments into an upstream HTTP request.
/// The database stores it as JSON, so field changes need a migration of the stored rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestTemplate {
    pub method: HttpMethod,
    /// The path from the spec, for example `/pet/{petId}`.
    pub path: String,
    pub params: Vec<ParamDef>,
    pub body: Option<BodyDef>,
    /// Headers that the server sends on every call, before the input headers.
    #[serde(default)]
    pub fixed_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamDef {
    pub name: String,
    pub location: ParamLocation,
    pub required: bool,
    /// The key sent upstream, when it differs from the input name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamLocation {
    Path,
    Query,
    Header,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyDef {
    pub content_type: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Put,
    Post,
    Delete,
    Options,
    Head,
    Patch,
    Trace,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Post => "POST",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Head => "HEAD",
            Self::Patch => "PATCH",
            Self::Trace => "TRACE",
        }
    }

    pub(crate) fn from_openapi(method: &str) -> Option<Self> {
        Some(match method {
            "get" => Self::Get,
            "put" => Self::Put,
            "post" => Self::Post,
            "delete" => Self::Delete,
            "options" => Self::Options,
            "head" => Self::Head,
            "patch" => Self::Patch,
            "trace" => Self::Trace,
            _ => return None,
        })
    }
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
