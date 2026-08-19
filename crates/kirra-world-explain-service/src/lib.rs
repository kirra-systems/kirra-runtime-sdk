//! **The World-side explanation producer.**
//!
//! One job: expose bounded, presentation-safe [`ExplanationArtifact`] responses.
//! Tier 4 box 3a is the core (`explain`), 3b is the transport over it.
//!
//! [`ExplanationArtifact`]: kirra_explain_types::ExplanationArtifact

pub mod explain;
pub mod labels;

pub use explain::{explain_current_subject, ExplainError};
pub use labels::StoreLabels;
