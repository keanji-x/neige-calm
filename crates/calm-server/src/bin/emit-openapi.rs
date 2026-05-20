//! Build-time helper for the frontend codegen pipeline.
//!
//! Prints the aggregated OpenAPI document to stdout so `web/` can pipe it
//! into `openapi.json` without spinning up the actual HTTP server. See
//! `web/package.json`'s `gen:api` script.

use calm_server::openapi::ApiDoc;
use utoipa::OpenApi;

fn main() {
    println!("{}", ApiDoc::openapi().to_pretty_json().unwrap());
}
