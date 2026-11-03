use serde::Deserialize;
use serde_json::{Map, Value};
use toolport_core::{InputRow, schema_from_rows};

#[derive(Deserialize)]
struct Case {
    name: String,
    rows: Vec<InputRow>,
    body: Option<Value>,
    schema: Map<String, Value>,
}

#[test]
fn preview_fixtures_match() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/preview.json")).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let schema = schema_from_rows(&case.rows, case.body.as_ref());
        assert_eq!(schema, case.schema, "case {}", case.name);
    }
}
