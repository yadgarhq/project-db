//! This module's own listener transport: `yadgar_lifecycle::serve_tls`,
//! adopted (B-U5, ADR-0846, ADR-0852). Split out of `boot.rs` for ledger 719's
//! 500-line ceiling per file.
//!
//! **THE LOCAL `ServeTls` COPY IS GONE.** Six gRPC servers each held one and
//! none called `client_ca_root`, so no hop in the estate verified a client
//! certificate. The lifted [`ServerTls`] is the one implementation the six
//! adopt; what stays here is the wiring only this repository can state — the
//! `LISTEN` prefix, the `tls` chart block, and the adaptation of its refusals
//! to [`BootError`].
//!
//! **NOTHING HERE DEFAULTS.** `LISTEN_TLS_ENABLED` (ADR-0845) and
//! `LISTEN_TLS_CLIENT_AUTH` (ADR-0854) are both required; absence refuses the
//! boot naming the variable and the chart key, and `off` is the emergency value
//! for client auth rather than what a missing variable means.

use tonic::transport::Server;
use yadgar_lifecycle::serve_tls::ServeTlsError;
pub use yadgar_lifecycle::serve_tls::{ClientAuth, ServerTls};
// THE ONE ERROR-CHAIN FLATTENER FOR THE ESTATE (ADR-0591).
use yadgar_telemetry::diagnose::chain;

use super::{BootError, LISTEN};

/// The values block every listener key renders from: `tls.enabled`,
/// `tls.certSecret`, `tls.clientAuth`, `tls.clientCaSecret`. The crate builds
/// each refusal's chart key from it, so a refusal names the key an operator
/// edits in THIS chart.
pub const TLS_CHART_KEY: &str = "tls";

/// Read the listener's transport through `lookup`: `Ok(None)` is the cleartext
/// listener, and only ever the answer to an explicit `LISTEN_TLS_ENABLED=0`
/// with client auth `off`.
///
/// The lookup is injected because `std::env` is process-global: a test that
/// set one variable would steer every other test in the binary.
pub fn listener_tls(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<ServerTls>, BootError> {
    ServerTls::from_lookup(LISTEN, TLS_CHART_KEY, lookup).map_err(refused)
}

/// Build the gRPC server this module listens with.
///
/// **THE ONLY SERVER CONSTRUCTION IN THIS BINARY.** The failure this seam
/// exists to prevent is a listener that opens in cleartext because TLS
/// configuration failed. `None` is the cleartext listener [`listener_tls`]
/// returned for an explicit `"0"`; `Some` is TLS or an error, never cleartext,
/// and `a_tls_listener_refuses_a_cleartext_client` is looking.
///
/// **Called BEFORE the probe and the migration**, so a deployment that asked
/// for TLS and got the mount wrong exits without touching the engine. The
/// crate's builder is eager: it reads every file, matches the certificate to
/// its key and builds the client verifier here.
pub fn server(tls: Option<&ServerTls>) -> Result<Server, BootError> {
    yadgar_lifecycle::serve_tls::server(tls).map_err(refused)
}

/// The crate's refusal as this binary's, flattened through [`chain`] for
/// every variant (ADR-0591): tonic's own `Display` is the two words
/// `transport error` with the reason a `source()` hop down, and the walker is
/// the one place that knows how to reach it.
fn refused(error: ServeTlsError) -> BootError {
    BootError::ListenerTls {
        detail: chain(&error),
    }
}
