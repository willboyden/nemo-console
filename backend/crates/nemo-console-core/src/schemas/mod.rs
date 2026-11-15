//! The config-file schemas the console checks itself (no tool needed), one module per system. Each was moved
//! unchanged from the backend adapter that owns the file; the adapter re-exports it and adds what needs a tool or the
//! host (Switchyard's dry run, the evaluator wrapper's own validator and prefetch state). [`crate::check`] composes
//! them the way the backend's validate route does.

pub mod aiperf;
pub mod evaluator;
pub mod modelopt;
pub mod relay;
pub mod switchyard;
