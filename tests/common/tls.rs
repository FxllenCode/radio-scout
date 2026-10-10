//! A certificate authority of the test's own (#76), for driving **built-in TLS**
//! without a CA on the internet.
//!
//! An Operator's own certificate files are the source that needs nothing else
//! running, so they are how the listener, the LAN door and the `Secure` cookie
//! are proven: [`TestCa`] issues a chain and a key for [`NAME`], an app is
//! started on those files, and [`TestCa::client`] is a browser that trusts this
//! CA and nothing else — so a handshake that succeeds is one the Instance
//! answered with *this* certificate. ACME, which needs a CA that answers, is
//! `tests/acme.rs`'s.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};

/// The name every test certificate is issued for, and the one the client
/// resolves to the Instance's HTTPS port — a real name rather than `localhost`,
/// so the hostname check is a check.
pub const NAME: &str = "scanner.test";

/// A root that signs leaves for the tests that need one.
pub struct TestCa {
    issuer: Issuer<'static, KeyPair>,
    root_pem: String,
}

/// One issued certificate: its chain and key as an Operator would have them on
/// disk, and the leaf's DER — which is what a test compares against what the
/// Instance actually served.
pub struct Issued {
    pub chain_pem: String,
    pub key_pem: String,
    pub leaf_der: Vec<u8>,
}

impl Issued {
    /// Write this certificate where `[tls] cert_file` and `key_file` will
    /// find it, and answer with the two paths.
    pub fn write_to(&self, dir: &Path) -> (PathBuf, PathBuf) {
        let cert = dir.join("fullchain.pem");
        let key = dir.join("privkey.pem");
        std::fs::write(&cert, &self.chain_pem).expect("write the chain");
        std::fs::write(&key, &self.key_pem).expect("write the key");
        (cert, key)
    }
}

impl TestCa {
    pub fn new() -> Self {
        let key = KeyPair::generate().expect("a CA key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Radio-Scout test CA");
        let root = params.self_signed(&key).expect("a self-signed root");
        TestCa {
            root_pem: root.pem(),
            issuer: Issuer::new(params, key),
        }
    }

    /// A leaf for `names`, signed by this CA.
    pub fn issue(&self, names: &[&str]) -> Issued {
        let key = KeyPair::generate().expect("a leaf key");
        let params = CertificateParams::new(
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
        .expect("leaf params");
        let leaf = params.signed_by(&key, &self.issuer).expect("a signed leaf");
        Issued {
            chain_pem: format!("{}{}", leaf.pem(), self.root_pem),
            key_pem: key.serialize_pem(),
            leaf_der: leaf.der().to_vec(),
        }
    }

    /// The root, as PEM.
    pub fn root_pem(&self) -> &str {
        &self.root_pem
    }

    /// A client that trusts this CA alone, resolves [`NAME`] to `https`, keeps
    /// cookies like a browser, and reports which certificate it was shown.
    pub fn client(&self, https: SocketAddr) -> reqwest::Client {
        reqwest::Client::builder()
            .tls_certs_only([
                reqwest::Certificate::from_pem(self.root_pem.as_bytes()).expect("the root")
            ])
            .resolve(NAME, https)
            .tls_info(true)
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("an https client")
    }
}

/// Open a WebSocket over TLS to `path` and hand back the first frame the
/// server sends, parsed.
///
/// By hand rather than through the harness's `Ws`, which is the plain-socket
/// type: the TLS half is `tokio-rustls`, trusting [`TestCa`] alone and asking
/// for [`NAME`], and the WebSocket half is tungstenite over whatever stream it
/// is given — the same upgrade a browser on `https://` makes.
pub async fn wss_greeting(ca: &TestCa, https: SocketAddr, path: &str) -> serde_json::Value {
    use futures_util::StreamExt;
    use rustls_pki_types::pem::PemObject;

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(
            rustls_pki_types::CertificateDer::from_pem_slice(ca.root_pem.as_bytes())
                .expect("the root"),
        )
        .expect("trust the root");
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(https)
        .await
        .expect("connect");
    let tls = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config))
        .connect(
            rustls_pki_types::ServerName::try_from(NAME).expect("a name"),
            tcp,
        )
        .await
        .expect("a TLS handshake");
    let (mut ws, _) = tokio_tungstenite::client_async(format!("wss://{NAME}{path}"), tls)
        .await
        .expect("a WebSocket upgrade over TLS");
    loop {
        match ws.next().await.expect("a frame").expect("not an error") {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                return serde_json::from_str(text.as_str()).expect("json");
            }
            _ => continue,
        }
    }
}

/// The certificate the server presented on this response.
pub fn served_certificate(response: &reqwest::Response) -> Vec<u8> {
    response
        .extensions()
        .get::<reqwest::tls::TlsInfo>()
        .and_then(|info| info.peer_certificate())
        .expect("a TLS response names its certificate")
        .to_vec()
}
