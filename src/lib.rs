//! open-harness — library surface.
//!
//! This crate deliberately has a small, binding-friendly API surface: the same
//! core is exposed to Python/TypeScript via `uniffi`/`napi`, so hosts embed it
//! rather than shelling out. [`api`] is the stable facade consumers depend on;
//! `oh` (see `main.rs`) is the CLI face of the same functions.

pub mod adapters;
pub mod api;
pub mod capture;
pub mod config;
pub mod deps;
pub mod dispatch;
pub mod event;
pub mod kind;
pub mod kinds;
pub mod manifest;
pub mod matrix;
pub mod mcp;
pub mod model;
pub mod profile;
pub mod runtime;
pub mod scaffold;
pub mod sync;
pub mod tools;
pub mod trust;
pub mod yaml;

// Generates the uniffi scaffolding for the language bindings (feature-gated so
// the default build never pulls uniffi).
#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();

pub use adapters::{Harness, Support};
pub use deps::{Dependencies, Dependency, Relation, Requirement, Version};
pub use dispatch::{dispatch, dispatch_with_limits, DispatchOutcome};
pub use event::NormEvent;
pub use kind::{kind_impl, Artifact, Installability, Kind, KindId, KindPlan};
pub use manifest::{discover, LoadedCapability};
pub use model::{
    merge, negotiate, parse_decision, validate_decision, Decision, NativeResponse, Verdict,
    PROTOCOL, PROTOCOL_VERSION,
};
pub use profile::{resolve, Lock, Profile, Resolved, Source};
pub use runtime::{RunError, RunLimits};
pub use sync::{
    apply, check, plan_sync, ApplyReport, ChangeAction, DesiredFile, DriftKind, DriftReport,
    SyncPlan,
};
pub use trust::{capability_digest, sign, verify, Keyfile, Signature, TrustStore, Verification};
