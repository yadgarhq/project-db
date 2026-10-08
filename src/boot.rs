//! What `main` decides before it opens anything — in a place a test can reach.
//!
//! `main` is a binary entry point, so nothing in it is reachable from a test.
//! That is fine for wiring and not fine for decisions, and two decisions here
//! are exactly the kind that fail silently: which transport mode the connections
//! to the engine use, and which transport this module SERVES on. Both live in
//! this module, and both have a test.
//!
//! **The connection options are the point.** D7's capability probe runs on a
//! connection of its own, before the pool exists. A binary that builds that
//! connection by `format!`-ing a DSN has no `ssl-mode` in it, so it inherits
//! sqlx's default — `Preferred`, which sqlx documents as falling back to an
//! unencrypted connection when an encrypted one cannot be established — while
//! the pool beside it is on `Required`. Two code paths that must agree about TLS
//! is the bug; one path is the fix, and [`probe_connect_options`] is the seam
//! that keeps it one. `task-db` shipped that defect and this module starts
//! without it.
//!
//! **The listener is the same argument, one hop further out.** `DB_SSL_MODE`
//! decides how this module reaches its engine; [`ServerTls`] decides what
//! `project` gets when it reaches this module. NEITHER DEFAULTS ANY MORE:
//! `DB_SSL_MODE` is required and refuses the boot when unset (ADR-0569), and the
//! listener's transport is two required variables, the switch and the
//! client-auth mode (ADR-0845, ADR-0854). Both refuse rather than downgrade
//! when asked for something they cannot deliver, and neither names an issuer, a
//! CRD or a mesh (D80) — two switches and file paths are the whole of the
//! configuration.
//!
//! **THERE IS NO `DB_REQUIRE_TLS` REFUSAL HERE, unlike in `task-db` and
//! `iam-db`.** That refusal exists in those modules because they once READ the
//! key, so an operator who set it was owed a boot failure rather than a silently
//! withdrawn guarantee. This module has never read it, and adding a refusal for
//! a key it never had would be a deprecation notice for a history it does not
//! have.

use std::path::PathBuf;
use std::time::Duration;

use sqlx::mysql::MySqlConnectOptions;
use yadgar_store::credentials::Secret;
// NO `DEFAULT_SSL_MODE`. It was the fallback `pool_config` handed
// `parse_ssl_mode` when DB_SSL_MODE was unset, and under ADR-0569 there is no
// such position to hand anything to. The constant still exists in
// `yadgar-store` and this module is simply no longer one of its readers.
use yadgar_store::pool::{parse_ssl_mode, PoolConfig, PoolError};

/// The key selecting how TLS is negotiated to the engine.
const SSL_MODE_KEY: &str = "DB_SSL_MODE";

/// The key naming the authority the verifying modes check the engine against.
///
/// **`DB_SSL_*` rather than `DB_TLS_*`, and the difference is deliberate.** The
/// prefix rule this module follows for [`LISTEN`] gives `DB` either way — a
/// connection OUT is named for what it reaches. What differs is the middle word,
/// and the gate decides it. Every `<UPSTREAM>_TLS_*` family in the estate is
/// gated by a boolean `_TLS_ENABLED`. This dial has no such flag: it is gated by
/// five-valued [`SSL_MODE_KEY`], and this file is meaningful under two of those
/// values and inert under three. `SSL` also names what it fills: sqlx's
/// `ssl_ca`, on [`yadgar_store::pool::PoolConfig::ssl_ca`].
const SSL_CA_KEY: &str = "DB_SSL_CA_FILE";

/// The environment variables this module's own listener is configured from:
/// `LISTEN_TLS_ENABLED`, `LISTEN_TLS_CERT_FILE`, `LISTEN_TLS_KEY_FILE`,
/// `LISTEN_TLS_CLIENT_AUTH` and `LISTEN_TLS_CLIENT_CA_FILE`.
///
/// Built from a PREFIX rather than written out three times, so the naming stays
/// mechanical. `LISTEN` is already the variable holding the address this module
/// binds, so the listener's transport keys extend a name that exists; a
/// connection OUT is named for what it reaches, which is why `DB_*` means the
/// engine. A bare `TLS_ENABLED` is ambiguous between the two directions, which
/// is what makes a prefix necessary at all.
pub const LISTEN: &str = "LISTEN";

