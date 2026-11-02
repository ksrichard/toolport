use openapiv3::OpenAPI;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("the spec is not valid JSON")]
    NotJson(#[source] serde_json::Error),
    #[error("Swagger 2.0 specs are not supported, convert the spec to OpenAPI 3.0 first")]
    Swagger2,
    #[error("the spec has no `openapi` version field")]
    MissingVersion,
    #[error("OpenAPI version {0} is not supported, only 3.0.x is supported")]
    UnsupportedVersion(String),
    #[error("the spec does not match the OpenAPI 3.0 format")]
    Invalid(#[source] serde_json::Error),
}

/// Parses an OpenAPI 3.0 spec in JSON format.
pub fn parse_spec(text: &str) -> Result<OpenAPI, SpecError> {
    let value: Value = serde_json::from_str(text).map_err(SpecError::NotJson)?;
    if value.get("swagger").is_some() {
        return Err(SpecError::Swagger2);
    }
    let version = value
        .get("openapi")
        .and_then(Value::as_str)
        .ok_or(SpecError::MissingVersion)?;
    if !version.starts_with("3.0.") {
        return Err(SpecError::UnsupportedVersion(version.to_owned()));
    }
    // Parse the text again, not `value`: `Value` sorts object keys, and the tools should keep the spec order.
    serde_json::from_str(text).map_err(SpecError::Invalid)
}
