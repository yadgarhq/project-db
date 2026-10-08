//! The pool's acquire timeout must fire BEFORE the caller's own deadline does.
//!
//! `database.acquireTimeoutSeconds` bounds how long `store`'s pool waits for a
//! free connection (ledger C-DB2, ADR-0837). This module has no dial of its
//! own — it is a `-db` twin, never a dialler — but every caller that reaches it
//! over gRPC dials through `yadgar-dial`, whose own `REQUEST_TIMEOUT` is the
//! WHOLE of a caller's request budget. If this pool's acquire timeout could
//! reach or exceed that budget, the acquire would never be the error a caller
//! sees FIRST: the caller's own timeout would fire first, or at the same
//! instant, and the operator reading it would learn nothing about which layer
//! actually ran out of room.
//!
//! **EVERY NUMBER IS PINNED BY A LITERAL (ADR-0599), THE SAME DISCIPLINE
//! `gateway`'s `chart_grace_period.rs` HOLDS.** A bound another component's
//! configuration must respect is asserted against the literal, never only
//! through the constant that carries it. So `chart/values.schema.json`'s `29`
//! and `chart/values.yaml`'s `25` are read out of the files and checked against
//! literals, `yadgar_dial::default_request_timeout()` is checked against `30`,
//! and the three-way relation is asserted last as what must survive a
//! deliberate change to any of them.
//!
//! **THIS TEST REDS WHEN THIS MODULE BUMPS ITS `yadgar-dial` PIN, not when
//! somebody edits `dial`'s `main`.** That is the correct moment: the chart
//! must agree with the DIALLER EVERY CALLER ACTUALLY LINKS, and `dial` cannot
//! hold this check itself — its own `REQUEST_TIMEOUT` doc says so, because the
//! `-db` charts live in seven other repositories it cannot read.
//!
//! `yadgar-dial` is a DEV-ONLY dependency here: nothing this binary ships
//! dials anything, so this is the one place in this repository that reads it.

use std::path::{Path, PathBuf};

/// The value of a scalar NESTED one level under a top-level key in
/// `chart/values.yaml` — `database.acquireTimeoutSeconds`, in this file's one
/// use.
///
/// **ANCHORED AT EXACTLY TWO SPACES OF INDENT, and that is not a detail.**
/// Every other key this chart renders under `database:` sits at the same
/// depth, and the key is also NAMED in the prose comment above its own
/// definition — a search that merely CONTAINS the key finds a comment first
/// and reads a number out of English. A two-space-anchored `strip_prefix`
/// cannot match a `#`-led comment line or a key nested one level deeper.
///
/// **A MISSING KEY PANICS RATHER THAN DEFAULTING**, the same rule
/// `gateway`'s `chart_scalar` holds: a parse that answers a fallback when it
/// finds nothing is a test that passes after somebody deletes the field.
fn database_scalar(key: &str) -> u64 {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("chart/values.yaml");
    let values = std::fs::read_to_string(&path)
        .unwrap_or_else(|why| panic!("{} must be readable: {why}", path.display()));

    let prefix = format!("  {key}:");
    let found: Vec<&str> = values
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .map(|rest| rest.split('#').next().unwrap_or_default().trim())
        .collect();

    assert_eq!(
        found.len(),
        1,
        "expected exactly one `{prefix}` nested under `database:` in {}, found {}: {found:?} \
         — a duplicate key means Helm reads the last one and this test would read the first",
        path.display(),
        found.len()
    );

    found[0].parse().unwrap_or_else(|why| {
        panic!(
            "`{key}: {}` in {} must be a whole number of seconds: {why}",
            found[0],
            path.display()
        )
    })
}

/// The JSON Schema `maximum` bound on a leaf nested under `database` in
/// `chart/values.schema.json`.
///
/// Parsed rather than string-scanned, because JSON SCHEMA NESTING IS NOT
/// LINE-SHAPED the way `chart/values.yaml`'s indentation is: a pattern
/// anchored on a column cannot tell this leaf's `"maximum"` from
/// `migrationLockTimeoutSeconds`'s, which sits at the exact same depth a few
/// lines away.
fn schema_maximum(leaf: &str) -> u64 {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("chart/values.schema.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|why| panic!("{} must be readable: {why}", path.display()));
    let schema: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|why| panic!("{} must be valid JSON: {why}", path.display()));

    schema
        .pointer(&format!("/properties/database/properties/{leaf}/maximum"))
        .unwrap_or_else(|| {
            panic!(
                "{} declares no maximum at /properties/database/properties/{leaf}",
                path.display()
            )
        })
        .as_u64()
        .unwrap_or_else(|| {
            panic!(
                "{leaf}'s maximum in {} is not a whole number",
                path.display()
            )
        })
}

/// The shipped value and the schema bound are read from the CHART'S OWN
/// files; the deadline is read from the CRATE this module's callers actually
/// dial through. All three are then pinned against literals, so a change to
/// any one of them without the others reds here rather than at a caller, in
/// production, as an acquire that loses the race against its own request.
#[test]
fn the_acquire_timeout_fires_before_a_callers_own_request_deadline() {
    let shipped = database_scalar("acquireTimeoutSeconds");
    let maximum = schema_maximum("acquireTimeoutSeconds");
    let deadline = yadgar_dial::default_request_timeout().as_secs();

    assert_eq!(
        shipped, 25,
        "chart/values.yaml's shipped database.acquireTimeoutSeconds moved; it is the value \
         every rendered deployment gets today, and the relation below is asserted against it"
    );
    assert_eq!(
        maximum, 29,
        "chart/values.schema.json's maximum on database.acquireTimeoutSeconds moved; it exists \
         to stay strictly below the dial's own request deadline"
    );
    assert_eq!(
        deadline, 30,
        "yadgar_dial::default_request_timeout() moved; chart/values.schema.json's maximum of \
         29 on database.acquireTimeoutSeconds was pinned strictly below the OLD value of 30 — \
         re-derive the chart's bound and change both together"
    );

    assert!(
        shipped <= maximum,
        "the shipped acquireTimeoutSeconds ({shipped}) exceeds the schema's own maximum \
         ({maximum}); every rendered deployment would already fail validation"
    );
    assert!(
        maximum < deadline,
        "database.acquireTimeoutSeconds's maximum ({maximum}) no longer sits strictly below \
         yadgar_dial::default_request_timeout() ({deadline}s): an operator setting the chart \
         value right up to that maximum would make this pool's own acquire time out at or \
         after the caller's whole request deadline, so a caller never sees which layer actually \
         ran out of room"
    );
}

/// A smoke check that [`database_scalar`] and [`schema_maximum`] are not
/// quietly reading the wrong file: `chart/values.yaml` and
/// `chart/values.schema.json` both exist at the paths this test assumes.
#[test]
fn the_chart_files_this_test_reads_exist() {
    for relative in ["chart/values.yaml", "chart/values.schema.json"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        assert!(path.is_file(), "{} must exist", path.display());
    }
}
