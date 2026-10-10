//! A certificate this Instance can serve: a PEM chain and key, checked against
//! each other, with the one fact about it everything else needs — when it
//! expires.
//!
//! **One loader for both sources.** An Operator's own files and an ACME
//! issuance arrive as the same two PEM documents, so they are read by the same
//! function and refused for the same reasons: no certificate, no key, or a key
//! that is not the certificate's — the last being the mistake rdio-scanner
//! leaves for Go's `tls.LoadX509KeyPair` to report as a crash at boot.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::sign::CertifiedKey;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

/// A chain and key that belong together, ready to hand a handshake.
#[derive(Clone, Debug)]
pub struct Certificate {
    pub key: Arc<CertifiedKey>,
    /// When the leaf stops being valid, in unix milliseconds.
    pub not_after_ms: i64,
    /// ...and when it started being, for the schedule's share of its lifetime.
    pub not_before_ms: i64,
}

impl Certificate {
    /// Parse a PEM chain (leaf first) and its PEM private key.
    pub fn from_pem(chain: &[u8], key: &[u8]) -> Result<Self, &'static str> {
        let chain = CertificateDer::pem_slice_iter(chain)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "the certificate is not PEM")?;
        let Some(leaf) = chain.first() else {
            return Err("no certificate in it");
        };
        // A certificate whose dates cannot be read is refused rather than
        // served on hope: nothing could say when it expires, so nothing could
        // renew it in time or warn that it was about to stop working.
        let (not_before_ms, not_after_ms) =
            validity_ms(leaf).ok_or("the certificate's dates cannot be read")?;
        let key = PrivateKeyDer::from_pem_slice(key).map_err(|_| "no private key in it")?;
        let certified = CertifiedKey::from_der(chain, key, &super::provider())
            .map_err(|_| "the private key does not belong to the certificate")?;
        Ok(Certificate {
            key: Arc::new(certified),
            not_after_ms,
            not_before_ms,
        })
    }

    /// The leaf, as the CA issued it.
    pub fn leaf(&self) -> &CertificateDer<'static> {
        &self.key.cert[0]
    }
}

/// When a certificate is valid, in unix milliseconds.
fn validity_ms(leaf: &CertificateDer<'_>) -> Option<(i64, i64)> {
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref()).ok()?;
    let validity = parsed.validity();
    Some((
        validity.not_before.timestamp() * 1000,
        validity.not_after.timestamp() * 1000,
    ))
}

/// Why an Operator's certificate files could not be served — the file, and
/// what was wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub path: PathBuf,
    pub why: String,
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not load {}: {}", self.path.display(), self.why)
    }
}

impl std::error::Error for Unreadable {}

/// Read an Operator's chain and key from disk.
///
/// The error names the **file**, because that is what the Operator edits: a
/// missing path names itself, and a parse failure names the certificate (a key
/// that does not match it is a property of the pair, and the certificate is the
/// half an Operator renews).
pub fn read(cert: &Path, key: &Path) -> Result<Certificate, Unreadable> {
    let pem = read_pem(cert, key)?;
    parse(cert, &pem.chain, &pem.key)
}

/// A certificate file and its key file, as read — bytes, not yet a
/// certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pem {
    pub chain: Vec<u8>,
    pub key: Vec<u8>,
}

/// The two files' bytes, or which of them could not be read.
pub fn read_pem(cert: &Path, key: &Path) -> Result<Pem, Unreadable> {
    let read = |path: &Path| {
        std::fs::read(path).map_err(|error| Unreadable {
            path: path.to_path_buf(),
            why: error.to_string(),
        })
    };
    Ok(Pem {
        chain: read(cert)?,
        key: read(key)?,
    })
}

