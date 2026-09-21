// ─── Canonical Types ──────────────────────────────────────────────────────────
//
// Internal representation for the AI Gateway.
// Deliberately NOT OpenAI's schema — these are the gateway's own canonical
// objects that can represent any provider's capabilities.
//
// Design: 80-90% normalized, 10-20% escape hatch via `provider_options`.

pub mod model;
pub mod provider;
pub mod reasoning;
pub mod request;
pub mod response;

pub use model::*;
pub use provider::*;
#[allow(unused_imports)]
pub use reasoning::*;
pub use request::*;
pub use response::*;
