//! Turns OpenAPI 3.0 schemas into JSON Schema 2020-12, which MCP tool input schemas use.

use std::collections::BTreeSet;

use openapiv3::{ReferenceOr, Schema};
use serde::Serialize;
use serde_json::{Map, Value};

const COMPONENT_PREFIX: &str = "#/components/schemas/";

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("the reference {0} is not supported, only {COMPONENT_PREFIX}<name> is supported")]
    UnsupportedRef(String),
    #[error("the schema {0} does not exist in components")]
    MissingComponent(String),
    #[error("the schema cannot be converted to JSON")]
    Serialize(#[source] serde_json::Error),
}

/// Converts one schema. It adds the names of the component schemas it uses to `refs`.
pub fn convert(schema: &impl Serialize, refs: &mut BTreeSet<String>) -> Result<Value, SchemaError> {
    let value = serde_json::to_value(schema).map_err(SchemaError::Serialize)?;
    convert_value(value, refs)
}

/// Converts every component schema that `refs` needs, also the ones that those schemas need.
/// The result goes into `$defs`, because the refs now point to `#/$defs/<name>`.
pub fn defs(
    components: Option<&openapiv3::Components>,
    mut refs: BTreeSet<String>,
) -> Result<Map<String, Value>, SchemaError> {
    let mut defs = Map::new();
    while let Some(name) = refs.pop_first() {
        if defs.contains_key(&name) {
            continue;
        }
        let schema: &ReferenceOr<Schema> = components
            .and_then(|c| c.schemas.get(&name))
            .ok_or_else(|| SchemaError::MissingComponent(name.clone()))?;
        let mut more = BTreeSet::new();
        defs.insert(name, convert(schema, &mut more)?);
        refs.extend(more.into_iter().filter(|n| !defs.contains_key(n)));
    }
    Ok(defs)
}

fn convert_value(value: Value, refs: &mut BTreeSet<String>) -> Result<Value, SchemaError> {
    let Value::Object(mut map) = value else {
        // `true`, `false`, or a bad value. JSON Schema accepts booleans as schemas.
        return Ok(value);
    };

    if let Some(Value::String(reference)) = map.get("$ref") {
        let name = reference
            .strip_prefix(COMPONENT_PREFIX)
            .ok_or_else(|| SchemaError::UnsupportedRef(reference.clone()))?
            .to_owned();
        map.insert("$ref".into(), Value::String(format!("#/$defs/{name}")));
        refs.insert(name);
    }

    if map.remove("nullable") == Some(Value::Bool(true))
        && let Some(Value::String(kind)) = map.remove("type")
    {
        map.insert(
            "type".into(),
            Value::Array(vec![kind.into(), "null".into()]),
        );
    }
    if let Some(example) = map.remove("example") {
        map.entry("examples").or_insert(Value::Array(vec![example]));
    }
    // These keywords exist only in OpenAPI. They mean nothing to a JSON Schema validator.
    for key in ["xml", "discriminator", "externalDocs"] {
        map.remove(key);
    }

    // Only walk keywords that hold schemas. Other values, such as `enum` or `default`, are data.
    for key in ["items", "not", "additionalProperties"] {
        if let Some(inner) = map.remove(key) {
            map.insert(key.into(), convert_value(inner, refs)?);
        }
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        if let Some(Value::Array(list)) = map.remove(key) {
            let list = list
                .into_iter()
                .map(|inner| convert_value(inner, refs))
                .collect::<Result<_, _>>()?;
            map.insert(key.into(), Value::Array(list));
        }
    }
    if let Some(Value::Object(properties)) = map.remove("properties") {
        let properties = properties
            .into_iter()
            .map(|(name, inner)| Ok((name, convert_value(inner, refs)?)))
            .collect::<Result<_, _>>()?;
        map.insert("properties".into(), Value::Object(properties));
    }

    Ok(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn run(value: Value) -> (Value, BTreeSet<String>) {
        let mut refs = BTreeSet::new();
        let out = convert_value(value, &mut refs).unwrap();
        (out, refs)
    }

    #[test]
    fn rewrites_component_refs() {
        let (out, refs) =
            run(json!({ "type": "array", "items": { "$ref": "#/components/schemas/Pet" } }));
        assert_eq!(out["items"]["$ref"], "#/$defs/Pet");
        assert_eq!(refs, BTreeSet::from(["Pet".to_owned()]));
    }

    #[test]
    fn nullable_becomes_a_null_type() {
        let (out, _) = run(json!({ "type": "string", "nullable": true }));
        assert_eq!(out, json!({ "type": ["string", "null"] }));
    }

    #[test]
    fn example_becomes_examples_and_openapi_keywords_go_away() {
        let (out, _) =
            run(json!({ "type": "string", "example": "doggie", "xml": { "name": "x" } }));
        assert_eq!(out, json!({ "type": "string", "examples": ["doggie"] }));
    }

    #[test]
    fn property_names_that_look_like_keywords_stay() {
        let input = json!({ "properties": { "xml": { "type": "string" }, "nullable": { "type": "boolean" } } });
        let (out, _) = run(input.clone());
        assert_eq!(out, input);
    }

    #[test]
    fn enum_values_are_not_changed() {
        let input = json!({ "type": "object", "enum": [{ "$ref": "not-a-ref" }] });
        let (out, refs) = run(input.clone());
        assert_eq!(out, input);
        assert!(refs.is_empty());
    }

    #[test]
    fn external_refs_are_rejected() {
        let mut refs = BTreeSet::new();
        let result = convert_value(json!({ "$ref": "other.json#/Pet" }), &mut refs);
        assert!(matches!(result, Err(SchemaError::UnsupportedRef(_))));
    }
}
