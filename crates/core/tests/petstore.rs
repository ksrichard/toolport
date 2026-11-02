use serde_json::{Value, json};
use toolport_core::{
    BodyDef, HttpMethod, ParamLocation, SkipReason, SpecError, ToolDef, convert, parse_spec,
};

const PETSTORE: &str = include_str!("fixtures/petstore.json");

fn petstore_tools() -> Vec<ToolDef> {
    let conversion = convert(&parse_spec(PETSTORE).unwrap());
    assert!(conversion.skipped.is_empty(), "{:?}", conversion.skipped);
    conversion.tools
}

fn tool<'a>(tools: &'a [ToolDef], name: &str) -> &'a ToolDef {
    tools
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

#[test]
fn makes_one_tool_per_petstore_operation() {
    let names: Vec<_> = petstore_tools().into_iter().map(|t| t.name).collect();
    assert_eq!(names.len(), 19);
    assert_eq!(names[..3], ["updatePet", "addPet", "findPetsByStatus"]);
    for name in [
        "addPet",
        "getPetById",
        "findPetsByStatus",
        "deletePet",
        "loginUser",
        "uploadFile",
    ] {
        assert!(names.iter().any(|n| n == name), "missing {name}");
    }
}

#[test]
fn path_parameter_becomes_a_required_property() {
    let tools = petstore_tools();
    let tool = tool(&tools, "getPetById");
    assert_eq!(tool.request.method, HttpMethod::Get);
    assert_eq!(tool.request.path, "/pet/{petId}");
    assert_eq!(tool.input_schema["required"], json!(["petId"]));
    assert_eq!(tool.input_schema["properties"]["petId"]["type"], "integer");
    assert!(tool.description.starts_with("Find pet by ID."));
}

#[test]
fn json_body_uses_defs_for_component_schemas() {
    let tools = petstore_tools();
    let tool = tool(&tools, "addPet");
    let schema = Value::Object(tool.input_schema.clone());
    assert_eq!(schema["properties"]["body"]["$ref"], "#/$defs/Pet");
    assert_eq!(schema["required"], json!(["body"]));
    // Pet uses Category and Tag, so they must be in $defs too.
    let defs = schema["$defs"].as_object().unwrap();
    let mut def_names: Vec<_> = defs.keys().map(String::as_str).collect();
    def_names.sort();
    assert_eq!(def_names, ["Category", "Pet", "Tag"]);
    assert_eq!(
        defs["Pet"]["properties"]["tags"]["items"]["$ref"],
        "#/$defs/Tag"
    );
    assert_eq!(
        tool.request.body,
        Some(BodyDef {
            content_type: "application/json".into(),
            required: true
        })
    );
}

#[test]
fn header_and_query_parameters_keep_their_location() {
    let tools = petstore_tools();
    let delete = tool(&tools, "deletePet");
    let locations: Vec<_> = delete
        .request
        .params
        .iter()
        .map(|p| (p.name.as_str(), p.location))
        .collect();
    assert_eq!(
        locations,
        [
            ("api_key", ParamLocation::Header),
            ("petId", ParamLocation::Path)
        ]
    );
    assert!(
        delete.input_schema["properties"]["api_key"]
            .get("description")
            .is_none()
    );

    let find = tool(&tools, "findPetsByStatus");
    assert_eq!(find.request.params[0].location, ParamLocation::Query);
    assert_eq!(
        find.input_schema["properties"]["status"]["enum"],
        json!(["available", "pending", "sold"])
    );
}

#[test]
fn optional_binary_body_is_left_out() {
    let tools = petstore_tools();
    let upload = tool(&tools, "uploadFile");
    assert_eq!(upload.request.body, None);
    assert!(upload.input_schema["properties"].get("body").is_none());
}

#[test]
fn swagger_2_is_rejected() {
    let result = parse_spec(r#"{ "swagger": "2.0", "info": {}, "paths": {} }"#);
    assert!(matches!(result, Err(SpecError::Swagger2)));
}

#[test]
fn openapi_3_1_is_rejected() {
    let result = parse_spec(r#"{ "openapi": "3.1.0", "info": {}, "paths": {} }"#);
    assert!(matches!(result, Err(SpecError::UnsupportedVersion(v)) if v == "3.1.0"));
}

#[test]
fn text_that_is_not_json_is_rejected() {
    assert!(matches!(
        parse_spec("openapi: 3.0.0"),
        Err(SpecError::NotJson(_))
    ));
}

fn spec(paths: Value) -> String {
    json!({
        "openapi": "3.0.3",
        "info": { "title": "t", "version": "1" },
        "paths": paths,
    })
    .to_string()
}

#[test]
fn bad_operations_are_skipped_and_the_rest_stay() {
    let text = spec(json!({
        "/ok": { "get": { "operationId": "ok", "responses": {} } },
        "/same": { "get": { "operationId": "ok", "responses": {} } },
        "/items/{id}": { "get": { "operationId": "noParam", "responses": {} } },
        "/xml": { "post": {
            "operationId": "xmlOnly",
            "requestBody": { "required": true, "content": { "application/xml": {} } },
            "responses": {},
        } },
    }));
    let conversion = convert(&parse_spec(&text).unwrap());
    let names: Vec<_> = conversion.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["ok"]);

    let reason = |path: &str| {
        let skipped = conversion.skipped.iter().find(|s| s.path == path);
        &skipped
            .unwrap_or_else(|| panic!("{path} was not skipped"))
            .reason
    };
    assert!(matches!(reason("/same"), SkipReason::DuplicateToolName(n) if n == "ok"));
    assert!(matches!(reason("/items/{id}"), SkipReason::UndefinedPathParameter(n) if n == "id"));
    assert!(matches!(reason("/xml"), SkipReason::UnsupportedBody(t) if t == "application/xml"));
}

#[test]
fn operation_parameter_replaces_path_level_parameter() {
    let text = spec(json!({
        "/items": {
            "parameters": [{ "name": "limit", "in": "query", "schema": { "type": "string" } }],
            "get": {
                "parameters": [{ "name": "limit", "in": "query", "required": true, "schema": { "type": "integer" } }],
                "responses": {},
            },
        },
    }));
    let conversion = convert(&parse_spec(&text).unwrap());
    let tool = &conversion.tools[0];
    assert_eq!(tool.name, "get_items");
    assert_eq!(tool.request.params.len(), 1);
    assert_eq!(tool.input_schema["properties"]["limit"]["type"], "integer");
    assert_eq!(tool.input_schema["required"], json!(["limit"]));
}
