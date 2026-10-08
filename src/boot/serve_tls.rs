//! This module's own listener transport (split out of `boot.rs`, ledger 719:
//! a 500-line ceiling per file). `boot.rs` keeps the engine-facing half
//! (`pool_config`, `probe_connect_options`); this one is everything about
//! what `project` gets when it reaches THIS pod.

use std::path::{Path, PathBuf};

use tonic::transport::{Identity, Server, ServerTlsConfig};
// THE ONE ERROR-CHAIN FLATTENER FOR THE ESTATE (ADR-0591). The body that used
// to sit below `shutdown` in `boot.rs` was one of five — `iam`, `iam-db`,
// `task`, `task-db` and here — byte-identical apart from local names, under
// TWO names: `chain` in the first two and `describe` in the other three. It
// is deleted rather than left beside the shared one, because a consolidation
// that adds a sixth copy without removing the five is worse than none.
use yadgar_telemetry::diagnose::chain;

use super::{env_required, BootError};

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
    pub fn from_env(prefix: &'static str) -> Result<Option<Self>, BootError> {
        Self::from_lookup(prefix, |key| std::env::var(key).ok())
    }

    /// The same decision, over an injected lookup — the shape every other
    /// decision in this module already takes, and for the same reason:
    /// `std::env` is process-global, so a test that sets one variable steers
    /// every other test in the binary.
    ///
    /// **ADR-0845: ABSENT OR EMPTY REFUSES.** This used to default to
    /// cleartext — `Ok(None)` for anything but exactly `"1"` — which is the
    /// posture ADR-0845 names as the most dangerous compiled-in default in
    /// the estate: a security switch whose unset value nobody wrote. The
    /// chart renders `tls.enabled` unconditionally now (ternary, never
    /// `if`), so an absent variable at runtime can only mean a values file
    /// that bypassed the chart.
    ///
    /// Exactly `"1"` or `"0"` are accepted; everything else, including an
    /// absent or empty value, refuses the boot naming the variable and the
    /// chart key `tls.enabled`. [`env_required`] already carries the
    /// absent/empty distinction (its own refusal does not collapse them into
    /// one message), so the flag is read through it rather than through the
    /// `get` helper below, which is reserved for the two file paths.
    pub fn from_lookup(
        prefix: &'static str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, BootError> {
        let get = |suffix: &str| {
            lookup(&format!("{prefix}_{suffix}"))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        let enabled =
            env_required(&lookup, &format!("{prefix}_TLS_ENABLED")).map_err(|sentence| {
                BootError::MissingKnob(format!("{sentence} Set the chart value tls.enabled."))
            })?;

        match enabled.as_str() {
            "1" => Ok(Some(Self {
                cert_file: PathBuf::from(
                    get("TLS_CERT_FILE").ok_or(BootError::NoTlsCertFile(prefix))?,
                ),
                key_file: PathBuf::from(
                    get("TLS_KEY_FILE").ok_or(BootError::NoTlsKeyFile(prefix))?,
                ),
            })),
            "0" => {
                if get("TLS_CERT_FILE").is_some() || get("TLS_KEY_FILE").is_some() {
                    // NOT an error. Leaving the certificate in place while the
                    // flag is off is exactly how a cut-over gets reverted, so
                    // refusing it would make the lever unusable. It is still
                    // worth a line: a deployment that believes it is
                    // encrypted and is not should be able to see that from
                    // the boot log.
                    tracing::warn!(
                        prefix,
                        "a serving certificate is configured but {prefix}_TLS_ENABLED is \
                         \"0\", so this module listens in CLEARTEXT"
                    );
                }
                Ok(None)
            }
            // ANYTHING ELSE REFUSES, including the values this module used
            // to treat as off ("false", "no", "true", "yes"). A permissive
            // parse here is how a setting meant to be off ends up on by
            // accident; refusing the unrecognised value is louder than
            // guessing which way it meant.
            other => Err(BootError::TlsEnabledInvalid {
                prefix,
                value: other.to_string(),
            }),
        }
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
    // ADR-0523-WATCHED: ServeTls
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
