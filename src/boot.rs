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
//! decides how this module reaches its engine; [`ServeTls`] decides what
//! `project` gets when it reaches this module. NEITHER DEFAULTS ANY MORE:
//! `DB_SSL_MODE` is required and refuses the boot when unset (ADR-0569), and the
//! listener's transport is a flag that is either set or absent. Both refuse
//! rather than downgrade when asked for something they cannot deliver, and
//! neither names an issuer, a CRD or a mesh (D80) — a flag and file paths is the
//! whole of the configuration.
//!
//! **THERE IS NO `DB_REQUIRE_TLS` REFUSAL HERE, unlike in `task-db` and
//! `iam-db`.** That refusal exists in those modules because they once READ the
//! key, so an operator who set it was owed a boot failure rather than a silently
//! withdrawn guarantee. This module has never read it, and adding a refusal for
//! a key it never had would be a deprecation notice for a history it does not
//! have.

use std::path::{Path, PathBuf};

use sqlx::mysql::MySqlConnectOptions;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use yadgar_store::credentials::Secret;
// NO `DEFAULT_SSL_MODE`. It was the fallback `pool_config` handed
// `parse_ssl_mode` when DB_SSL_MODE was unset, and under ADR-0569 there is no
// such position to hand anything to. The constant still exists in
// `yadgar-store` and this module is simply no longer one of its readers.
use yadgar_store::pool::{parse_ssl_mode, PoolConfig, PoolError};
// THE ONE ERROR-CHAIN FLATTENER FOR THE ESTATE (ADR-0591). The body that used to
// sit below `shutdown` in this file was one of five — `iam`, `iam-db`, `task`,
// `task-db` and here — byte-identical apart from local names, under TWO names:
// `chain` in the first two and `describe` in the other three. It is deleted
// rather than left beside the shared one, because a consolidation that adds a
// sixth copy without removing the five is worse than none.
use yadgar_telemetry::diagnose::chain;

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
/// `LISTEN_TLS_ENABLED`, `LISTEN_TLS_CERT_FILE` and `LISTEN_TLS_KEY_FILE`.
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

