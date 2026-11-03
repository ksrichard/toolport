//! The input table of a tool, and the JSON schema that `tools/list` sends for it.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ts_rs::TS;

use crate::tool::BODY_ARGUMENT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    String,
    Number,
    Integer,
    Boolean,
    StringArray,
    /// A schema that the table cannot show, such as an enum or a nested object.
    Complex,
}

/// One row of the input table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InputRow {
    pub name: String,
    pub input_type: InputType,
    pub required: bool,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("the part {0:?} must look like key={{input}}")]
    BadPair(String),
}

/// Builds the input schema of a hand-made tool. The validator must have rejected `Complex` rows.
pub fn schema_from_rows(rows: &[InputRow], body: Option<&Value>) -> Map<String, Value> {
    let mut properties = Map::new();
    for row in rows {
        let mut property = Map::new();
        set_type(&mut property, row.input_type);
        set_description(&mut property, &row.description);
        properties.insert(row.name.clone(), Value::Object(property));
    }
    if let Some(body) = body {
        properties.insert(BODY_ARGUMENT.into(), body.clone());
    }
    let mut schema = Map::new();
    schema.insert("type".into(), "object".into());
    schema.insert("properties".into(), Value::Object(properties));
    let required = required_names(rows);
    if !required.is_empty() {
        schema.insert("required".into(), json!(required));
    }
    schema
}

/// Reads the input table from a schema. Properties that the table cannot show are `Complex`.
pub fn rows_from_schema(schema: &Map<String, Value>) -> Vec<InputRow> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| InputRow {
            name: name.clone(),
            input_type: input_type_of(property),
            required: required.contains(&name.as_str()),
            description: property
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect()
}

/// Applies the table to the schema of a spec tool. It changes only `description`, `required`,
/// and the `type` of simple properties. Every other key stays as the spec has it.
pub fn apply_rows(schema: &Map<String, Value>, rows: &[InputRow]) -> Map<String, Value> {
    let mut out = schema.clone();
    if let Some(Value::Object(properties)) = out.get_mut("properties") {
        for row in rows {
            let Some(Value::Object(property)) = properties.get_mut(&row.name) else {
                continue;
            };
            set_description(property, &row.description);
            // Never change the type of a property that the table cannot show.
            if row.input_type == InputType::Complex || is_complex(property) {
                continue;
            }
            if row.input_type == InputType::StringArray
                && property.get("type") == Some(&json!("array"))
            {
                continue;
            }
            set_type(property, row.input_type);
        }
    }
    // Names without a row stay as the spec has them.
    let old: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let required: Vec<&str> = old
        .into_iter()
        .filter(|name| !rows.iter().any(|row| row.name == *name))
        .chain(required_names(rows))
        .collect();
    if required.is_empty() {
        out.remove("required");
    } else {
        out.insert("required".into(), json!(required));
    }
    out
}

/// The tool as `tools/list` sends it. This matches the fields of an rmcp `Tool`.
pub fn tool_json(name: &str, description: &str, schema: &Map<String, Value>) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

/// Parses `latitude={lat}&longitude={lon}` into pairs of query key and input name.
pub fn parse_query_template(text: &str) -> Result<Vec<(String, String)>, TemplateError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    text.split('&')
        .map(|part| {
            let bad = || TemplateError::BadPair(part.to_owned());
            let (key, value) = part.split_once('=').ok_or_else(bad)?;
            let input = value
                .strip_prefix('{')
                .and_then(|v| v.strip_suffix('}'))
                .ok_or_else(bad)?;
            let invalid = |s: &str| s.is_empty() || s.contains(['{', '}', '&', '=']);
            if invalid(key) || invalid(input) {
                return Err(bad());
            }
            Ok((key.to_owned(), input.to_owned()))
        })
        .collect()
}

pub fn print_query_template(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, input)| format!("{key}={{{input}}}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn required_names(rows: &[InputRow]) -> Vec<&str> {
    rows.iter()
        .filter(|row| row.required)
        .map(|row| row.name.as_str())
        .collect()
}

fn set_description(property: &mut Map<String, Value>, description: &str) {
    if description.is_empty() {
        property.remove("description");
    } else {
        property.insert("description".into(), description.into());
    }
}

fn set_type(property: &mut Map<String, Value>, input_type: InputType) {
    let name = match input_type {
        InputType::String => "string",
        InputType::Number => "number",
        InputType::Integer => "integer",
        InputType::Boolean => "boolean",
        InputType::StringArray => "array",
        InputType::Complex => return,
    };
    property.insert("type".into(), name.into());
    if input_type == InputType::StringArray {
        property.insert("items".into(), json!({ "type": "string" }));
    } else {
        property.remove("items");
    }
}

fn is_complex(property: &Map<String, Value>) -> bool {
    input_type_of(&Value::Object(property.clone())) == InputType::Complex
}

