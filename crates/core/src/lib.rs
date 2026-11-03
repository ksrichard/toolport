//! Domain types and the OpenAPI to tool conversion. This crate does no I/O.

pub mod convert;
pub mod inputs;
pub mod request;
mod schema;
pub mod server_name;
pub mod spec;
pub mod tool;

pub use convert::{Conversion, SkipReason, SkippedOperation, convert};
pub use inputs::{
    InputRow, InputType, TemplateError, apply_rows, parse_query_template, print_query_template,
    rows_from_schema, schema_from_rows, tool_json,
};
pub use request::{ArgumentError, UpstreamRequest, build_request};
pub use spec::{SpecError, parse_spec};
pub use tool::{
    BODY_ARGUMENT, BodyDef, HttpMethod, ParamDef, ParamLocation, RequestTemplate, ToolDef,
};
