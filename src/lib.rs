//! open-harness feasibility spike — library surface.
//!
//! This crate deliberately has a small, binding-friendly API surface: the
//! intent is that the same core is later exposed to Python/TypeScript via
//! `uniffi`/`napi`, so hosts embed it rather than shelling out. For the spike,
//! `oh-dispatch` (see `main.rs`) is the CLI face of the same functions.

pub mod adapters;
pub mod api;
pub mod dispatch;
pub mod event;
pub mod kind;
pub mod kinds;
pub mod manifest;
pub mod model;

pub use adapters::{Harness, Support};
pub use dispatch::{dispatch, DispatchOutcome};
pub use event::NormEvent;
pub use kind::{kind_impl, Artifact, Installability, Kind, KindId, KindPlan};
pub use manifest::{discover, LoadedCapability};
pub use model::{
    merge, negotiate, parse_decision, validate_decision, Decision, NativeResponse, Verdict,
    PROTOCOL, PROTOCOL_VERSION,
};
