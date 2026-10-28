//! Topology: the managed systems (health from their adapters) plus litellm / engine probes.
//! Port of `services/topology.py`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::config::TopologyEngine;
use crate::domain::models::{Health, SystemId, Topology, TopologyEdge, TopologyNode, now_ts};
use crate::services::common::RecoverLock as _;
use crate::services::engines::{EngineProbe, EngineState};
use crate::services::http::Http;
use crate::services::status::StatusService;

pub const TTL: f64 = 5.0;
pub const NOT_DEPLOYED: &str = " (not deployed)";
pub const FIXED_IDS: [&str; 5] = [
    "clients",
    "relay",
    "litellm",
    "switchyard",
    "dynamo-frontend",
];

pub struct TopologyService {
    pub status: Arc<StatusService>,
    http: Http,
    probe: Arc<EngineProbe>,
    litellm_url: String,
    pub engines: Vec<TopologyEngine>,
    cache: Mutex<Option<(Instant, Topology)>>,
}

fn node(
    id: &str,
    label: &str,
    kind: &str,
    health: Health,
    system: Option<SystemId>,
) -> TopologyNode {
    TopologyNode {
        id: id.to_owned(),
        label: label.to_owned(),
        kind: kind.to_owned(),
        health,
        system,
    }
}

fn edge(src: &str, dst: &str, label: &str, active: bool) -> TopologyEdge {
    TopologyEdge {
        src: src.to_owned(),
        dst: dst.to_owned(),
        label: label.to_owned(),
        active,
    }
}

impl TopologyService {
    #[must_use]
    pub fn new(
        status: Arc<StatusService>,
        http: Http,
        probe: Arc<EngineProbe>,
        litellm_url: &str,
        engines: &[TopologyEngine],
    ) -> Self {
        let mut seen: Vec<String> = FIXED_IDS.iter().map(|s| (*s).to_owned()).collect();
        let mut kept = Vec::new();
        for e in engines {
            if !seen.contains(&e.id) {
                seen.push(e.id.clone());
                kept.push(e.clone());
            }
        }
        TopologyService {
            status,
            http,
            probe,
            litellm_url: litellm_url.trim_end_matches('/').to_owned(),
            engines: kept,
            cache: Mutex::new(None),
        }
    }

    async fn litellm(&self) -> Health {
        match self
            .http
            .get(&format!("{}/health/liveliness", self.litellm_url), 2.0)
            .await
        {
            Ok(r) if r.status == 200 => Health::Up,
            Ok(_) => Health::Degraded,
            Err(_) => Health::Down,
        }
    }

    /// (health, deployed). Not deployed (no such container) is "off" for every engine, never red.
    async fn engine(&self, container: &str, opt_in: bool) -> (Health, bool) {
        match self.probe.state(container).await {
            EngineState::Running => (Health::Up, true),
            EngineState::Stopped => (if opt_in { Health::Off } else { Health::Down }, true),
            EngineState::Absent => (Health::Off, false),
            EngineState::Unknown => (Health::Unknown, true),
        }
    }

    pub async fn get(&self) -> Topology {
        if let Ok(c) = self.cache.lock_recovering()
            && let Some((t, topo)) = c.as_ref()
            && t.elapsed().as_secs_f64() < TTL
        {
            return topo.clone();
        }
        let engines_fut = futures::future::join_all(
            self.engines
                .iter()
                .map(|e| self.engine(&e.container, e.opt_in)),
        );
        let (statuses, litellm, engines) =
            tokio::join!(self.status.all(), self.litellm(), engines_fut);
        let h = |s: SystemId| {
            statuses
                .iter()
                .find(|st| st.system == s)
                .map_or(Health::Unknown, |st| st.health)
        };
        let dyn_h = h(SystemId::Dynamo);
        let sy = h(SystemId::Switchyard);
        let mut nodes = vec![
            node(
                "clients",
                "Clients (Open WebUI, opencode, agents)",
                "client",
                Health::Unknown,
                None,
            ),
            node(
                "relay",
                "NeMo Relay",
                "gateway",
                h(SystemId::Relay),
                Some(SystemId::Relay),
            ),
            node("litellm", "LiteLLM router", "router", litellm, None),
            node(
                "switchyard",
                "NeMo Switchyard",
                "router",
                sy,
                Some(SystemId::Switchyard),
            ),
            node(
                "dynamo-frontend",
                "Dynamo frontend (opt-in)",
                "engine",
                dyn_h,
                Some(SystemId::Dynamo),
            ),
        ];
        for (e, (health, deployed)) in self.engines.iter().zip(&engines) {
            let label = format!("{}{}", e.label, if *deployed { "" } else { NOT_DEPLOYED });
            nodes.push(node(&e.id, &label, "engine", *health, None));
        }
        let mut edges = vec![
            edge("clients", "litellm", "/v1", true),
            edge("clients", "relay", "agent gateway", true),
            edge("relay", "litellm", "scoped key", true),
            edge(
                "clients",
                "switchyard",
                "default",
                matches!(sy, Health::Up | Health::Degraded),
            ),
            edge("switchyard", "litellm", "targets", true),
            edge("litellm", "dynamo-frontend", "alias", dyn_h == Health::Up),
        ];
        for (e, (health, _)) in self.engines.iter().zip(&engines) {
            edges.push(edge("litellm", &e.id, "alias", *health == Health::Up));
        }
        let topo = Topology {
            nodes,
            edges,
            ts: now_ts(),
        };
        if let Ok(mut c) = self.cache.lock_recovering() {
            *c = Some((Instant::now(), topo.clone()));
        }
        topo
    }
}
