//! Adapter registry. Owned by core. Every module is pre-registered here, so a workstream only
//! ever edits its own `<module>.rs` (+ `<module>/`), never this file. Port of `adapters/__init__.py`.

pub mod base;
pub mod switchyard_capable;

// workstream A
pub mod dynamo;
pub mod relay;
pub mod switchyard;
// workstream B
pub mod aiperf;
pub mod dcgm;
pub mod guardrails;
pub mod openshell;
// workstream C
pub mod datadesigner;
pub mod evaluator;
pub mod skillevaluator;
// workstream D
pub mod modelopt;

use crate::adapters::base::{AdapterContext, AdapterError, AdapterRef};
use crate::config::Settings;
use crate::domain::models::SystemId;

/// Registry order (`SYSTEM_IDS`).
pub const SYSTEM_IDS: [SystemId; 11] = SystemId::ALL;

/// The registry's build function type (tests may substitute one, like Python monkeypatching `_REGISTRY`).
pub type BuildFn = fn(SystemId, &AdapterContext, &Settings) -> Result<AdapterRef, AdapterError>;

/// `build_adapter(system, ctx)` + the container's `_configure_<system>` hook in one call.
pub fn build_adapter(
    system: SystemId,
    ctx: &AdapterContext,
    cfg: &Settings,
) -> Result<AdapterRef, AdapterError> {
    match system {
        SystemId::Switchyard => switchyard::build(ctx, cfg),
        SystemId::Relay => relay::build(ctx, cfg),
        SystemId::Dynamo => dynamo::build(ctx, cfg),
        SystemId::Skillevaluator => skillevaluator::build(ctx, cfg),
        SystemId::Datadesigner => datadesigner::build(ctx, cfg),
        SystemId::Openshell => openshell::build(ctx, cfg),
        SystemId::Dcgm => dcgm::build(ctx, cfg),
        SystemId::Aiperf => aiperf::build(ctx, cfg),
        SystemId::Guardrails => guardrails::build(ctx, cfg),
        SystemId::Evaluator => evaluator::build(ctx, cfg),
        SystemId::Modelopt => modelopt::build(ctx, cfg),
    }
}
