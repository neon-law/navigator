//! A DBOS-owned schedule never needs or contacts the Restate ingress.

fn trigger(flag: &str) -> std::process::Output {
    let directory =
        std::env::temp_dir().join(format!("navigator-canary-trigger-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_billing-canary-trigger"));
    command
        .current_dir(&directory)
        .env_clear()
        .env("NAVIGATOR_DBOS_BILLING_CANARY_ENABLED", flag);
    // Keep instrumentation receipts while excluding application credentials.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let result = command.output();
    std::fs::remove_dir_all(&directory).unwrap();
    result.unwrap()
}

#[test]
fn dbos_owned_canary_trigger_exits_without_a_restate_configuration() {
    let output = trigger("true");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn invalid_ownership_refuses_to_start_in_either_engine() {
    let output = trigger("typo");
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("NAVIGATOR_DBOS_BILLING_CANARY_ENABLED"));
    assert!(!error.contains("RESTATE_INGRESS_URL must be set"));
}
