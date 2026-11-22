//! Retained billing trigger names cannot submit to either engine.

#[test]
fn billing_triggers_are_inert_even_with_stale_engine_settings() {
    let directory = std::env::temp_dir().join(format!(
        "navigator-disabled-billing-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&directory).unwrap();
    for binary in [
        env!("CARGO_BIN_EXE_billing-canary-trigger"),
        env!("CARGO_BIN_EXE_billing-digest-trigger"),
        env!("CARGO_BIN_EXE_reconcile-trigger"),
        env!("CARGO_BIN_EXE_sandbox-credit-reset-trigger"),
    ] {
        for flag in ["false", "true", "typo"] {
            let mut command = std::process::Command::new(binary);
            command
                .current_dir(&directory)
                .env_clear()
                .env("NAVIGATOR_DBOS_BILLING_CANARY_ENABLED", flag)
                .env("RESTATE_INGRESS_URL", "http://127.0.0.1:1")
                .env("NAVIGATOR_DBOS_INGRESS_URL", "http://127.0.0.1:1");
            if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
                command.env("LLVM_PROFILE_FILE", profile);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
