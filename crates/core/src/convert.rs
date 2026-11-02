use std::collections::BTreeSet;

use openapiv3::{
    Components, MediaType, OpenAPI, Operation, Parameter, ParameterSchemaOrContent, ReferenceOr,
    RequestBody,
};
use serde_json::{Map, Value, json};

use crate::{
    schema::{self, SchemaError},
    tool::{BODY_ARGUMENT, BodyDef, HttpMethod, ParamDef, ParamLocation, RequestTemplate, ToolDef},
};

/// MCP clients such as Claude accept tool names with at most 64 characters.
const MAX_TOOL_NAME_LEN: usize = 64;

/// OpenAPI says that these header parameters are ignored. HTTP and the upstream auth set them.
const IGNORED_HEADERS: [&str; 3] = ["accept", "content-type", "authorization"];

#[derive(Debug, Default)]
pub struct Conversion {
    pub tools: Vec<ToolDef>,
    pub skipped: Vec<SkippedOperation>,
}

/// An operation that cannot become a tool. The import shows these to the user.
#[derive(Debug)]
pub struct SkippedOperation {
    /// `None` when the whole path item was skipped.
    pub method: Option<HttpMethod>,
    pub path: String,
    pub reason: SkipReason,
}

#[derive(Debug, thiserror::Error)]
pub enum SkipReason {
    #[error("path item references are not supported")]
    PathItemRef,
    #[error("the reference {0} cannot be resolved")]
    UnresolvedRef(String),
    #[error("the parameter {0} uses `content`, only `schema` is supported")]
    ParameterContent(String),
    #[error("the parameter {0} is a required cookie, cookies are not supported")]
    RequiredCookie(String),
    #[error("the path uses {{{0}}}, but no path parameter has that name")]
    UndefinedPathParameter(String),
    #[error("the request body needs one of {0}, only JSON bodies are supported")]
    UnsupportedBody(String),
    #[error("two inputs have the name {0}")]
    DuplicateArgument(String),
    #[error("another operation already uses the tool name {0}")]
    DuplicateToolName(String),
    #[error("the schema cannot be converted")]
    Schema(#[from] SchemaError),
}

/// Makes one tool per operation. One bad operation does not stop the others.
pub fn convert(spec: &OpenAPI) -> Conversion {
    let mut conversion = Conversion::default();
    let mut names = BTreeSet::new();
    let components = spec.components.as_ref();

    for (path, item) in spec.paths.iter() {
        let ReferenceOr::Item(item) = item else {
            conversion.skipped.push(SkippedOperation {
                method: None,
                path: path.clone(),
                reason: SkipReason::PathItemRef,
            });
            continue;
        };
        for (method, operation) in item.iter() {
            let Some(method) = HttpMethod::from_openapi(method) else {
                continue;
            };
            let result = convert_operation(components, path, method, &item.parameters, operation)
                .and_then(|tool| {
                    if names.insert(tool.name.clone()) {
                        Ok(tool)
                    } else {
                        Err(SkipReason::DuplicateToolName(tool.name))
                    }
                });
            match result {
                Ok(tool) => conversion.tools.push(tool),
                Err(reason) => conversion.skipped.push(SkippedOperation {
                    method: Some(method),
                    path: path.clone(),
                    reason,
                }),
            }
        }
    }
    conversion
}

fn convert_operation(
    components: Option<&Components>,
    path: &str,
    method: HttpMethod,
    path_params: &[ReferenceOr<Parameter>],
    operation: &Operation,
) -> Result<ToolDef, SkipReason> {
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut params = Vec::new();
    let mut refs = BTreeSet::new();

    for parameter in merge_parameters(components, path_params, &operation.parameters)? {
        let data = parameter.parameter_data_ref();
        let location = match parameter {
            Parameter::Path { .. } => ParamLocation::Path,
            Parameter::Query { .. } => ParamLocation::Query,
            Parameter::Header { .. } => {
                if IGNORED_HEADERS.contains(&data.name.to_ascii_lowercase().as_str()) {
                    continue;
                }
                ParamLocation::Header
            }
            Parameter::Cookie { .. } if data.required => {
                return Err(SkipReason::RequiredCookie(data.name.clone()));
            }
            Parameter::Cookie { .. } => continue,
        };
        let ParameterSchemaOrContent::Schema(schema) = &data.format else {
            return Err(SkipReason::ParameterContent(data.name.clone()));
        };
        let mut schema = schema::convert(schema, &mut refs)?;
        add_description(&mut schema, data.description.as_deref());
        // OpenAPI says that path parameters are always required.
        let is_required = data.required || location == ParamLocation::Path;
        add_property(&mut properties, &data.name, schema)?;
        if is_required {
            required.push(data.name.clone());
        }
        params.push(ParamDef {
            name: data.name.clone(),
            location,
            required: is_required,
        });
    }

    for name in path_placeholders(path) {
        let defined = params
            .iter()
            .any(|p| p.location == ParamLocation::Path && p.name == name);
        if !defined {
            return Err(SkipReason::UndefinedPathParameter(name.to_owned()));
        }
    }

    let body = match &operation.request_body {
        None => None,
        Some(body) => {
            let body = resolve(body, |name| components?.request_bodies.get(name))?;
            convert_body(body, &mut properties, &mut required, &mut refs)?
        }
    };

    let mut input_schema = Map::new();
    input_schema.insert("type".into(), "object".into());
    input_schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        input_schema.insert("required".into(), json!(required));
    }
    let defs = schema::defs(components, refs)?;
    if !defs.is_empty() {
        input_schema.insert("$defs".into(), Value::Object(defs));
    }

