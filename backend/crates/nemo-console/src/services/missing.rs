//! Stand-in for an adapter that failed to construct: health=unknown + a Problem, never a crash.
//! Port of `services/missing.py`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::adapters::base::{AdapterError, EventStream, SystemAdapter};
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, ConfigFile, ConfigFileSpec, Health, MetricsSnapshot,
    Problem, Severity, SystemEvent, SystemId, SystemStatus, ValidationResult, now_ts,
};
use crate::services::pyfmt::char_prefix;

pub struct MissingAdapter {
    pub system: SystemId,
    pub reason: String,
}

#[async_trait]
impl SystemAdapter for MissingAdapter {
    fn id(&self) -> SystemId {
        self.system
    }

    fn label(&self) -> String {
        self.system.capitalized()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        Ok(SystemStatus {
            system: self.system,
            health: Health::Unknown,
            version: None,
            summary: "adapter not available".to_owned(),
            endpoints: vec![],
            components: vec![],
            problems: vec![Problem::new(
                Severity::Error,
                "console.adapter.missing",
                format!(
                    "adapter for {} could not be loaded: {}",
                    self.system,
                    char_prefix(&self.reason, 200)
                ),
                Some("reinstall nemo-console (uv sync --frozen) and restart it".to_owned()),
            )],
            checked_at: now_ts(),
        })
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        Ok(MetricsSnapshot {
            system: self.system,
            ts: now_ts(),
            available: false,
            points: vec![],
            error: Some("adapter not available".to_owned()),
        })
    }

    async fn recent_events(&self, _limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        Ok(Vec::new())
    }

    fn follow_events(self: Arc<Self>, _since_ts: Option<f64>) -> EventStream {
        Box::pin(futures::stream::empty())
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        Ok(Vec::new())
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }

    async fn validate_config(
        &self,
        file_id: &str,
        _content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }

    async fn post_apply(&self, file_id: &str, _restart: bool) -> Result<ApplyResult, AdapterError> {
        Err(AdapterError::UnknownId(file_id.to_owned()))
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        Ok(Vec::new())
    }

    async fn run_action(
        &self,
        action_id: &str,
        _params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        Err(AdapterError::UnknownId(action_id.to_owned()))
    }
}