/// A knob read from ONE source, refusing rather than inventing (ADR-0569).
///
/// It replaces `env_or(env, key, default)`, and deleting the `default` parameter
/// is more of the point than the rename: while the helper took one, every knob
/// in [`pool_config`] had somewhere for a fallback to live, and a fallback is
/// invisible at the point of use, survives an upgrade unnoticed, and makes the
/// effective setting depend on which layer a reader happens to inspect.
///
/// AN EMPTY VALUE REFUSES TOO, AND WITH ITS OWN MESSAGE. A set-but-empty
/// variable and an absent one collapsing into a single branch is a defect this
/// estate found three separate times in one week. Helm renders an unset value as
/// `""`, so the empty case is what a nulled chart value actually produces, and it
/// is the one an operator is most likely to hit. It also has to be caught HERE
/// rather than by the parse: `"".parse::<u16>()` is a `ParseIntError` that names
/// no key at all, so an operator reading a crash loop would learn that some
/// number was unreadable and never which one.
///
/// THE LOOKUP IS PASSED IN, for the reason [`pool_config`] gives — a test states
/// a whole environment without mutating the process.
fn env_required(env: &impl Fn(&str) -> Option<String>, key: &str) -> Result<String, String> {
    match env(key) {
        Some(value) if !value.is_empty() => Ok(value),
        Some(_) => Err(format!(
            "{key} is set but EMPTY. It has no compiled-in default (ADR-0569), so there is \
             nothing to fall back to. The chart renders it; a values override that nulls it \
             produces exactly this."
        )),
        None => Err(format!(
            "{key} is NOT SET. It has no compiled-in default (ADR-0569): this process reads \
             it from the environment alone and refuses to start rather than invent a value. \
             The chart renders it."
        )),
    }
}

/// [`env_required`], with the chart key appended to whichever sentence it
/// chose (absent or set-but-empty).
///
/// ADR-0569 asks a refusal to name the knob AND where it is set. Before this,
/// every refusal here ended in the generic "The chart renders it." — true,
/// but not where. `boot::lock::migration_lock` already appends the chart key
/// this way; this is that same shape made available to every knob in
/// [`pool_config`] rather than one.
fn env_required_chart(
    env: &impl Fn(&str) -> Option<String>,
    key: &str,
    chart_key: &str,
) -> Result<String, BootError> {
    env_required(env, key).map_err(|sentence| {
        BootError::MissingKnob(format!("{sentence} Set the chart value {chart_key}."))
    })
}

/// A value read by [`env_required_chart`], then parsed as a whole number.
///
/// REPLACES THE BARE `?` ON `.parse()`, which used to reach
/// [`BootError::Int`] — a `ParseIntError` naming no variable and no chart
/// key, so an operator reading a crash loop learned only that SOME number
/// was unreadable (ledger 1257).
fn parse_knob<T>(raw: String, key: &'static str, chart_key: &'static str) -> Result<T, BootError>
where
    T: std::str::FromStr<Err = std::num::ParseIntError>,
{
    raw.parse().map_err(|source| BootError::Unparsable {
        key,
        chart_key,
        value: raw,
        source,
    })
}

/// Parse `LISTEN` or `METRICS_LISTEN`, naming the variable on failure.
///
/// LIVES HERE RATHER THAN IN `main.rs` (ledger 1257, coordinator review on
/// project-db#54), for this module's own founding reason: "nothing in a
/// binary entry point is reachable from a test." `main.rs` used to wrap
/// `.parse()` in its own inline `map_err` at each of the two call sites —
/// correct, but untested, because `main` cannot be unit-tested and the
/// mutation that deletes one `map_err` compiles and passes every suite
/// here. One function, called from both sites, is one thing `src/boot/
/// tests.rs` can prove the naming survives.
///
/// Neither `LISTEN` nor `METRICS_LISTEN` has a chart key to name: both are
/// hardcoded literals in `templates/deployment.yaml` (`"0.0.0.0:50051"`,
/// `"0.0.0.0:9090"`), never read from a `values.yaml` key — unlike every
/// knob in [`pool_config`], there is nowhere else to point an operator.
pub fn parse_listen_addr(key: &'static str, value: &str) -> Result<std::net::SocketAddr, String> {
    value
        .parse()
        .map_err(|e| format!("{key} is not a usable socket address: {e}."))
}

