//! The hook kind — the first kind behind the `Kind` abstraction.
//!
//! It reuses the per-harness hook adapter (`adapters::Harness`) unchanged: the
//! event→native mapping, the deny-signal encoding, and the registration
//! generation all still live there. This module only expresses "hook" in terms
//! of the generic [`Kind`] trait so the rest of the system is kind-agnostic. The
//! hook *runtime* stays in `dispatch.rs`.

use crate::adapters::{Harness, Support};
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;

pub struct HookKind;

impl Kind for HookKind {
    fn id(&self) -> KindId {
        KindId::Hook
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let mut fanout_targets = 0usize;
        let mut required_missing = Vec::new();
        let mut optional_missing = Vec::new();

        for b in &cap.manifest.events {
            let ev = b.event();
            match harness.support(&ev) {
                Support::Native(_, _) => {}
                Support::Fanout(list) => fanout_targets += list.len(),
                Support::Unsupported(reason) => {
                    if b.required {
                        required_missing.push(format!("{} ({reason})", ev.id()));
                    } else {
                        optional_missing.push(ev.id());
                    }
                }
            }
        }

        let installability = if !required_missing.is_empty() {
            Installability::Unsupported(format!(
                "missing required event(s): {}",
                required_missing.join(", ")
            ))
        } else if fanout_targets > 0 || !optional_missing.is_empty() {
            let mut parts = Vec::new();
            if fanout_targets > 0 {
                parts.push(format!("fan-out to {fanout_targets} native events"));
            }
            if !optional_missing.is_empty() {
                parts.push(format!("skipped optional: {}", optional_missing.join(", ")));
            }
            Installability::Degraded(parts.join("; "))
        } else {
            Installability::Clean
        };

        let artifacts = match installability {
            Installability::Unsupported(_) => Vec::new(),
            _ => {
                let events: Vec<_> = cap.manifest.events.iter().map(|b| b.event()).collect();
                vec![Artifact::Registration {
                    text: harness.emit_registration(&events, &cap.manifest.id),
                }]
            }
        };

        let notes = harness
            .platform_note()
            .map(|n| vec![n.to_string()])
            .unwrap_or_default();

        KindPlan {
            installability,
            artifacts,
            notes,
        }
    }
}
