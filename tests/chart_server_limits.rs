//! The chart's `server.limits` defaults (ADR-1006) sit inside the ranges the
//! library enforces, and each one is pinned by a literal.
//!
//! `yadgar_lifecycle::serve_tls::ServerLimits` has no default (ADR-0569,
//! ADR-0705): this chart's `values.yaml` is the one place the four numbers are
//! stated, and the binary refuses to boot on a value outside the library's range.
//! `scripts/tests/test_server_limits.py` holds the render half (the schema and the
//! keepalive render check). This file holds the half that needs the library: a
//! bump of the `yadgar-lifecycle` pin that moves a range turns it red here
//! instead of letting a default the binary would now refuse ship in the chart.
//!
//! **EVERY NUMBER IS PINNED BY A LITERAL (ADR-0599).** The defaults are checked
//! against ADR-1006's literals and the ranges against literals too, so a test
//! that routed both sides through the library's constants could not stay green
//! through a change to them.

use std::ops::RangeInclusive;
use std::path::PathBuf;

use yadgar_project_db::boot::ServerLimits;

fn read(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|why| panic!("{} must be readable: {why}", path.display()))
}

/// The value of `leaf` inside the `server:` / `limits:` block of `values.yaml`.
///
/// ANCHORED to the two-level block, because the same leaf names are quoted in the
/// comments around them, and a search that merely CONTAINS a name reads a number
/// out of English. A missing leaf panics rather than defaulting: a parse that
/// answers a fallback is a test that passes after somebody deletes the field.
fn chart_limit(leaf: &str) -> u64 {
    let values = read("chart/values.yaml");
    let mut in_server = false;
    let mut in_limits = false;
    let mut found = Vec::new();
    for line in values.lines() {
        if !line.starts_with(' ') && !line.starts_with('#') && !line.is_empty() {
            in_server = line.starts_with("server:");
            in_limits = false;
        } else if in_server && line.starts_with("  ") && !line.starts_with("   ") {
            in_limits = line.trim_start().starts_with("limits:");
        } else if in_limits {
            if let Some(rest) = line.strip_prefix("    ").and_then(|l| l.strip_prefix(leaf)) {
                if let Some(rest) = rest.strip_prefix(':') {
                    found.push(
                        rest.split('#')
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                    );
                }
            }
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one `server.limits.{leaf}` in chart/values.yaml, found {found:?}"
    );
    found[0]
        .parse()
        .unwrap_or_else(|why| panic!("`{leaf}: {}` must be a whole number: {why}", found[0]))
}

/// (chart leaf, ADR-1006 default, literal minimum, literal maximum, library range).
fn limits() -> [(&'static str, u64, u64, u64, RangeInclusive<u64>); 4] {
    [
        (
            "maxConcurrentStreams",
            256,
            1,
            10_000,
            ServerLimits::MAX_CONCURRENT_STREAMS,
        ),
        (
            "keepaliveIntervalSeconds",
            30,
            1,
            3_600,
            ServerLimits::KEEPALIVE_INTERVAL_SECS,
        ),
        (
            "keepaliveTimeoutSeconds",
            10,
            1,
            600,
            ServerLimits::KEEPALIVE_TIMEOUT_SECS,
        ),
        (
            "maxHeaderListBytes",
            16_384,
            1_024,
            1_048_576,
            ServerLimits::MAX_HEADER_LIST_BYTES,
        ),
    ]
}

#[test]
fn the_charts_defaults_are_the_adr_1006_numbers() {
    for (leaf, default, _, _, _) in limits() {
        assert_eq!(
            chart_limit(leaf),
            default,
            "`server.limits.{leaf}` moved from ADR-1006's default; change the ADR first"
        );
    }
}

#[test]
fn the_charts_defaults_sit_inside_the_ranges_the_library_enforces() {
    for (leaf, _, _, _, range) in limits() {
        let value = chart_limit(leaf);
        assert!(
            range.contains(&value),
            "`server.limits.{leaf}: {value}` is outside {range:?}, so the binary would refuse to boot"
        );
    }
}

#[test]
fn the_library_ranges_are_the_ones_the_schema_states() {
    let schema: serde_json::Value =
        serde_json::from_str(&read("chart/values.schema.json")).expect("the schema is JSON");
    let block = &schema["properties"]["server"]["properties"]["limits"];
    for (leaf, _, minimum, maximum, range) in limits() {
        assert_eq!(
            (*range.start(), *range.end()),
            (minimum, maximum),
            "the library's range for `{leaf}` moved; the schema and this file move with it"
        );
        let property = &block["properties"][leaf];
        assert_eq!(property["type"], "integer", "{leaf}");
        assert_eq!(property["minimum"], minimum, "{leaf}");
        assert_eq!(property["maximum"], maximum, "{leaf}");
        assert!(
            block["required"]
                .as_array()
                .expect("`server.limits.required` is a list")
                .iter()
                .any(|name| name == leaf),
            "`{leaf}` must be required in the schema"
        );
    }
}

#[test]
fn the_charts_keepalive_timeout_is_below_its_interval() {
    assert!(
        chart_limit("keepaliveTimeoutSeconds") < chart_limit("keepaliveIntervalSeconds"),
        "the library refuses a timeout that is not below the interval"
    );
}