/// `REPLICAS`'s chart key is two keys, not one: `templates/deployment.yaml`
/// renders `autoscaling.maxReplicas` instead of `replicaCount` whenever
/// `autoscaling.enabled` is true. A refusal naming only `replicaCount` would
/// send an operator who has autoscaling on looking at a key the template
/// never read.
const REPLICAS_CHART_KEY: &str = "replicaCount (or autoscaling.maxReplicas when \
     autoscaling.enabled is true)";

/// The four pool-sizing knobs `yadgar-store` v0.4.0 stopped defaulting
/// (ADR-0837, ADR-0849): how long `acquire` waits, how long an idle
/// connection is kept, the age a connection is retired at, and how many of
/// the engine's connections stay reserved for an operator. Each is a
/// boot-only, single-reader, per-module knob (ADR-0837), so each is one
/// environment variable rendered from this chart, read with no fallback —
/// the same shape [`lock::migration_lock`] already holds for its own knob.
const ACQUIRE_TIMEOUT_KEY: &str = "DB_ACQUIRE_TIMEOUT_SECONDS";
const ACQUIRE_TIMEOUT_CHART_KEY: &str = "database.acquireTimeoutSeconds";
const IDLE_TIMEOUT_KEY: &str = "DB_IDLE_TIMEOUT_SECONDS";
const IDLE_TIMEOUT_CHART_KEY: &str = "database.idleTimeoutSeconds";
const MAX_LIFETIME_KEY: &str = "DB_MAX_LIFETIME_SECONDS";
const MAX_LIFETIME_CHART_KEY: &str = "database.maxLifetimeSeconds";
const OPERATOR_RESERVE_KEY: &str = "DB_ENGINE_OPERATOR_RESERVE";
const OPERATOR_RESERVE_CHART_KEY: &str = "database.engineOperatorReserve";