    Ok(ToolDef {
        name: tool_name(operation.operation_id.as_deref(), method, path),
        description: description(operation, method, path),
        input_schema,
        request: RequestTemplate {
            method,
            path: path.to_owned(),
            params,
            body,
        },
    })
}

/// Joins the path-level and the operation-level parameters.
/// An operation parameter replaces a path parameter with the same name and location.
fn merge_parameters<'a>(
    components: Option<&'a Components>,
    path_params: &'a [ReferenceOr<Parameter>],
    operation_params: &'a [ReferenceOr<Parameter>],
) -> Result<Vec<&'a Parameter>, SkipReason> {
    let mut merged: Vec<&Parameter> = Vec::new();
    for parameter in path_params.iter().chain(operation_params) {
        let parameter = resolve(parameter, |name| components?.parameters.get(name))?;
        let key = (
            location_key(parameter),
            &parameter.parameter_data_ref().name,
        );
        merged.retain(|p| (location_key(p), &p.parameter_data_ref().name) != key);
        merged.push(parameter);
    }
    Ok(merged)
}

fn location_key(parameter: &Parameter) -> u8 {
    match parameter {
        Parameter::Path { .. } => 0,
        Parameter::Query { .. } => 1,
        Parameter::Header { .. } => 2,
        Parameter::Cookie { .. } => 3,
    }
}

fn convert_body(
    body: &RequestBody,
    properties: &mut Map<String, Value>,
    required: &mut Vec<String>,
    refs: &mut BTreeSet<String>,
) -> Result<Option<BodyDef>, SkipReason> {
    let Some((content_type, media)) = pick_json(body) else {
        if body.required {
            let types: Vec<&str> = body.content.keys().map(String::as_str).collect();
            return Err(SkipReason::UnsupportedBody(types.join(", ")));
        }
        return Ok(None);
    };
    let mut schema = match &media.schema {
        Some(schema) => schema::convert(schema, refs)?,
        None => json!({}),
    };
    add_description(&mut schema, body.description.as_deref());
    add_property(properties, BODY_ARGUMENT, schema)?;
    if body.required {
        required.push(BODY_ARGUMENT.to_owned());
    }
    Ok(Some(BodyDef {
        content_type: content_type.to_owned(),
        required: body.required,
    }))
}

