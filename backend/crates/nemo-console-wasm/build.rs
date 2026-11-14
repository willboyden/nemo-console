//! Link the WebAssembly module with an 8 MiB stack. The TOML parser bounds nesting by Python's recursion budget
//! (`pytoml::NESTING_FRAME_BUDGET`), not by the stack; natively a deep document is parsed on a 16 MiB thread, and
//! wasm32 has no threads, so the module's own stack must hold the budget (the default is 1 MiB). The shared test
//! vectors at the budget's edge (crates/nemo-console-core/tests/vectors) prove both builds give the same answer.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-link-arg=-zstack-size=8388608");
    }
}
