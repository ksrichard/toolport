use serde_json::{Map, Value};

use crate::tool::{BODY_ARGUMENT, HttpMethod, ParamLocation, RequestTemplate};

/// The parts of one upstream HTTP request, before the server joins them with the base URL.
#[derive(Debug, PartialEq)]
pub struct UpstreamRequest {
    pub method: HttpMethod,
    /// Path segments with the arguments filled in. They are not percent-encoded yet.
    pub path_segments: Vec<String>,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    /// The content type and the JSON body.
    pub body: Option<(String, Value)>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ArgumentError {
    #[error("the argument {0} is required")]
    Missing(String),
    #[error("a path argument cannot be `.` or `..`")]
    DotSegment,
}

/// Fills a request template with the tool arguments. Arguments with a `null` value count as missing.
/// Arguments that the template does not know are ignored.
pub fn build_request(
    template: &RequestTemplate,
    args: &Map<String, Value>,
) -> Result<UpstreamRequest, ArgumentError> {
    let arg = |name: &str| args.get(name).filter(|v| !v.is_null());

    let mut path_values = Vec::new();
    let mut query = Vec::new();
    let mut headers = template.fixed_headers.clone();
    for param in &template.params {
        let Some(value) = arg(&param.name) else {
            if param.required {
                return Err(ArgumentError::Missing(param.name.clone()));
            }
            continue;
        };
        let key = param.target.as_ref().unwrap_or(&param.name);
        match param.location {
            ParamLocation::Path => path_values.push((key.as_str(), joined(value))),
            ParamLocation::Header => headers.push((key.clone(), joined(value))),
            // Form style with explode, the OpenAPI default: `?tag=a&tag=b`.
            ParamLocation::Query => match value {
                Value::Array(items) => {
                    for item in items.iter().filter(|v| !v.is_null()) {
                        query.push((key.clone(), text(item)));
                    }
                }
                _ => query.push((key.clone(), text(value))),
            },
        }
    }

    let path_segments = template
        .path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            // The URL parser resolves `..`, so this value would change the upstream path.
            let filled = fill(segment, &path_values)?;
            match filled.as_str() {
                "." | ".." => Err(ArgumentError::DotSegment),
                _ => Ok(filled),
            }
        })
        .collect::<Result<_, _>>()?;

    let body = match (&template.body, arg(BODY_ARGUMENT)) {
        (Some(def), Some(value)) => Some((def.content_type.clone(), value.clone())),
        (Some(def), None) if def.required => {
            return Err(ArgumentError::Missing(BODY_ARGUMENT.to_owned()));
        }
        _ => None,
    };

    Ok(UpstreamRequest {
        method: template.method,
        path_segments,
        query,
        headers,
        body,
    })
}

/// Replaces each `{name}` in one path segment, for example `{id}.json`.
fn fill(segment: &str, values: &[(&str, String)]) -> Result<String, ArgumentError> {
    let mut out = String::new();
    let mut rest = segment;
    while let Some((before, after)) = rest.split_once('{') {
        let Some((name, tail)) = after.split_once('}') else {
            break;
        };
        let value = values
            .iter()
            .find(|(n, _)| *n == name)
            .ok_or_else(|| ArgumentError::Missing(name.to_owned()))?;
        out.push_str(before);
        out.push_str(&value.1);
        rest = tail;
    }
    out.push_str(rest);
    Ok(out)
}

/// Simple style, the OpenAPI default for path and header parameters: `a,b,c`.
fn joined(value: &Value) -> String {
    match value {
        Value::Array(items) => items.iter().map(text).collect::<Vec<_>>().join(","),
        _ => text(value),
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{BodyDef, ParamDef};

    fn template() -> RequestTemplate {
        RequestTemplate {
            method: HttpMethod::Post,
            path: "/pet/{petId}/files/{name}.json".into(),
            params: vec![
                param("petId", ParamLocation::Path, true),
                param("name", ParamLocation::Path, true),
                param("tags", ParamLocation::Query, false),
                param("api_key", ParamLocation::Header, false),
            ],
            body: Some(BodyDef {
                content_type: "application/json".into(),
                required: false,
            }),
            fixed_headers: Vec::new(),
        }
    }

    fn param(name: &str, location: ParamLocation, required: bool) -> ParamDef {
        ParamDef {
            name: name.into(),
            location,
            required,
            target: None,
        }
    }

    fn args(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("not an object")
        };
        map
    }

    #[test]
    fn fills_every_location() {
        let request = build_request(
            &template(),
            &args(json!({
                "petId": 7,
                "name": "a/b",
                "tags": ["x", "y"],
                "api_key": "k",
                "body": { "id": 7 },
                "unknown": true,
            })),
        )
        .unwrap();
        assert_eq!(request.path_segments, ["pet", "7", "files", "a/b.json"]);
        assert_eq!(
            request.query,
            [("tags".into(), "x".into()), ("tags".into(), "y".into())]
        );
        assert_eq!(request.headers, [("api_key".into(), "k".into())]);
        assert_eq!(
            request.body,
            Some(("application/json".into(), json!({ "id": 7 })))
        );
    }

    #[test]
    fn optional_arguments_can_be_left_out_or_null() {
        let request = build_request(
            &template(),
            &args(json!({ "petId": 1, "name": "n", "tags": null })),
        )
        .unwrap();
        assert!(request.query.is_empty());
        assert!(request.headers.is_empty());
        assert_eq!(request.body, None);
    }

    #[test]
    fn missing_required_argument_is_an_error() {
        let result = build_request(&template(), &args(json!({ "name": "n" })));
        assert_eq!(result, Err(ArgumentError::Missing("petId".into())));
    }

    #[test]
    fn dot_segments_are_rejected() {
        for value in [".", ".."] {
            let result = build_request(&template(), &args(json!({ "petId": value, "name": "n" })));
            assert_eq!(result, Err(ArgumentError::DotSegment));
        }
    }

    #[test]
    fn missing_required_body_is_an_error() {
        let mut template = template();
        template.body = Some(BodyDef {
            content_type: "application/json".into(),
            required: true,
        });
        let result = build_request(&template, &args(json!({ "petId": 1, "name": "n" })));
        assert_eq!(result, Err(ArgumentError::Missing("body".into())));
    }

    #[test]
    fn target_renames_the_query_key() {
        let mut template = template();
        template.path = "/weather".into();
        template.params = vec![ParamDef {
            target: Some("latitude".into()),
            ..param("lat", ParamLocation::Query, true)
        }];
        let request = build_request(&template, &args(json!({ "lat": 47.5 }))).unwrap();
        assert_eq!(request.query, [("latitude".into(), "47.5".into())]);
    }

    #[test]
    fn fixed_headers_are_sent() {
        let mut template = template();
        template.fixed_headers = vec![("x-source".into(), "toolport".into())];
        let request = build_request(
            &template,
            &args(json!({ "petId": 1, "name": "n", "api_key": "k" })),
        )
        .unwrap();
        assert_eq!(
            request.headers,
            [
                ("x-source".into(), "toolport".into()),
                ("api_key".into(), "k".into())
            ]
        );
    }

    #[test]
    fn old_request_json_still_parses() {
        let old = json!({
            "method": "GET",
            "path": "/x",
            "params": [{ "name": "a", "location": "query", "required": true }],
            "body": null,
        });
        let template: RequestTemplate = serde_json::from_value(old).unwrap();
        assert_eq!(template.params[0].target, None);
        assert!(template.fixed_headers.is_empty());
    }
}