/// Prefers `application/json`, then any `+json` type such as `application/problem+json`.
fn pick_json(body: &RequestBody) -> Option<(&str, &MediaType)> {
    let base = |key: &str| {
        key.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    };
    body.content
        .iter()
        .find(|(key, _)| base(key) == "application/json")
        .or_else(|| {
            body.content
                .iter()
                .find(|(key, _)| base(key).ends_with("+json"))
        })
        .map(|(key, media)| (key.as_str(), media))
}

/// The schema keeps its own description if it has one.
fn add_description(schema: &mut Value, text: Option<&str>) {
    let text = text.map(str::trim).filter(|t| !t.is_empty());
    if let (Some(text), Value::Object(map)) = (text, schema) {
        map.entry("description").or_insert_with(|| text.into());
    }
}

fn add_property(
    properties: &mut Map<String, Value>,
    name: &str,
    schema: Value,
) -> Result<(), SkipReason> {
    if properties.contains_key(name) {
        return Err(SkipReason::DuplicateArgument(name.to_owned()));
    }
    properties.insert(name.to_owned(), schema);
    Ok(())
}

/// Resolves a local `#/components/<kind>/<name>` reference. `lookup` finds a name in the map of that kind.
fn resolve<'a, T>(
    item: &'a ReferenceOr<T>,
    lookup: impl Fn(&str) -> Option<&'a ReferenceOr<T>>,
) -> Result<&'a T, SkipReason> {
    let mut current = item;
    // A reference can point to another reference. The limit stops a loop.
    for _ in 0..8 {
        let ReferenceOr::Reference { reference } = current else {
            break;
        };
        current = reference
            .rsplit_once('/')
            .filter(|(prefix, _)| prefix.starts_with("#/components/"))
            .and_then(|(_, name)| lookup(name))
            .ok_or_else(|| SkipReason::UnresolvedRef(reference.clone()))?;
    }
    match current {
        ReferenceOr::Item(item) => Ok(item),
        ReferenceOr::Reference { reference } => Err(SkipReason::UnresolvedRef(reference.clone())),
    }
}

pub(crate) fn path_placeholders(path: &str) -> impl Iterator<Item = &str> {
    path.split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name))
}

fn tool_name(operation_id: Option<&str>, method: HttpMethod, path: &str) -> String {
    let clean = |c: char| {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '_'
        }
    };
    let mut name: String = match operation_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => id.chars().map(clean).collect(),
        None => {
            // A generated name such as `get_pet_petId` for `GET /pet/{petId}`.
            let raw = format!("{}_{path}", method.as_str().to_ascii_lowercase());
            let mut name = String::new();
            for c in raw.chars().map(clean) {
                if !(c == '_' && name.ends_with('_')) {
                    name.push(c);
                }
            }
            name.trim_end_matches('_').to_owned()
        }
    };
    name.truncate(MAX_TOOL_NAME_LEN);
    name
}

fn description(operation: &Operation, method: HttpMethod, path: &str) -> String {
    let parts: Vec<&str> = [&operation.summary, &operation.description]
        .into_iter()
        .filter_map(|text| text.as_deref().map(str::trim).filter(|t| !t.is_empty()))
        .collect();
    if parts.is_empty() {
        format!("{method} {path}")
    } else {
        parts.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tool_names_are_readable() {
        assert_eq!(
            tool_name(None, HttpMethod::Get, "/pet/{petId}"),
            "get_pet_petId"
        );
        assert_eq!(
            tool_name(None, HttpMethod::Delete, "/a/b.json"),
            "delete_a_b_json"
        );
    }

    #[test]
    fn operation_ids_are_cleaned_and_cut() {
        assert_eq!(
            tool_name(Some("pets.list v2"), HttpMethod::Get, "/"),
            "pets_list_v2"
        );
        let long = "x".repeat(100);
        assert_eq!(
            tool_name(Some(&long), HttpMethod::Get, "/").len(),
            MAX_TOOL_NAME_LEN
        );
    }

    #[test]
    fn finds_path_placeholders() {
        let names: Vec<_> = path_placeholders("/a/{id}/b/{name}.json").collect();
        assert_eq!(names, ["id", "name"]);
    }
}
