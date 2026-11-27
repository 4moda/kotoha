//! Kotoha Core library structured using Clean / Hexagonal (Onion) Architecture.
//!
//! Layers (dependencies point strictly inward):
//! - [`domain`]: pure entities, value objects, domain rules, and port traits.
//! - [`application`]: pipeline orchestration, session ownership, and runtime translator swapping.
//! - [`infrastructure`]: OS-native IPC server, audio preprocessor, and mock/Phase 1 adapters.

pub mod application;
pub mod domain;
pub mod infrastructure;
