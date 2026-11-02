//! Domain types and the OpenAPI to tool conversion. This crate does no I/O.

pub mod convert;
pub mod request;
mod schema;
pub mod server_name;
pub mod spec;
pub mod tool;

pub use convert::{Conversion, SkipReason, SkippedOperation, convert};
pub use request::{ArgumentError, UpstreamRequest, build_request};
pub use spec::{SpecError, parse_spec};
pub use tool::{
    BODY_ARGUMENT, BodyDef, HttpMethod, ParamDef, ParamLocation, RequestTemplate, ToolDef,
};