/// Read the pool configuration, refusing rather than guessing.
///
/// Takes the environment as a lookup rather than reading it directly, so a test
/// can state a whole environment without mutating the process — `std::env` is
/// global and `cargo test` runs threads in parallel.
pub fn pool_config(env: impl Fn(&str) -> Option<String>) -> Result<PoolConfig, BootError> {
    // EVERY ONE OF THE TWELVE IS REQUIRED, and every one is rendered by this
    // repository's chart — which is the half that makes the requirement safe
    // rather than a pod that will not boot. `map_err` at each site rather than a
    // signature change: this function's error type is `BootError` and its callers
    // read it, so the refusal joins the enum as one more sentence instead of
    // becoming a second error type beside it.
    Ok(PoolConfig {
        host: env_required_chart(&env, "DB_HOST", "database.host")?,
        port: parse_knob(
            env_required_chart(&env, "DB_PORT", "database.port")?,
            "DB_PORT",
            "database.port",
        )?,
        database: env_required_chart(&env, "DB_NAME", "database.name")?,
        username: env_required_chart(&env, "DB_USER", "database.user")?,
        max_connections: parse_knob(
            env_required_chart(&env, "DB_MAX_CONNECTIONS", "database.maxConnections")?,
            "DB_MAX_CONNECTIONS",
            "database.maxConnections",
        )?,
        replicas: parse_knob(
            env_required_chart(&env, "REPLICAS", REPLICAS_CHART_KEY)?,
            "REPLICAS",
            REPLICAS_CHART_KEY,
        )?,
        engine_max_connections: parse_knob(
            env_required_chart(
                &env,
                "DB_ENGINE_MAX_CONNECTIONS",
                "database.engineMaxConnections",
            )?,
            "DB_ENGINE_MAX_CONNECTIONS",
            "database.engineMaxConnections",
        )?,
        operator_reserve: parse_knob(
            env_required_chart(&env, OPERATOR_RESERVE_KEY, OPERATOR_RESERVE_CHART_KEY)?,
            OPERATOR_RESERVE_KEY,
            OPERATOR_RESERVE_CHART_KEY,
        )?,
        acquire_timeout: Duration::from_secs(parse_knob(
            env_required_chart(&env, ACQUIRE_TIMEOUT_KEY, ACQUIRE_TIMEOUT_CHART_KEY)?,
            ACQUIRE_TIMEOUT_KEY,
            ACQUIRE_TIMEOUT_CHART_KEY,
        )?),
        idle_timeout: Duration::from_secs(parse_knob(
            env_required_chart(&env, IDLE_TIMEOUT_KEY, IDLE_TIMEOUT_CHART_KEY)?,
            IDLE_TIMEOUT_KEY,
            IDLE_TIMEOUT_CHART_KEY,
        )?),
        max_lifetime: Duration::from_secs(parse_knob(
            env_required_chart(&env, MAX_LIFETIME_KEY, MAX_LIFETIME_CHART_KEY)?,
            MAX_LIFETIME_KEY,
            MAX_LIFETIME_CHART_KEY,
        )?),
        ssl_mode: parse_ssl_mode(&env_required_chart(&env, SSL_MODE_KEY, "database.sslMode")?)?,
        // STILL AN OPTIONAL READ, and deliberately NOT converted to
        // `env_required` with the rest (ADR-0569). The chart renders
        // DB_SSL_CA_FILE only under `database.sslCaSecret`, so requiring it would
        // refuse the boot of every deployment that never asked for certificate
        // verification — the rule's own failure mode, pointed the other way. An
        // absent authority is a correct system, which is exactly the shape
        // ADR-0569's revisit trigger names.
        //
        // TRIMMED AND EMPTY-FILTERED, unlike every value above, because this one
        // is an `Option` and Helm renders an unset value as `""`. Without the
        // filter that empty string becomes `Some(PathBuf::new())` — a path sqlx
        // opens and cannot, so a deployment that never asked for certificate
        // verification fails to boot. Absent and empty must mean the same thing:
        // no authority named, which is what `None` is.
        ssl_ca: env(SSL_CA_KEY)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    })
}

/// The options D7's capability probe connects with.
///
/// It is `store`'s own [`yadgar_store::pool::connect_options`] and deliberately
/// nothing else — the same call the pool makes, given the same config. This
/// function adds no behaviour; it exists so that the probe's description of a
/// connection and the pool's cannot drift apart, and so that a test can say
/// which one the probe got.
///
/// **IT RETURNS A `Result` BECAUSE `store` REFUSES A MODE HERE, and forwarding
/// that refusal unchanged is the whole of what this adds.** `verify_ca` names a
/// check sqlx does not perform — the trust store is seeded with the public web
/// roots before the configured authority is appended, and every mode but
/// `verify_identity` skips the hostname check — so it accepts any
/// publicly-trusted certificate for any name.
pub fn probe_connect_options(
    config: &PoolConfig,
    secret: &Secret,
) -> Result<MySqlConnectOptions, BootError> {
    Ok(yadgar_store::pool::connect_options(config, secret)?)
}