fn input_type_of(property: &Value) -> InputType {
    const COMPLEX_KEYS: [&str; 5] = ["enum", "format", "$ref", "oneOf", "anyOf"];
    let Some(property) = property.as_object() else {
        return InputType::Complex;
    };
    if COMPLEX_KEYS
        .iter()
        .chain(&["allOf"])
        .any(|key| property.contains_key(*key))
    {
        return InputType::Complex;
    }
    match property.get("type").and_then(Value::as_str) {
        Some("string") => InputType::String,
        Some("number") => InputType::Number,
        Some("integer") => InputType::Integer,
        Some("boolean") => InputType::Boolean,
        Some("array") => match property.get("items") {
            // Any other key in `items`, such as `enum` or `format`, makes it complex.
            Some(Value::Object(items))
                if items.len() == 1 && items.get("type") == Some(&json!("string")) =>
            {
                InputType::StringArray
            }
            _ => InputType::Complex,
        },
        _ => InputType::Complex,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("not an object")
        };
        map
    }

    fn row(name: &str, input_type: InputType, required: bool, description: &str) -> InputRow {
        InputRow {
            name: name.into(),
            input_type,
            required,
            description: description.into(),
        }
    }

    #[test]
    fn rows_from_schema_marks_enum_as_complex() {
        let schema = obj(json!({
            "type": "object",
            "properties": { "a": { "type": "string", "enum": ["a"] }, "b": { "type": "string" } },
        }));
        let rows = rows_from_schema(&schema);
        assert_eq!(rows[0].input_type, InputType::Complex);
        assert_eq!(rows[1].input_type, InputType::String);
    }

    #[test]
    fn rows_from_schema_marks_other_shapes_as_complex() {
        for property in [
            json!({ "type": "string", "format": "date" }),
            json!({ "$ref": "#/$defs/x" }),
            json!({ "oneOf": [{ "type": "string" }] }),
            json!({ "allOf": [{ "type": "string" }] }),
            json!({ "type": "object" }),
            json!({ "type": "array", "items": { "type": "integer" } }),
        ] {
            let schema = obj(json!({ "properties": { "p": property } }));
            assert_eq!(rows_from_schema(&schema)[0].input_type, InputType::Complex);
        }
    }

    #[test]
    fn apply_rows_keeps_enum_and_nested_object() {
        let schema = obj(json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["a", "b"] },
                "pet": { "type": "object", "properties": { "id": { "type": "integer" } } },
            },
        }));
        let mut rows = rows_from_schema(&schema);
        let status = rows.iter_mut().find(|r| r.name == "status").unwrap();
        status.description = "New text".into();
        let out = apply_rows(&schema, &rows);
        assert_eq!(out["properties"]["pet"], schema["properties"]["pet"]);
        assert_eq!(out["properties"]["status"]["enum"], json!(["a", "b"]));
        assert_eq!(out["properties"]["status"]["description"], "New text");
    }

    #[test]
    fn rows_from_schema_marks_enum_array_as_complex() {
        let schema = obj(json!({ "properties": {
            "status": { "type": "array", "items": { "type": "string", "enum": ["a"] } },
            "tags": { "type": "array", "items": { "type": "string" } },
        } }));
        let rows = rows_from_schema(&schema);
        assert_eq!(rows[0].input_type, InputType::Complex);
        assert_eq!(rows[1].input_type, InputType::StringArray);
    }

    #[test]
    fn apply_rows_keeps_enum_array_items() {
        let schema = obj(json!({ "properties": {
            "status": { "type": "array", "items": { "type": "string", "enum": ["a"] } },
        } }));
        let mut rows = rows_from_schema(&schema);
        rows[0].input_type = InputType::StringArray;
        let out = apply_rows(&schema, &rows);
        assert_eq!(out["properties"], schema["properties"]);
    }

    #[test]
    fn apply_rows_keeps_required_names_without_rows() {
        let schema = obj(json!({
            "properties": { "a": { "type": "string" }, "b": { "type": "string" } },
            "required": ["a", "b"],
        }));
        let rows = [row("b", InputType::String, false, "")];
        assert_eq!(apply_rows(&schema, &rows)["required"], json!(["a"]));
    }

    #[test]
    fn apply_rows_updates_required_list() {
        let schema = obj(json!({
            "type": "object",
            "properties": { "a": { "type": "string" }, "b": { "type": "string" } },
            "required": ["a"],
        }));
        let mut rows = rows_from_schema(&schema);
        rows[0].required = false;
        rows[1].required = true;
        assert_eq!(apply_rows(&schema, &rows)["required"], json!(["b"]));
        rows[1].required = false;
        assert!(!apply_rows(&schema, &rows).contains_key("required"));
    }

    #[test]
    fn apply_rows_changes_the_type() {
        let schema = obj(json!({ "properties": { "a": { "type": "string", "minLength": 2 } } }));
        let rows = [row("a", InputType::Integer, false, "")];
        let out = apply_rows(&schema, &rows);
        assert_eq!(
            out["properties"]["a"],
            json!({ "type": "integer", "minLength": 2 })
        );
    }

    #[test]
    fn query_template_round_trip() {
        let text = "latitude={lat}&longitude={lon}";
        let pairs = parse_query_template(text).unwrap();
        assert_eq!(
            pairs,
            [
                ("latitude".to_owned(), "lat".to_owned()),
                ("longitude".to_owned(), "lon".to_owned())
            ]
        );
        assert_eq!(print_query_template(&pairs), text);
    }

    #[test]
    fn query_template_rejects_bad_text() {
        for text in ["lat", "a={}", "a={b", "={b}", "a=b", "a={b}&"] {
            assert!(parse_query_template(text).is_err(), "{text}");
        }
    }

    #[test]
    fn tool_json_has_the_tools_list_shape() {
        let value = tool_json("t", "d", &obj(json!({ "type": "object" })));
        let mut keys: Vec<_> = value.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["description", "inputSchema", "name"]);
    }
}
