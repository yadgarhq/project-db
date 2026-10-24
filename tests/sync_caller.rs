//! Plan case 10 over REAL handshakes: `SyncProjects` admits `project`'s leaf
//! (`project-caller`) and refuses every other verifying leaf PERMISSION_DENIED
//! (ADR-0936, ADR-0938). `RegisterProject` takes the same gate (ADR-1007), and
//! its cases are at the end of this file because they need the same authority.
//!
//! `tests/sync.rs` proves the check runs before any connection is taken, with
//! the identity passed in. This proves the other half: the gRPC handler reads
//! that identity off the client certificate the listener verified — through
//! `boot::server`, the one function `main` builds its server with, under
//! `required`, with platform-shaped leaves (a common name, a dNSName SAN equal
//! to it, `clientAuth`) from ONE authority. Every leaf verifies; only the name
//! differs.

mod support;
use support::*;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::Code;
use yadgar_project_db::boot;
use yadgar_project_db::pb::yadgar::project::v1::project_db_service_server::ProjectDbServiceServer;
use yadgar_project_db::pb::yadgar::project::v1::{
    RegisterProjectRequest, RegisterProjectResponse, SyncProjectsRequest, SyncProjectsResponse,
};
use yadgar_project_db::service::ProjectDb;

/// The chart's shipped `server.limits` (ADR-1006), as the library reads them:
/// the library has no default, so a test that builds a server states them.
fn limits() -> boot::ServerLimits {
    boot::listener_limits(|key| {
        match key {
            "LISTEN_MAX_CONCURRENT_STREAMS" => Some("256"),
            "LISTEN_KEEPALIVE_INTERVAL_SECS" => Some("30"),
            "LISTEN_KEEPALIVE_TIMEOUT_SECS" => Some("10"),
            "LISTEN_MAX_HEADER_LIST_BYTES" => Some("16384"),
            _ => None,
        }
        .map(str::to_string)
    })
    .expect("the chart's shipped limits are in range")
}

const SERVED_NAME: &str = "localhost";

/// One authority, its serving leaf, and a way to mint named client leaves.
struct Pki {
    issuer: CertifiedIssuer<'static, KeyPair>,
    cert_pem: String,
    key_pem: String,
}

impl Pki {
    fn new() -> Self {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params
            .distinguished_name
            .push(DnType::CommonName, "sync-caller test authority");
        let issuer = CertifiedIssuer::self_signed(params, key).unwrap();

        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![SERVED_NAME.to_string()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params.signed_by(&key, &issuer).unwrap();
        Self {
            issuer,
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        }
    }

    /// A client leaf as `platform` issues one: `commonName: <module>-caller`.
    fn client(&self, common_name: &str) -> Identity {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![common_name.to_string()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        Identity::from_pem(cert.pem(), key.serialize_pem())
    }
}

/// A PEM file removed on drop, named per test so parallel cases never share.
struct TempPem(PathBuf);

impl TempPem {
    fn new(test: &str, what: &str, body: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pdb-sync-caller-{}-{test}-{what}.pem",
            std::process::id()
        ));
        std::fs::write(&path, body).unwrap();
        Self(path)
    }
}