/// The future `serve_with_shutdown` drains on, adapted to this module's error.
///
/// **THE BEHAVIOUR IS `yadgar_lifecycle::shutdown`'S AND NOTHING HERE CHANGES
/// IT.** SIGTERM and SIGINT, both handlers installed before the call returns.
/// What lives in this repository is three lines of error adaptation, and the
/// paragraph an operator reads: `main` returns `Box<dyn Error>`, which Rust
/// prints with `Debug`, so a bare `io::Error` here would land in the crash loop
/// as `Os { code: 24, .. }` and say none of it.
///
/// **A `map_err` RATHER THAN A `From` IMPL, deliberately.** [`BootError`] would
/// need `From<std::io::Error>` for a bare `?` to work, and
/// [`BootError::SignalHandler`] is not the only reason this module could meet an
/// `io::Error` — so the blanket impl would let any unreadable file become a
/// signal-handler refusal at whatever call site next wrote `?`.
///
/// # Errors
///
/// Registration can fail, and `main` refuses to start on it. A server that
/// cannot hear SIGTERM cannot drain, and starting anyway hides that until the
/// next rollout.
pub fn shutdown() -> Result<impl std::future::Future<Output = ()>, BootError> {
    yadgar_lifecycle::shutdown().map_err(|source| BootError::SignalHandler { source })
}

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// The listener's transport refused the boot
    /// (`yadgar_lifecycle::serve_tls`, B-U5). `detail` is the crate's own
    /// sentence, which names the variable AND the chart key (ADR-0845,
    /// ADR-0854), or for an unusable identity the whole error chain flattened
    /// once (ADR-0591). Nothing here downgrades to a cleartext listener.
    #[error("{detail}")]
    ListenerTls { detail: String },

    // NO `name` FIELD. `yadgar_lifecycle::shutdown` installs both handlers and
    // returns one `io::Error`, so WHICH of the two failed is not knowable here.
    // Naming both is the honest reading and costs the operator nothing: the
    // sentence below already says there is no value to correct and that the pod
    // restarting is the right response.
    #[error(
        "the SIGTERM and SIGINT handlers could not be installed: {source}. This module \
         refuses to start rather than run without them: Kubernetes ends every pod with \
         SIGTERM, and a process that cannot hear it is one that never drains — its \
         in-flight writes are severed by the SIGKILL that follows, on every rolling \
         update, with nothing in the logs to say so. This is a broken process \
         environment rather than a configuration mistake, so there is no value to \
         correct; the pod restarting is the right response."
    )]
    SignalHandler {
        #[source]
        source: std::io::Error,
    },

    // THE SENTENCE COMES THROUGH UNWRAPPED. `env_required` already writes a
    // paragraph naming the knob, saying which of absent and empty it met, and
    // pointing at the chart that renders it — so a variant adding words of its
    // own would say the key twice and the reason once. The two cases are one
    // variant for the same reason: they differ only in the sentence
    // `env_required` chose (absent versus set-but-empty, which is what Helm
    // renders an unset value as), and a variant carrying only the key could not
    // tell them apart without becoming two variants that say the same thing.
    #[error("{0}")]
    MissingKnob(String),

    /// The migration lock's wait is set and unusable (ledger 814, ADR-0837).
    /// Absent and empty are [`BootError::MissingKnob`]; this is a value that is
    /// there and cannot be a wait. It names the variable AND the chart key,
    /// because ADR-0569 asks a refusal to say where the knob is set. The
    /// reason goes LAST, since `store`'s ends in a full stop and a parse error's
    /// does not, and the bound is NOT restated: chart/values.schema.json is its
    /// one source.
    #[error(
        "DB_MIGRATION_LOCK_TIMEOUT_SECONDS is {value:?}, which is not a usable migration \
         lock wait. Set the chart value database.migrationLockTimeoutSeconds to a whole \
         number of seconds, at least 1, within the bound chart/values.schema.json sets. \
         Why: {reason}"
    )]
    MigrationLockWait { value: String, reason: String },

    #[error(transparent)]
    Pool(#[from] PoolError),

    /// A knob that IS set, read, and is not a whole number (ledger 1257).
    ///
    /// Replaces the bare `Int(#[from] ParseIntError)` this variant used to
    /// be: a `?` on `.parse()` turned any unreadable number into a
    /// `ParseIntError` carrying neither the variable nor the chart key, so
    /// an operator reading a crash loop learned only that SOME number could
    /// not be read. [`parse_knob`] builds this naming both, plus the value
    /// that failed — the same discipline [`BootError::MigrationLockWait`]
    /// already holds for its own knob.
    #[error("{key} is {value:?}, which is not a whole number: {source}. Set the chart value {chart_key}.")]
    Unparsable {
        key: &'static str,
        chart_key: &'static str,
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
}

mod lock;
pub use lock::{migration_lock, MIGRATION_LOCK_TIMEOUT_CHART_KEY, MIGRATION_LOCK_TIMEOUT_KEY};

mod serve_tls;
pub use serve_tls::{listener_tls, server, ClientAuth, ServerTls, TLS_CHART_KEY};

#[cfg(test)]
mod tests;
