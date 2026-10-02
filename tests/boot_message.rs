//! What an operator reads when the boot refuses.
//!
//! `main` returns `Box<dyn Error>`, and Rust prints that with DEBUG. A
//! `BootError` handed to it with a bare `?` therefore prints its variant name —
//! `ObsoleteRequireTls`, `MigrationLockWait { .. }` — and drops the sentence
//! that names the knob and says what to set, which is the whole of what
//! ADR-0569 asks a refusal to carry. `boot`'s unit tests prove the sentences
//! exist; only running the BINARY proves they reach the operator.
//!
//! No engine is needed: every case here is refused before anything connects.

use std::process::Command;

/// Run the real binary with exactly `vars` in its environment, and return what
/// it printed to stderr. It must exit non-zero: these are all refusals.
fn refusal(vars: &[(&str, &str)]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_yadgar-project-db"))
        .env_clear()
        .envs(vars.iter().copied())
        .output()
        .expect("the test rig could not start the binary");
    assert!(
        !out.status.success(),
        "a refused boot must exit non-zero: {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // PLAIN DISPLAY, NOT DEBUG OF A STRING. Rust prints `main`'s `Err` with
    // Debug, so even a refusal converted to its sentence arrived wrapped in
    // quotes with every inner quote escaped: `Error: "… is \"0\" …"`.
    let line = stderr
        .lines()
        .rfind(|l| l.starts_with("Error: "))
        .unwrap_or_else(|| panic!("no `Error: ` line on stderr: {stderr}"));
    assert!(
        !line.starts_with("Error: \"") && !line.contains("\\\""),
        "the refusal was printed as a Debug string: {line}"
    );
    stderr
}

/// An empty environment: `pool_config`'s first required knob refuses. This
/// module never read `DB_REQUIRE_TLS`, so there is no obsolete-key arm to reach.
#[test]
fn an_absent_knob_is_refused_with_its_sentence() {
    let stderr = refusal(&[]);
    assert!(
        stderr.contains("DB_HOST is NOT SET"),
        "the refusal must name the knob: {stderr}"
    );
    assert!(
        !stderr.contains("MissingKnob("),
        "the operator got the Debug variant, not the sentence: {stderr}"
    );
}

/// The migration lock's wait is read right after the pool's knobs, so a full
/// pool environment plus an unusable wait reaches that refusal and nothing
/// later. Its variant carries fields: Debug would print `MigrationLockWait {
/// value: "0", .. }` and name neither the variable nor the chart key.
#[test]
fn an_unusable_migration_lock_wait_is_refused_naming_the_variable_and_the_chart_key() {
    let stderr = refusal(&[
        ("DB_HOST", "engine.example.invalid"),
        ("DB_PORT", "13306"),
        ("DB_NAME", "project_fixture"),
        ("DB_USER", "project_fixture_user"),
        ("DB_MAX_CONNECTIONS", "4"),
        ("REPLICAS", "3"),
        ("DB_ENGINE_MAX_CONNECTIONS", "200"),
        ("DB_SSL_MODE", "verify-identity"),
        ("DB_MIGRATION_LOCK_TIMEOUT_SECONDS", "0"),
    ]);
    assert!(
        stderr.contains("DB_MIGRATION_LOCK_TIMEOUT_SECONDS is"),
        "the refusal must name the variable: {stderr}"
    );
    assert!(
        stderr.contains("database.migrationLockTimeoutSeconds"),
        "the refusal must name the chart key: {stderr}"
    );
    assert!(
        !stderr.contains("MigrationLockWait"),
        "the operator got the Debug variant, not the sentence: {stderr}"
    );
}