impl Drop for TempPem {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Serve the REAL `ProjectDbService` over `db`, through `boot::server`, under
/// `required`. Returns the port and the files the listener reads.
async fn serve(test: &str, pki: &Pki, db: ProjectDb) -> (u16, [TempPem; 3]) {
    let cert = TempPem::new(test, "cert", &pki.cert_pem);
    let key = TempPem::new(test, "key", &pki.key_pem);
    let ca = TempPem::new(test, "ca", &pki.issuer.pem());
    let (c, k, a) = (
        cert.0.display().to_string(),
        key.0.display().to_string(),
        ca.0.display().to_string(),
    );
    let tls = boot::listener_tls(|name| match name {
        "LISTEN_TLS_ENABLED" => Some("1".to_string()),
        "LISTEN_TLS_CERT_FILE" => Some(c.clone()),
        "LISTEN_TLS_KEY_FILE" => Some(k.clone()),
        "LISTEN_TLS_CLIENT_AUTH" => Some("required".to_string()),
        "LISTEN_TLS_CLIENT_CA_FILE" => Some(a.clone()),
        _ => None,
    })
    .unwrap()
    .unwrap();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = boot::server(Some(&tls), &limits())
        .unwrap()
        .add_service(ProjectDbServiceServer::new(db));
    tokio::spawn(async move {
        let _ = router
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await;
    });
    (port, [cert, key, ca])
}

/// Dial as the holder of `identity` and send one `SyncProjects`.
async fn sync_over_tls(
    port: u16,
    pki: &Pki,
    identity: Identity,
    req: SyncProjectsRequest,
) -> Result<SyncProjectsResponse, tonic::Status> {
    unary_over_tls(
        port,
        pki,
        identity,
        req,
        "/yadgar.project.v1.ProjectDbService/SyncProjects",
    )
    .await
}

/// Dial as the holder of `identity` and send one `RegisterProject`.
async fn register_over_tls(
    port: u16,
    pki: &Pki,
    identity: Identity,
    req: RegisterProjectRequest,
) -> Result<RegisterProjectResponse, tonic::Status> {
    unary_over_tls(
        port,
        pki,
        identity,
        req,
        "/yadgar.project.v1.ProjectDbService/RegisterProject",
    )
    .await
}

/// Dial as the holder of `identity` and send one unary call to `path`.
async fn unary_over_tls<Req, Resp>(
    port: u16,
    pki: &Pki,
    identity: Identity,
    req: Req,
    path: &'static str,
) -> Result<Resp, tonic::Status>
where
    Req: prost::Message + Send + Sync + 'static,
    Resp: prost::Message + Default + Send + Sync + 'static,
{
    let channel: Channel = Endpoint::from_shared(format!("https://127.0.0.1:{port}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(10))
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(pki.issuer.pem()))
                .domain_name(SERVED_NAME)
                .identity(identity),
        )
        .unwrap()
        .connect()
        .await
        .expect("the handshake completes");
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.unwrap();
    grpc.unary(
        tonic::Request::new(req),
        PathAndQuery::from_static(path),
        tonic_prost::ProstCodec::default(),
    )
    .await
    .map(tonic::Response::into_inner)
}

/// `project`'s leaf reaches the store, and the five counts come back.
#[tokio::test]
async fn project_s_leaf_is_admitted_and_the_sync_applies() {
    let w = World::fresh("project_db_sync_caller_admitted").await;
    let pki = Pki::new();
    let (port, _files) = serve("admitted", &pki, ProjectDb::new(w.pool.clone())).await;

    let req = w.sync_request(
        &[declared("tapir-1", "", "tapir-1/registry")],
        &[declared("tapir-1/svc", "", "tapir-1/svc")],
        false,
    );
    let r = sync_over_tls(port, &pki, pki.client("project-caller"), req)
        .await
        .expect("project-caller is admitted");
    assert_eq!((r.inserted, r.unchanged), (2, 0));
    assert_eq!(w.active_org_paths().await, ["tapir-1", "tapir-1/svc"]);
}

/// Every OTHER verifying leaf is refused PERMISSION_DENIED — the seeder's
/// included, because the seeder calls `project`, never this store. The pool
/// points nowhere: a check that ran after `begin()` would answer from the
/// pool instead.
#[tokio::test]
async fn every_other_verifying_leaf_is_refused_before_the_store() {
    let pki = Pki::new();
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .acquire_timeout(Duration::from_secs(1))
        .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/nowhere")
        .unwrap();
    let (port, _files) = serve("refused", &pki, ProjectDb::new(pool)).await;

    for name in [
        "gateway-caller",
        "task-caller",
        "iam-caller",
        "seeder-caller",
    ] {
        let req = SyncProjectsRequest {
            scope: None,
            roots: vec![declared("tapir-1", "", "tapir-1/registry")],
            projects: vec![],
            allow_mass_archive: false,
            allow_mass_revoke: false,
        };
        let err = sync_over_tls(port, &pki, pki.client(name), req)
            .await
            .expect_err(name);
        assert_eq!(err.code(), Code::PermissionDenied, "{name}: {err:?}");
        assert!(err.message().contains(name), "{name}: {}", err.message());
    }
}

// ---------------------------------------------------------------------------
// RegisterProject (ADR-1007, yaadgaar #216)
// ---------------------------------------------------------------------------

/// A private path [`U1`] may register: its own account segment.
fn own_private_path() -> String {
    format!("local/{U1_SEG}/sync-caller-probe")
}

/// `project`'s leaf reaches the store, and the row is written.
///
/// **THE MUTATION THAT REDS IT:** compare against any other name in
/// `write::register::admit`.
#[tokio::test]
async fn project_s_leaf_is_admitted_and_the_registration_applies() {
    let w = World::fresh("project_db_register_caller_admitted").await;
    let pki = Pki::new();
    let (port, _files) = serve("register-admitted", &pki, ProjectDb::new(w.pool.clone())).await;

    let path = own_private_path();
    let req = w.registration(&path, "", U1, None, "");
    let r = register_over_tls(port, &pki, pki.client("project-caller"), req)
        .await
        .expect("project-caller is admitted");
    assert_eq!(r.meta.expect("meta").project_id, path);
    assert_eq!(w.project_count().await, 1);
}

/// Every OTHER verifying leaf is refused PERMISSION_DENIED, the seeder's and
/// the gateway's included. The pool points nowhere: a check that ran after
/// `begin()` would answer from the pool instead, and a check that ran after
/// `scope_of` would answer INVALID_ARGUMENT for the absent Scope sent here.
///
/// **THE MUTATION THAT REDS IT:** delete the `admit` call in
/// `write::register::register_as`.
#[tokio::test]
async fn every_other_verifying_leaf_is_refused_registration_before_the_store() {
    let pki = Pki::new();
    let (port, _files) = serve("register-refused", &pki, ProjectDb::new(nowhere())).await;

    for name in [
        "gateway-caller",
        "task-caller",
        "iam-caller",
        "seeder-caller",
    ] {
        let req = RegisterProjectRequest {
            idempotency: None,
            scope: None,
            path: own_private_path(),
            display_name: String::new(),
            source_repo: String::new(),
        };
        let err = register_over_tls(port, &pki, pki.client(name), req)
            .await
            .expect_err(name);
        assert_eq!(err.code(), Code::PermissionDenied, "{name}: {err:?}");
        assert!(err.message().contains(name), "{name}: {}", err.message());
        assert!(
            err.message().contains("ADR-1007"),
            "{name}: {}",
            err.message()
        );
    }
}

/// A probe registration with no Scope: a check placed after `scope_of` would
/// answer INVALID_ARGUMENT instead of PERMISSION_DENIED.
fn probe() -> RegisterProjectRequest {
    RegisterProjectRequest {
        idempotency: None,
        scope: None,
        path: own_private_path(),
        display_name: String::new(),
        source_repo: String::new(),
    }
}

/// NO caller at all, through the gRPC HANDLER: a request with no peer chain,
/// which is what a listener at `clientAuth: "off"` or `optional` hands it. A
/// `required` listener cannot produce this case over the wire, so it is driven
/// in-process.
///
/// **THE MUTATION THAT REDS IT:** a handler that defaults a missing caller,
/// `caller.as_deref().or(Some(REGISTER_CALLER))`. A test of `register_as(None)`
/// alone stays green under that mutation, because it never enters the handler.
#[tokio::test]
async fn the_handler_refuses_a_request_with_no_peer_certificate() {
    use yadgar_project_db::pb::yadgar::project::v1::project_db_service_server::ProjectDbService as _;

    let err = ProjectDb::new(nowhere())
        .register_project(tonic::Request::new(probe()))
        .await
        .expect_err("no caller");
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert!(err.message().contains("no caller"), "{}", err.message());
}

/// The comparison is EXACT: no case fold, no trim, and an empty name is no
/// name. The same table `tests/sync.rs` holds for `SyncProjects`.
#[tokio::test]
async fn register_admits_the_exact_name_only() {
    let db = ProjectDb::new(nowhere());
    for caller in [
        None,
        Some("PROJECT-CALLER"),
        Some(" project-caller"),
        Some(""),
    ] {
        let err = db.register_as(caller, probe()).await.unwrap_err();
        assert_eq!(
            err.code(),
            Code::PermissionDenied,
            "{caller:?} must be refused before the pool: {err:?}"
        );
    }
}

/// A pool that points nowhere: any statement fails, so a refusal that came
/// from the store would not be PERMISSION_DENIED.
fn nowhere() -> sqlx::MySqlPool {
    sqlx::mysql::MySqlPoolOptions::new()
        .acquire_timeout(Duration::from_secs(1))
        .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/nowhere")
        .unwrap()
}
