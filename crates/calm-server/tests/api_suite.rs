// Each module retains its former test-target namespace and helper tests.
use domain_api_suite::{common, support};
#[path = "claude_card_endpoint.rs"]
mod claude_card_endpoint;
#[path = "codex_card_endpoint.rs"]
mod codex_card_endpoint;
#[path = "domain_api_suite.rs"]
mod domain_api_suite;
#[path = "models_endpoint.rs"]
mod models_endpoint;
#[path = "plugin_suite.rs"]
mod plugin_suite;
#[path = "provider_conformance.rs"]
mod provider_conformance;
#[path = "terminal_ws_suite.rs"]
mod terminal_ws_suite;