/// Read the pool configuration, refusing rather than guessing.
///
/// Takes the environment as a lookup rather than reading it directly, so a test
/// can state a whole environment without mutating the process — `std::env` is
/// global and `cargo test` runs threads in parallel.
pub fn pool_config(env: impl Fn(&str) -> Option<String>) -> Result<PoolConfig, BootError> {
    // EVERY ONE OF THE EIGHT IS REQUIRED, and every one is rendered by this
    // repository's chart — which is the half that makes the requirement safe
    // rather than a pod that will not boot. `map_err` at each site rather than a
    // signature change: this function's error type is `BootError` and its callers
    // read it, so the refusal joins the enum as one more sentence instead of
    // becoming a second error type beside it.
    Ok(PoolConfig {
        host: env_required(&env, "DB_HOST").map_err(BootError::MissingKnob)?,
        port: env_required(&env, "DB_PORT")
            .map_err(BootError::MissingKnob)?
            .parse()?,
        database: env_required(&env, "DB_NAME").map_err(BootError::MissingKnob)?,
        username: env_required(&env, "DB_USER").map_err(BootError::MissingKnob)?,
        max_connections: env_required(&env, "DB_MAX_CONNECTIONS")
            .map_err(BootError::MissingKnob)?
            .parse()?,
        replicas: env_required(&env, "REPLICAS")
            .map_err(BootError::MissingKnob)?
            .parse()?,
        engine_max_connections: env_required(&env, "DB_ENGINE_MAX_CONNECTIONS")
            .map_err(BootError::MissingKnob)?
            .parse()?,
        ssl_mode: parse_ssl_mode(
            &env_required(&env, SSL_MODE_KEY).map_err(BootError::MissingKnob)?,
        )?,
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

/// The identity this module presents to callers: a certificate and its private
/// key, both as paths on disk.
///
/// **File paths, never an issuer-specific resource** (D80). cert-manager writes
/// these files in the reference deployment and a hand-assembled Secret writes
/// them anywhere else, and nothing here can tell the difference — which is the
/// point.
///
/// **No verification domain.** A client checks the name it dialled against the
/// certificate it was shown; a server presents what it was given and checks
/// nothing. A caller's `UpstreamTls` carries a domain override for that reason
/// and this does not, which is an asymmetry rather than an omission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeTls {
    cert_file: PathBuf,
    key_file: PathBuf,
}

impl ServeTls {
    /// Read the listener's transport configuration from the environment.
    ///
    /// `Ok(None)` is the ordinary answer today: TLS is opt-in, so an
    /// unconfigured deployment serves in cleartext.
    pub fn from_env(prefix: &'static str) -> Result<Option<Self>, BootError> {
        Self::from_lookup(prefix, |key| std::env::var(key).ok())
    }

    /// The same decision, over an injected lookup — the shape every other
    /// decision in this module already takes, and for the same reason:
    /// `std::env` is process-global, so a test that sets one variable steers
    /// every other test in the binary.
    pub fn from_lookup(
        prefix: &'static str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, BootError> {
        let get = |suffix: &str| {
            lookup(&format!("{prefix}_{suffix}"))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        // Exactly "1". A permissive parse here — "0", "false" and "no" all
        // enabling it — is how a setting meant to be off ends up on, and the
        // reverse mistake is worse: this flag is the revert lever for a
        // cut-over, and a lever that does not move is not one.
        if get("TLS_ENABLED").as_deref() != Some("1") {
            if get("TLS_CERT_FILE").is_some() || get("TLS_KEY_FILE").is_some() {
                // NOT an error. Leaving the certificate in place while the flag
                // is off is exactly how a cut-over gets reverted, so refusing it
                // would make the lever unusable. It is still worth a line: a
                // deployment that believes it is encrypted and is not should be
                // able to see that from the boot log.
                tracing::warn!(
                    prefix,
                    "a serving certificate is configured but {prefix}_TLS_ENABLED is not \
                     \"1\", so this module listens in CLEARTEXT"
                );
            }
            return Ok(None);
        }

        Ok(Some(Self {
            cert_file: PathBuf::from(get("TLS_CERT_FILE").ok_or(BootError::NoTlsCertFile(prefix))?),
            key_file: PathBuf::from(get("TLS_KEY_FILE").ok_or(BootError::NoTlsKeyFile(prefix))?),
        }))
    }

    /// The PEM certificate this module presents.
    pub fn cert_file(&self) -> &Path {
        &self.cert_file
    }

    /// The PEM private key belonging to that certificate.
    pub fn key_file(&self) -> &Path {
        &self.key_file
    }

    /// Read both files and hand tonic the pair.
    ///
    /// Reading them HERE rather than letting tonic do it is what lets the error
    /// name WHICH file was wrong. `Identity::from_pem` takes bytes and has no
    /// idea where they came from, so an operator whose Secret mounted only one
    /// of the two would otherwise be told that "an identity" was unusable.
    fn identity(&self) -> Result<Identity, BootError> {
        let cert = read_pem(&self.cert_file, "certificate")?;
        let key = read_pem(&self.key_file, "private key")?;
        Ok(Identity::from_pem(cert, key))
    }
}

fn read_pem(path: &Path, what: &'static str) -> Result<Vec<u8>, BootError> {
    std::fs::read(path).map_err(|source| BootError::TlsUnreadable {
        what,
        path: path.to_path_buf(),
        source,
    })
}

/// Build the gRPC server this module listens with.
///
/// **THE ONLY SERVER CONSTRUCTION IN THIS BINARY, and that is structural rather
/// than tidy.** The failure this seam exists to prevent is a listener that opens
/// in cleartext because TLS configuration failed. A `Server::builder()` call
/// anywhere else would be a place that downgrade could be written; with one, the
/// only way to reintroduce it is to add a fallback here, where
/// `a_tls_listener_refuses_a_cleartext_client` is looking.
///
/// **ALPN is tonic's, not ours.** `ServerTlsConfig` pushes `h2` onto the
/// acceptor's protocol list, and a gRPC listener that negotiated anything else
/// would answer nothing useful. It is verified rather than assumed: tonic's own
/// client refuses a channel whose negotiated protocol is not `h2`, so the
/// handshake cases in `tests/serve_tls.rs` fail if it ever stops being offered.
///
/// **Called BEFORE the probe and the migration**, so that a deployment which
/// asked for TLS and got the mount wrong exits without touching the engine at
/// all. D69 puts the refusals first; this one is cheaper than the rest.
pub fn server(tls: Option<&ServeTls>) -> Result<Server, BootError> {
    let server = Server::builder();
    let Some(tls) = tls else {
        return Ok(server);
    };

    let identity = tls.identity()?;
    // EAGER, and before anything binds. `tls_config` builds the rustls acceptor
    // here — it is what decodes the PEM and checks that the certificate belongs
    // to the key — so a bad pair is an error at boot rather than a handshake
    // that fails on a stranger's first connection.
    server
        .tls_config(ServerTlsConfig::new().identity(identity))
        .map_err(|e| BootError::TlsUnusable {
            cert: tls.cert_file.clone(),
            key: tls.key_file.clone(),
            detail: chain(&e),
        })
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
/// [`BootError::TlsUnreadable`] already carries an `io::Error` for an entirely
/// different reason — so the blanket impl would let any unreadable file become a
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
    #[error(
        "{0}_TLS_ENABLED is set but {0}_TLS_CERT_FILE names no certificate. TLS was \
         asked for, so this is a deployment mistake rather than a reason to open a \
         plaintext listener — and it is NOT the same as leaving TLS off, which is the \
         supported way to serve without one. Point {0}_TLS_CERT_FILE at the PEM \
         certificate this module should present."
    )]
    NoTlsCertFile(&'static str),

    #[error(
        "{0}_TLS_ENABLED is set but {0}_TLS_KEY_FILE names no private key. A \
         certificate without its key cannot complete a handshake, so this refuses \
         rather than opening a plaintext listener. Point {0}_TLS_KEY_FILE at the PEM \
         private key belonging to {0}_TLS_CERT_FILE."
    )]
    NoTlsKeyFile(&'static str),

    #[error(
        "the TLS {what} at {path} could not be read: {source}. TLS was asked for, so \
         this module refuses to start rather than serving in cleartext. The usual \
         cause is a Secret that was never mounted, or a key inside it under a \
         different name than the chart selected."
    )]
    TlsUnreadable {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "the TLS certificate at {cert} and the private key at {key} were read but \
         refused: {detail}. Both files exist, so this is their CONTENT: a PEM that \
         decodes to no certificate at all, or a certificate that does not belong to \
         the key beside it — what a half-finished rotation leaves behind. This module \
         refuses to start rather than serving in cleartext."
    )]
    TlsUnusable {
        cert: PathBuf,
        key: PathBuf,
        detail: String,
    },

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

    #[error(transparent)]
    Pool(#[from] PoolError),

    #[error(transparent)]
    Int(#[from] std::num::ParseIntError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use yadgar_store::pool::MySqlSslMode;

    /// An environment stating only what a test cares about.
    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    /// EVERY KNOB `pool_config` REQUIRES, WITH THE VALUE THE CHART WOULD RENDER.
    ///
    /// There is no such thing as a partial environment for this function any
    /// more, which is the whole of ADR-0569 restated as a fixture: a test that
    /// used to pass `&[]` was asserting the defaults, and the defaults are gone.
    ///
    /// THE VALUES ARE SENTINELS, not plausible ones. Nothing in this module, in
    /// `store`, or in sqlx would arrive at `engine.example.invalid`, port 13306
    /// or `verify-identity` on its own — so a field carrying one of these got it
    /// from the lookup rather than from a fallback somebody left behind. The
    /// former defaults (`127.0.0.1`, `3306`, `project`, `8`, `2`, `151`,
    /// `required`) are deliberately absent from this list: a fixture repeating
    /// them could not tell a read from a leftover default.
    const RENDERED: [(&str, &str); 8] = [
        ("DB_HOST", "engine.example.invalid"),
        ("DB_PORT", "13306"),
        ("DB_NAME", "okapi_3f19_fixture"),
        ("DB_USER", "okapi_3f19_user"),
        ("DB_MAX_CONNECTIONS", "4"),
        ("REPLICAS", "3"),
        ("DB_ENGINE_MAX_CONNECTIONS", "137"),
        ("DB_SSL_MODE", "verify-identity"),
    ];

    /// The rendered environment, with `overrides` winning over it.
    fn env_with<'a>(overrides: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            overrides
                .iter()
                .chain(RENDERED.iter())
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    /// The rendered environment with ONE knob deleted — an unrendered variable.
    fn env_without(omitted: &str) -> impl Fn(&str) -> Option<String> + '_ {
        move |key| {
            if key == omitted {
                return None;
            }
            RENDERED
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    /// `verify_identity` throughout, and never `required`.
    ///
    /// `required` is what this module used to default to, so an implementation
    /// that ignored the configuration entirely would pass a test fixed on it.
    /// `verify_identity` is a mode neither this code nor sqlx would ever arrive
    /// at on its own.
    fn config_with(mode: MySqlSslMode) -> PoolConfig {
        PoolConfig {
            host: "engine.example.invalid".to_string(),
            port: 13306,
            database: "okapi_3f19_fixture".to_string(),
            username: "okapi_3f19_user".to_string(),
            max_connections: 4,
            replicas: 2,
            engine_max_connections: 151,
            ssl_mode: mode,
            ssl_ca: None,
        }
    }

    #[test]
    fn the_probe_connects_with_the_configured_mode_not_a_mode_of_its_own() {
        let config = config_with(MySqlSslMode::VerifyIdentity);
        let options =
            probe_connect_options(&config, &Secret::new("fixture".to_string())).expect("options");

        // The defect this seam exists to prevent is a probe that describes its
        // own connection. Had it done so — hardcoding Required, or falling back
        // to sqlx's Preferred — this is the assertion that would not hold.
        assert!(
            matches!(options.get_ssl_mode(), MySqlSslMode::VerifyIdentity),
            "the probe did not take the configured ssl-mode"
        );

        // The rest of the connection comes from the same place, so a probe
        // pointed at a different host or database would be caught here too.
        // NO DSN in any message: an assert that interpolates one puts a username
        // into CI output.
        assert_eq!(options.get_host(), "engine.example.invalid");
        assert_eq!(options.get_port(), 13306);
        assert_eq!(options.get_username(), "okapi_3f19_user");
        assert_eq!(options.get_database(), Some("okapi_3f19_fixture"));
    }

    #[test]
    fn certificate_verification_is_reachable_from_the_environment() {
        // The hyphen spelling is the one a chart writes; sqlx writes the
        // underscore.
        let config = pool_config(env_with(&[("DB_SSL_MODE", "verify-identity")])).expect("config");
        assert!(matches!(config.ssl_mode, MySqlSslMode::VerifyIdentity));
    }

    #[test]
    fn verify_ca_is_refused_when_the_connection_options_are_built() {
        // `verify_ca` NAMES A CHECK NOTHING PERFORMS, which is why `store`
        // refuses it outright rather than documenting it. Measured in sqlx 0.9:
        // `sqlx-core`'s `net/tls/tls_rustls.rs` seeds the trust store with the
        // public web roots BEFORE appending the configured `ssl_ca`, so naming
        // an authority WIDENS trust and never restricts it; `sqlx-mysql`'s
        // `connection/tls.rs` then routes every mode but `VerifyIdentity`
        // through `NoHostnameTlsVerifier`, which maps a name mismatch to a
        // verified assertion.
        //
        // THE PROBE IS THE CALLER THIS PINS, and that is the point. The probe
        // opens its own connection before the pool exists, so a refusal that
        // lived only in `connect` would let the probe connect first — under the
        // very verifier being refused.
        let config = config_with(MySqlSslMode::VerifyCa);
        let err = probe_connect_options(&config, &Secret::new("fixture".to_string()))
            .expect_err("verify_ca must be refused before any connection is opened");

        assert!(
            matches!(err, BootError::Pool(PoolError::SslModeCannotVerify { .. })),
            "{err}"
        );

        // An operator reads this in a crash loop, so it has to name the mode
        // that WORKS. A refusal that only says no leaves them guessing between
        // three remaining values, two of which verify nothing either.
        let message = err.to_string();
        assert!(
            message.contains("verify_identity"),
            "the refusal must name the mode that does bind the engine's identity: {message}"
        );
    }

    /// A SENTINEL: nothing in this module or in `store` could produce this path,
    /// so a test that sees it saw it travel from the environment.
    const SENTINEL_CA: &str = "/etc/yadgar/okapi-3f19/engine-authority.pem";

    #[test]
    fn the_configured_certificate_authority_reaches_the_pool() {
        // A MODE IS NOT THE CAPABILITY. `verify_identity` checks a CHAIN, and
        // with no authority named sqlx falls back to the public web roots, which
        // sign no operator-issued engine certificate.
        let config = pool_config(env_with(&[("DB_SSL_CA_FILE", SENTINEL_CA)])).expect("config");

        assert_eq!(
            config.ssl_ca.as_deref(),
            Some(Path::new(SENTINEL_CA)),
            "the configured authority did not reach the pool configuration"
        );
    }

    #[test]
    fn an_unset_or_empty_authority_is_no_authority_rather_than_an_empty_path() {
        // UNSET is the shipped deployment and must stay `None`: `Some` here
        // would name a file sqlx then fails to open, and a default CA path is a
        // policy this module has no business inventing.
        assert_eq!(pool_config(env_with(&[])).expect("config").ssl_ca, None);

        // EMPTY is the same statement written by a chart. Helm renders an unset
        // value as "", so a naive read turns "no authority" into `PathBuf::new()`
        // — a path sqlx opens and cannot, failing the boot of every deployment
        // that never asked for verification at all.
        for value in ["", " ", "\t", "\n"] {
            assert_eq!(
                pool_config(env_with(&[("DB_SSL_CA_FILE", value)]))
                    .expect("config")
                    .ssl_ca,
                None,
                "{value:?} must mean no authority, not an unopenable path"
            );
        }
    }

    #[test]
    fn an_unrecognised_ssl_mode_refuses_the_boot_rather_than_falling_back() {
        // `yes` is not arbitrary: it is what a boolean-shaped answer looks like,
        // and failing open on a transport question is the class of bug rather
        // than one spelling of it.
        let err = pool_config(env_with(&[("DB_SSL_MODE", "yes")]))
            .expect_err("an unrecognised mode must refuse the boot");

        assert!(
            matches!(err, BootError::Pool(PoolError::UnknownSslMode { .. })),
            "{err}"
        );
    }

    /// THE PROPERTY THAT USED TO BE CALLED "the default encrypts", now that there
    /// is no default to name.
    ///
    /// What it guarded was never the value `required` — it was that an
    /// unconfigured deployment could not silently arrive at `Preferred`, which
    /// sqlx documents as falling back to an unencrypted connection. That
    /// guarantee is STRUCTURAL now rather than a value: an environment naming no
    /// mode does not reach `parse_ssl_mode` at all, it refuses the boot and names
    /// DB_SSL_MODE. A default of `required` was the WEAKER form of the same
    /// promise, because it also silently overrode an operator who meant to set a
    /// mode and mistyped the variable's name.
    #[test]
    fn an_unset_ssl_mode_refuses_the_boot_rather_than_encrypting_by_default() {
        let err = pool_config(env_without(SSL_MODE_KEY))
            .expect_err("an unset ssl-mode must refuse the boot, not fall back to a value");

        let message = err.to_string();
        assert!(matches!(err, BootError::MissingKnob(_)), "{message}");
        assert!(message.contains(SSL_MODE_KEY), "{message}");

        // And the mode still travels when it IS named, so the refusal above is
        // not simply this function having stopped reading the variable.
        let config = pool_config(env_with(&[("DB_SSL_MODE", "required")])).expect("config");
        assert!(matches!(config.ssl_mode, MySqlSslMode::Required), "{err}");
    }

    /// THE ENGINE'S NAME AND USER TRAVEL FROM THE ENVIRONMENT, and no longer
    /// from a constant in this file.
    ///
    /// This test used to be `the_engine_defaults_name_this_module`, asserting
    /// `project` out of an EMPTY environment — which is precisely the shape
    /// ADR-0569 deletes. `chart/values.yaml` carries `database.name: project`
    /// and `database.user: project` now, so a pod that connected to the wrong
    /// database would be a chart edit rather than a copied constant. The
    /// sentinels are what prove the read: nothing here could invent them.
    #[test]
    fn the_engines_name_and_user_travel_from_the_environment() {
        let config = pool_config(env_with(&[])).expect("config");
        assert_eq!(config.database, "okapi_3f19_fixture");
        assert_eq!(config.username, "okapi_3f19_user");
    }

    /// EVERY KNOB IS PROVED REQUIRED, ONE AT A TIME.
    ///
    /// A single test on one variable would pass while seven others kept a
    /// fallback nobody could see, so the loop is the assertion. The refusal has
    /// to NAME the knob: three of these are numbers, and an empty or absent
    /// number that reached `.parse()` would produce a `ParseIntError` saying
    /// "cannot parse integer from empty string" and naming nothing at all — the
    /// operator would learn only that some value was unreadable.
    #[test]
    fn every_rendered_knob_is_required_and_the_refusal_names_it() {
        for (key, _) in RENDERED {
            let err = pool_config(env_without(key)).err().unwrap_or_else(|| {
                panic!("{key} is absent from the environment and pool_config did not refuse")
            });

            let message = err.to_string();
            assert!(
                matches!(err, BootError::MissingKnob(_)),
                "{key} refused, but not as a missing knob: {message}"
            );
            assert!(
                message.contains(key),
                "the refusal must name the knob it wants; {key} produced: {message}"
            );
        }
    }

    /// **THE CASE THAT DISCRIMINATES.** Helm renders an unset value as `""`, so a
    /// nulled chart value arrives as set-but-empty rather than as absent. An
    /// implementation collapsing the two into one branch is a defect this estate
    /// found three separate times in one week, so the two messages are asserted
    /// to DIFFER rather than merely to exist.
    #[test]
    fn an_empty_knob_refuses_with_a_message_of_its_own() {
        for (key, _) in RENDERED {
            let empty = pool_config(env_with(&[(key, "")]))
                .expect_err("an empty knob must refuse the boot")
                .to_string();
            let absent = pool_config(env_without(key))
                .expect_err("an absent knob must refuse the boot")
                .to_string();

            assert!(empty.contains("set but EMPTY"), "{key}: {empty}");
            assert!(absent.contains("NOT SET"), "{key}: {absent}");
            assert_ne!(empty, absent, "{key}: empty and absent share one message");
        }
    }

    /// SENTINELS for the listener's configuration: nothing in this module could
    /// produce either path, so a test that sees one saw it travel from the
    /// lookup.
    const SENTINEL_CERT: &str = "/etc/yadgar/okapi-3f19/serving.crt";
    const SENTINEL_KEY: &str = "/etc/yadgar/okapi-3f19/serving.key";

    /// THE DEFAULT for the listener: nothing configured means the cleartext
    /// listener.
    #[test]
    fn nothing_configured_means_the_listener_serves_cleartext() {
        assert_eq!(ServeTls::from_lookup(LISTEN, env_of(&[])).unwrap(), None);
    }

    /// A certificate without the flag is the REVERTED state, not an error. The
    /// flag is the lever; leaving the paths in place is how it gets pulled back.
    #[test]
    fn a_certificate_alone_does_not_enable_the_listeners_tls() {
        let vars = [
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ];
        assert_eq!(ServeTls::from_lookup(LISTEN, env_of(&vars)).unwrap(), None);
    }

    /// Anything but "1" is off.
    #[test]
    fn only_exactly_one_enables_the_listeners_tls() {
        for value in ["0", "false", "no", "true", "yes", "", " "] {
            let vars = [
                ("LISTEN_TLS_ENABLED", value),
                ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
                ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
            ];
            assert_eq!(
                ServeTls::from_lookup(LISTEN, env_of(&vars)).unwrap(),
                None,
                "{value:?} must not enable TLS"
            );
        }
    }

    /// THE FAILURE THAT MUST NOT DEGRADE. Asking for TLS and naming neither file
    /// is a deployment mistake, and the answer to it is an error rather than a
    /// plaintext listener. The message names the half that is missing.
    #[test]
    fn asking_the_listener_for_tls_without_the_files_is_an_error() {
        let missing_cert = [
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ];
        assert!(
            matches!(
                ServeTls::from_lookup(LISTEN, env_of(&missing_cert)),
                Err(BootError::NoTlsCertFile("LISTEN"))
            ),
            "a missing certificate must be refused, not silently downgraded"
        );

        let missing_key = [
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ];
        assert!(
            matches!(
                ServeTls::from_lookup(LISTEN, env_of(&missing_key)),
                Err(BootError::NoTlsKeyFile("LISTEN"))
            ),
            "a missing private key must be refused, not silently downgraded"
        );
    }

    /// Both paths reach the settings, proved with names the module could not
    /// have chosen for itself.
    #[test]
    fn the_certificate_and_the_key_both_arrive() {
        let vars = [
            ("LISTEN_TLS_ENABLED", "1"),
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ];
        let tls = ServeTls::from_lookup(LISTEN, env_of(&vars))
            .unwrap()
            .expect("a flag, a certificate and a key enable TLS");
        assert_eq!(tls.cert_file(), Path::new(SENTINEL_CERT));
        assert_eq!(tls.key_file(), Path::new(SENTINEL_KEY));
    }

    /// The two directions cannot configure each other. `DB_SSL_MODE` decides how
    /// this module reaches its ENGINE and says nothing about what it serves, and
    /// a bare `TLS_ENABLED` belongs to neither.
    #[test]
    fn the_engines_transport_does_not_configure_the_listener() {
        let vars = [
            ("DB_SSL_MODE", "verify-identity"),
            ("TLS_ENABLED", "1"),
            ("TLS_CERT_FILE", SENTINEL_CERT),
        ];
        assert_eq!(ServeTls::from_lookup(LISTEN, env_of(&vars)).unwrap(), None);
    }

    /// The crate returns one `io::Error` for both handlers, so what must not be
    /// lost is the paragraph: an operator meeting this in a crash loop has to
    /// read that BOTH signals are involved, that SIGTERM is the one Kubernetes
    /// sends, and that there is no configuration value to go and correct.
    ///
    /// **The `Err` path itself is not reachable from a test.** Producing it means
    /// exhausting the process's signal-handler resources, which would break every
    /// other test in the binary. So this asserts what an operator READS, not that
    /// the failure occurs.
    #[test]
    fn a_handler_that_cannot_be_installed_names_both_signals_and_the_response() {
        let rendered = BootError::SignalHandler {
            source: std::io::Error::other("too many signal handlers"),
        }
        .to_string();

        for expected in [
            "SIGTERM",
            "SIGINT",
            "too many signal handlers",
            "no value to correct",
        ] {
            assert!(
                rendered.contains(expected),
                "the refusal no longer carries {expected:?}: {rendered}"
            );
        }
    }
}
