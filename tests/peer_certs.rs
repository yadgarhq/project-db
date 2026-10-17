//! THE MEASUREMENT `plans/the-d35-seeder.md` stage 2 requires before any caller
//! check is written: does a handler behind `yadgar_lifecycle::serve_tls` — the
//! listener `main` builds through `boot::server` — see the client's certificate
//! chain at all?
//!
//! `SyncProjects` admits only `project`'s leaf (ADR-0936), so its check reads
//! the peer certificate. tonic's `Request::peer_certs()` reads one request
//! extension, `TlsConnectInfo<TcpConnectInfo>`, and that extension is present
//! only when tonic's own TLS acceptor wrapped a `TcpStream` incoming. A listener
//! that served TLS correctly through any other path would still hand every
//! handler `None`, and the check would refuse every caller — or, written
//! carelessly, admit every one. So this asks the question over a REAL handshake,
//! through the SAME `boot::server` and the SAME incoming type `main` serves
//! (`serve_with_shutdown(addr, ..)` binds a `TcpIncoming`, whose items are
//! `TcpStream`), with a handler that ECHOES what it saw rather than a route
//! table that answers `Unimplemented` to everything.
//!
//! NEITHER AN ENGINE NOR A POOL IS INVOLVED, like `tests/serve_tls.rs`.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::codegen::{http, Service};
use tonic::server::NamedService;
use tonic::transport::server::{TcpConnectInfo, TlsConnectInfo};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

use yadgar_project_db::boot;

const SERVED_NAME: &str = "localhost";

/// The header the echo handler answers with: how many certificates the
/// request's peer chain held, or `none` when the extension was absent.
const ECHO: &str = "x-peer-certs";

/// A service that answers every request with the size of the peer chain it was
/// handed. Named as `ProjectDbService`, so it is routed exactly where the real
/// one is.
#[derive(Clone)]
struct Echo;

impl NamedService for Echo {
    const NAME: &'static str = "yadgar.project.v1.ProjectDbService";
}

impl Service<http::Request<tonic::body::Body>> for Echo {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        // THE EXTENSION `Request::peer_certs()` READS, read the same way.
        let seen = req
            .extensions()
            .get::<TlsConnectInfo<TcpConnectInfo>>()
            .and_then(|info| info.peer_certs())
            .map(|chain| chain.len().to_string())
            .unwrap_or_else(|| "none".to_string());
        Box::pin(async move {
            Ok(http::Response::builder()
                .status(200)
                .header(ECHO, seen)
                .body(tonic::body::Body::empty())
                .unwrap())
        })
    }
}

struct Pki {
    ca_pem: String,
    cert_pem: String,
    key_pem: String,
    client_cert_pem: String,
    client_key_pem: String,
}

fn pki() -> Pki {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "peer-certs measurement authority");
    let ca = CertifiedIssuer::self_signed(ca_params, ca_key).unwrap();

    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![SERVED_NAME.to_string()]).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.signed_by(&key, &ca).unwrap();

    // THE SHAPE `platform` ISSUES: a common name, no SAN, `clientAuth`.
    let client_key = KeyPair::generate().unwrap();
    let mut client_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    client_params
        .distinguished_name
        .push(DnType::CommonName, "project-caller");
    let client_cert = client_params.signed_by(&client_key, &ca).unwrap();

    Pki {
        ca_pem: ca.pem(),
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        client_cert_pem: client_cert.pem(),
        client_key_pem: client_key.serialize_pem(),
    }
}

fn temp(name: &str, body: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("pdb-peer-certs-{}-{name}.pem", std::process::id()));
    std::fs::write(&path, body).unwrap();
    path
}

#[tokio::test]
async fn a_handler_behind_serve_tls_sees_the_client_chain() {
    let p = pki();
    let cert = temp("cert", &p.cert_pem);
    let key = temp("key", &p.key_pem);
    let ca = temp("ca", &p.ca_pem);
    let (cert_s, key_s, ca_s) = (
        cert.display().to_string(),
        key.display().to_string(),
        ca.display().to_string(),
    );
    let tls = boot::listener_tls(|k| match k {
        "LISTEN_TLS_ENABLED" => Some("1".to_string()),
        "LISTEN_TLS_CERT_FILE" => Some(cert_s.clone()),
        "LISTEN_TLS_KEY_FILE" => Some(key_s.clone()),
        "LISTEN_TLS_CLIENT_AUTH" => Some("required".to_string()),
        "LISTEN_TLS_CLIENT_CA_FILE" => Some(ca_s.clone()),
        _ => None,
    })
    .unwrap()
    .unwrap();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = boot::server(Some(&tls)).unwrap().add_service(Echo);
    tokio::spawn(async move {
        let _ = router
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await;
    });

    let channel = Endpoint::from_shared(format!("https://127.0.0.1:{port}"))
        .unwrap()
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&p.ca_pem))
                .domain_name(SERVED_NAME)
                .identity(Identity::from_pem(&p.client_cert_pem, &p.client_key_pem)),
        )
        .unwrap()
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("the handshake completes");

    let req = http::Request::builder()
        .version(http::Version::HTTP_2)
        .method("POST")
        .uri(format!(
            "https://{SERVED_NAME}/yadgar.project.v1.ProjectDbService/SyncProjects"
        ))
        .header("content-type", "application/grpc")
        .body(tonic::body::Body::empty())
        .unwrap();
    let mut channel = channel;
    std::future::poll_fn(|cx| channel.poll_ready(cx))
        .await
        .unwrap();
    let response = channel.call(req).await.expect("answered");
    let seen = response
        .headers()
        .get(ECHO)
        .expect("the echo header")
        .to_str()
        .unwrap()
        .to_string();

    for f in [cert, key, ca] {
        let _ = std::fs::remove_file(f);
    }
    // MEASURED: one certificate — the client's leaf. rustls hands the chain the
    // client sent, and a client presenting a leaf alone sends a chain of one.
    assert_eq!(
        seen, "1",
        "a handler behind boot::server must see the caller's certificate chain; it saw {seen}"
    );
}
