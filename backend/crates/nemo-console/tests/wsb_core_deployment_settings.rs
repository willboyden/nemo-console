//! The workstream B-owned test of backend/tests/core/test_deployment_settings.py: the AIPerf lab helpers (install.sh,
//! mint-key.sh) are looked up in the `aiperf_dir` layout dir first, then under the repo root.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "fixtures/dcgm/support.rs"]
mod support;

use nemo_console::adapters::aiperf::{AiPerfAdapter, LAB_INSTALLER, LAB_MINT_KEY};
use support::{FakeRunner, ctx};

#[test]
fn test_lab_helpers_follow_the_layout_dir_first() {
    let tmp = tempfile::tempdir().unwrap();
    let lay = tmp.path().join("custom").join("aiperf");
    std::fs::create_dir_all(&lay).unwrap();
    std::fs::write(lay.join("install.sh"), "#!/bin/sh\n").unwrap();
    let c = ctx(
        FakeRunner::new(),
        &[],
        &[("aiperf_dir", &lay), ("repo", tmp.path())],
        &tmp.path().join("state"),
    );
    let a = AiPerfAdapter::new(c);
    assert_eq!(a.lab_file(LAB_INSTALLER), Some(lay.join("install.sh")));
    assert_eq!(a.lab_file(LAB_MINT_KEY), None); // absent everywhere: the generic text applies
}