/// A root certificate to trust — `[tls] directory_root` — read and parsed, or
/// why it could not be.
pub fn read_root(path: &Path) -> Result<(), Unreadable> {
    let unreadable = |why: String| Unreadable {
        path: path.to_path_buf(),
        why,
    };
    let pem = std::fs::read(path).map_err(|error| unreadable(error.to_string()))?;
    CertificateDer::from_pem_slice(&pem)
        .map(|_| ())
        .map_err(|_| unreadable("no certificate in it".to_string()))
}

/// The pair read from `cert`'s file and its key's, as something to serve.
pub fn parse(cert: &Path, chain: &[u8], key: &[u8]) -> Result<Certificate, Unreadable> {
    Certificate::from_pem(chain, key).map_err(|why| Unreadable {
        path: cert.to_path_buf(),
        why: why.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A leaf for `scanner.example` and its key, as PEM.
    fn pair() -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("a key");
        let leaf = rcgen::CertificateParams::new(vec!["scanner.example".to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("a certificate");
        (leaf.pem(), key.serialize_pem())
    }

    /// Bytes wrapped as a certificate that are not one — PEM that parses, DER
    /// that does not.
    const NOT_DER: &str =
        "-----BEGIN CERTIFICATE-----\nbm90IGEgY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n";

    #[test]
    fn a_matching_pair_is_served_with_its_dates() {
        let (chain, key) = pair();

        let certificate = Certificate::from_pem(chain.as_bytes(), key.as_bytes()).expect("serves");

        assert!(certificate.not_before_ms < certificate.not_after_ms);
    }

    /// Every way an Operator's pair can be wrong is refused, each in words that
    /// say which.
    #[rstest]
    #[case::no_certificate("", None, "no certificate")]
    #[case::not_pem(
        "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n",
        None,
        "not PEM"
    )]
    #[case::not_a_certificate(NOT_DER, None, "dates cannot be read")]
    #[case::no_key_in_the_key_file("", Some(""), "no private key")]
    #[case::someone_elses_key("", Some("other"), "does not belong")]
    fn a_pair_that_cannot_be_served_says_why(
        #[case] chain: &str,
        #[case] key: Option<&str>,
        #[case] says: &str,
    ) {
        let (good_chain, good_key) = pair();
        let chain = match chain {
            "" if key.is_some() => good_chain,
            chain => chain.to_string(),
        };
        let key = match key {
            None => good_key,
            Some("") => String::new(),
            Some(_) => pair().1,
        };

        let refused = Certificate::from_pem(chain.as_bytes(), key.as_bytes()).expect_err("refused");

        assert!(refused.contains(says), "{refused:?} should say {says:?}");
    }

    /// A private CA's root is checked at boot: there, readable, and a
    /// certificate — each failure naming the file.
    #[test]
    fn a_root_is_a_readable_certificate_or_says_why_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (root, junk, missing) = (
            dir.path().join("root.pem"),
            dir.path().join("junk.pem"),
            dir.path().join("missing.pem"),
        );
        std::fs::write(&root, pair().0).expect("write");
        std::fs::write(&junk, "not a certificate").expect("write");

        assert!(read_root(&root).is_ok());
        let no_pem = read_root(&junk).expect_err("junk").to_string();
        assert!(
            no_pem.contains("junk.pem") && no_pem.contains("no certificate"),
            "{no_pem}"
        );
        let gone = read_root(&missing).expect_err("missing").to_string();
        assert!(gone.contains("missing.pem"), "{gone}");
    }

    /// Read from disk, a pair that will not serve names the certificate's file
    /// — the half of the pair an Operator renews.
    #[test]
    fn a_file_that_will_not_serve_names_the_certificate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert, key) = (
            dir.path().join("fullchain.pem"),
            dir.path().join("privkey.pem"),
        );
        std::fs::write(&cert, NOT_DER).expect("write");
        std::fs::write(&key, pair().1).expect("write");

        let refused = read(&cert, &key).expect_err("refused").to_string();

        assert!(refused.contains("fullchain.pem"), "{refused}");
        assert!(refused.contains("dates cannot be read"), "{refused}");
    }
}
