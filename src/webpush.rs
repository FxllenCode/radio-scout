//! Web Push over the wire: RFC 8291 message encryption and RFC 8292 VAPID
//! authorization.
//!
//! This is the protocol half of #16, kept apart from the domain half
//! ([`crate::push`]) because it is entirely pure: bytes in, bytes out, no
//! database, no clock of its own, no network. Everything it produces is pinned
//! to the RFCs' own worked examples.

use base64::Engine;
use p256::elliptic_curve::sec1::ToEncodedPoint;

/// base64url without padding — the only encoding Web Push uses, for keys, for
/// the JWT, and for the values a browser hands us.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A subscriber we are encrypting for: the browser's public key and the
/// authentication secret it generated alongside it.
///
/// The auth secret is what keeps a push service from reading the messages it
/// carries, so `Debug` redacts it for the same reason [`VapidKey`]'s does.
pub struct Recipient {
    pub public_key: p256::PublicKey,
    pub auth: [u8; 16],
}

impl std::fmt::Debug for Recipient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recipient")
            .field("public_key", &self.public_key)
            .field("auth", &"<redacted>")
            .finish()
    }
}

/// A browser's push subscription, as it comes back from
/// `PushSubscription.toJSON()` and as we store it: where to deliver, and the
/// keys to encrypt for.
#[derive(Debug)]
pub struct Subscription {
    pub endpoint: String,
    pub recipient: Recipient,
}

/// One message to deliver, before it is encrypted.
pub struct Message<'a> {
    pub payload: &'a [u8],
    /// How long the push service should hold it for a device that is offline.
    pub ttl: std::time::Duration,
    /// RFC 8030 §5.4: a later message with the same topic **replaces** an
    /// undelivered earlier one. A phone that was off for an hour then wakes to
    /// one notification per Talkgroup rather than to a queue of them — the
    /// half of "no storms" that our own coalescing cannot reach, because by
    /// then the message has already left.
    pub topic: Option<&'a str>,
}

impl Subscription {
    /// Read a subscription as a browser reports it: the endpoint URL, and the
    /// `keys.p256dh` / `keys.auth` values from `PushSubscription.toJSON()`.
    ///
    /// Everything a message needs is validated **here**, before a row exists,
    /// so a stored subscription is one we can always deliver to — and a browser
    /// sending nonsense is told at subscribe time rather than silently never
    /// notified.
    pub fn parse(endpoint: &str, p256dh: &str, auth: &str) -> Result<Self, InvalidSubscription> {
        audience_of(endpoint).ok_or(InvalidSubscription::Endpoint)?;
        let public_key = B64
            .decode(p256dh)
            .ok()
            .and_then(|bytes| p256::PublicKey::from_sec1_bytes(&bytes).ok())
            .ok_or(InvalidSubscription::PublicKey)?;
        let auth: [u8; 16] = B64
            .decode(auth)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(InvalidSubscription::Auth)?;

        Ok(Subscription {
            endpoint: endpoint.to_string(),
            recipient: Recipient { public_key, auth },
        })
    }
}

/// Which part of a browser's subscription was unusable. The name doubles as the
/// `reason` on the WARN line that rejects it (ADR-0011 rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidSubscription {
    /// Not an absolute URL, so there is no origin to audience a token to.
    Endpoint,
    /// Not an uncompressed P-256 point.
    PublicKey,
    /// Not the 16 bytes RFC 8291 requires.
    Auth,
}

impl InvalidSubscription {
    /// A machine-readable slug for the log line and the response body.
    pub fn reason(self) -> &'static str {
        match self {
            InvalidSubscription::Endpoint => "bad-endpoint",
            InvalidSubscription::PublicKey => "bad-key",
            InvalidSubscription::Auth => "bad-auth",
        }
    }
}

impl std::fmt::Display for InvalidSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// A request to POST to a push service (RFC 8030). Built rather than sent, so
/// everything about the wire format is decided here and testable without a
/// network.
#[derive(Debug, PartialEq, Eq)]
pub struct PushRequest {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

/// How long a VAPID token stays valid. RFC 8292 §2 caps it at 24 hours; half
/// that leaves room for a device with a skewed clock at either end without
/// minting a token that outlives the day it was made for.
const TOKEN_LIFETIME_SECS: i64 = 12 * 60 * 60;

/// The origin of a push endpoint — the `aud` claim a token is minted for
/// (RFC 8292 §2), so a token issued for one push service is not replayable
/// against another.
///
/// Hand-parsed rather than pulling a URL crate in for one field: everything
/// before the first `/` after the scheme, and nothing believed unless both a
/// scheme and a host are there.
pub fn audience_of(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let host = rest.split('/').next().unwrap_or(rest);
    match host.is_empty() {
        true => None,
        false => Some(format!("{scheme}://{host}")),
    }
}

/// The one record size we emit (RFC 8188 `rs`). A push service will not carry
/// more than 4 KB of payload anyway, so a single record always suffices — and a
/// message that would not fit is refused by [`seal`]'s caller rather than split.
const RECORD_SIZE: u32 = 4096;

/// Encrypt `plaintext` for `recipient` under RFC 8291, returning the complete
/// `aes128gcm` body (RFC 8188 header, then one record).
///
/// `sender` is the **ephemeral** keypair for this one message and `salt` is
/// fresh randomness; both are parameters rather than generated here so the
/// RFC's worked example can be reproduced exactly.
pub fn seal(
    recipient: &Recipient,
    sender: &p256::SecretKey,
    salt: [u8; 16],
    plaintext: &[u8],
) -> Vec<u8> {
    let sender_public = sender.public_key().to_encoded_point(false);
    let recipient_public = recipient.public_key.to_encoded_point(false);

    // RFC 8291 §3.4: combine the ECDH secret with the subscription's auth
    // secret, keyed so that neither alone is enough to read the message.
    let shared =
        p256::ecdh::diffie_hellman(sender.to_nonzero_scalar(), recipient.public_key.as_affine());
    let mut key_info = Vec::with_capacity(14 + 65 + 65);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(recipient_public.as_bytes());
    key_info.extend_from_slice(sender_public.as_bytes());
    let ikm: [u8; 32] = expand(&recipient.auth, shared.raw_secret_bytes(), &key_info);

    // RFC 8188 §2.2: the content-encryption key and nonce, from the same salt
    // that rides in the header.
    let cek: [u8; 16] = expand(&salt, &ikm, b"Content-Encoding: aes128gcm\0");
    let nonce: [u8; 12] = expand(&salt, &ikm, b"Content-Encoding: nonce\0");

    // One record, so the padding delimiter is the last-record one.
    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    let ciphertext = {
        use aes_gcm::KeyInit;
        use aes_gcm::aead::Aead;
        aes_gcm::Aes128Gcm::new(&cek.into())
            .encrypt(&nonce.into(), record.as_slice())
            // The only documented failure is a plaintext beyond AES-GCM's
            // 64 GiB limit, which a 4 KB push payload cannot reach.
            .expect("aes128gcm encryption of one short record")
    };

    let mut body = Vec::with_capacity(21 + 65 + ciphertext.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(sender_public.as_bytes().len() as u8);
    body.extend_from_slice(sender_public.as_bytes());
    body.extend_from_slice(&ciphertext);
    body
}

/// One HKDF-SHA256 extract-and-expand, sized by the array the caller asks for.
///
/// Both derivations RFC 8291 needs are this shape — only the salt, the input
/// keying material and the info string differ — so writing it once is what
/// keeps the two from drifting.
fn expand<const N: usize>(salt: &[u8], ikm: &[u8], info: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), ikm)
        .expand(info, &mut out)
        // Only fails past 255 hash lengths (8160 bytes); ours are 12 to 32.
        .expect("hkdf output length");
    out
}

/// A `RADIO_SCOUT_VAPID_PRIVATE_KEY` that is not a P-256 private key.
///
/// Carries no copy of the offending text: the value it failed to parse is the
/// credential itself (ADR-0011 rule 2), and the operator already knows what
/// they wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidVapidKey;

impl std::fmt::Display for InvalidVapidKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("expected a base64url-encoded 32-byte P-256 private key")
    }
}

impl std::error::Error for InvalidVapidKey {}

/// The server's VAPID identity (RFC 8292): one long-lived P-256 keypair whose
/// public half a browser pins at subscribe time, and whose private half signs
/// every push we send.
///
/// `Debug` is written by hand: the private scalar is a credential, and ADR-0011
/// rule 2 has no exception for a `{:?}` in an error chain.
pub struct VapidKey(p256::SecretKey);

impl std::fmt::Debug for VapidKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("VapidKey").field(&"<redacted>").finish()
    }
}

impl VapidKey {
    /// A fresh identity, for a first run that has none.
    pub fn generate() -> Self {
        VapidKey(p256::SecretKey::random(
            &mut p256::elliptic_curve::rand_core::OsRng,
        ))
    }

    /// Read the key back from the text form [`VapidKey::secret_base64url`]
    /// wrote — a `.env` line, or a value an operator moved between installs.
    pub fn parse(text: &str) -> Result<Self, InvalidVapidKey> {
        let bytes = B64.decode(text.trim()).map_err(|_| InvalidVapidKey)?;
        // `from_slice` is what rejects both a wrong length and a scalar outside
        // the curve's order.
        p256::SecretKey::from_slice(&bytes)
            .map(VapidKey)
            .map_err(|_| InvalidVapidKey)
    }

    /// The private scalar, base64url — **a credential**. It goes to the env
    /// file and nowhere else (ADR-0011 rule 2).
    pub fn secret_base64url(&self) -> String {
        B64.encode(self.0.to_bytes())
    }

    /// The public key a browser passes as `applicationServerKey`: the
    /// uncompressed SEC1 point, base64url.
    pub fn public_base64url(&self) -> String {
        B64.encode(self.0.public_key().to_encoded_point(false).as_bytes())
    }

    /// The complete request that delivers `message` to `to` — the endpoint's
    /// URL, the RFC 8030 headers, and the RFC 8291 encrypted body.
    ///
    /// `subject` is the VAPID `sub` claim: how a push service's operator
    /// contacts ours if this server misbehaves.
    pub fn request(
        &self,
        subject: &str,
        to: &Subscription,
        message: Message<'_>,
        now_ms: i64,
    ) -> PushRequest {
        let mut salt = [0u8; 16];
        p256::elliptic_curve::rand_core::RngCore::fill_bytes(
            &mut p256::elliptic_curve::rand_core::OsRng,
            &mut salt,
        );
        let ephemeral = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);

        // An endpoint whose origin we cannot read is refused before it is
        // stored (`Subscription::parse`), so falling back to the endpoint
        // itself here is unreachable rather than lenient — and it keeps this
        // from being a `Result` every caller would have to unwrap.
        let audience = audience_of(&to.endpoint).unwrap_or_else(|| to.endpoint.clone());
        let mut headers = vec![
            ("TTL", message.ttl.as_secs().to_string()),
            ("Content-Encoding", "aes128gcm".to_string()),
            ("Content-Type", "application/octet-stream".to_string()),
            (
                "Authorization",
                self.authorization(&audience, subject, now_ms / 1000 + TOKEN_LIFETIME_SECS),
            ),
        ];
        if let Some(topic) = message.topic {
            headers.push(("Topic", topic.to_string()));
        }

        PushRequest {
            url: to.endpoint.clone(),
            headers,
            body: seal(&to.recipient, &ephemeral, salt, message.payload),
        }
    }

    /// The `Authorization` header value for a push to `audience` (the origin of
    /// the subscription's endpoint), expiring at `expires_at` unix seconds.
    ///
    /// The signature is deterministic (RFC 6979, which is what `p256`'s signer
    /// does), so the same inputs always produce the same header — a token is
    /// reproducible from a log line naming its audience and expiry, and nothing
    /// here depends on an RNG being seeded.
    pub fn authorization(&self, audience: &str, subject: &str, expires_at: i64) -> String {
        use p256::ecdsa::signature::Signer;

        // A JWS with a fixed header and three claims (RFC 8292 §2): serialized
        // by hand rather than through serde, because JWT signing is over the
        // *encoded* bytes and a re-serialization that reorders keys would
        // invalidate the signature.
        let protected = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(
            serde_json::json!({ "aud": audience, "exp": expires_at, "sub": subject }).to_string(),
        );
        let signing_input = format!("{protected}.{claims}");

        let key = p256::ecdsa::SigningKey::from(&self.0);
        let signature: p256::ecdsa::Signature = key.sign(signing_input.as_bytes());
        format!(
            "vapid t={signing_input}.{}, k={}",
            B64.encode(signature.to_bytes()),
            self.public_base64url()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::time::Duration;

    /// RFC 8291 §5's worked example, value for value. Everything the sender
    /// would otherwise choose at random — the ephemeral keypair and the salt —
    /// is fixed by the RFC, so the output is a single expected string that came
    /// from the standard rather than from this code.
    mod rfc8291 {
        pub const PLAINTEXT: &str = "When I grow up, I want to be a watermelon";
        pub const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
        pub const UA_PUBLIC: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        pub const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
        pub const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
        /// The complete encrypted body, from the POST in §5.
        pub const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";
    }

    fn decode(value: &str) -> Vec<u8> {
        B64.decode(value).expect("base64url")
    }

    /// A subscription at `endpoint`, keyed to RFC 8291's receiver.
    fn subscription(endpoint: &str) -> Subscription {
        Subscription {
            endpoint: endpoint.to_string(),
            recipient: Recipient {
                public_key: p256::PublicKey::from_sec1_bytes(&decode(rfc8291::UA_PUBLIC))
                    .expect("ua public key"),
                auth: decode(rfc8291::AUTH).try_into().expect("16-byte auth"),
            },
        }
    }

    /// The key has to survive a restart through a line of text in `.env`, or
    /// every boot would invent a new identity and silently invalidate every
    /// subscription a browser had pinned to the old one.
    #[test]
    fn a_generated_key_round_trips_through_its_text_form() {
        let key = VapidKey::generate();

        let restored = VapidKey::parse(&key.secret_base64url()).expect("a key we wrote ourselves");

        assert_eq!(restored.public_base64url(), key.public_base64url());
        assert_ne!(
            VapidKey::generate().public_base64url(),
            key.public_base64url(),
            "each generated identity must be its own"
        );
    }

    #[rstest]
    #[case("")]
    #[case("not base64!")]
    #[case("c2hvcnQ")] // valid base64url, but not 32 bytes
    fn a_key_that_is_not_a_key_is_refused(#[case] text: &str) {
        assert_eq!(VapidKey::parse(text).unwrap_err(), InvalidVapidKey);
    }

    /// The private scalar is a credential, and this type is exactly the kind of
    /// thing that ends up in a `?` chain on an ERROR line.
    #[test]
    fn debugging_a_key_never_shows_the_secret() {
        let key = VapidKey::generate();

        let rendered = format!("{key:?}");

        assert!(!rendered.contains(&key.secret_base64url()), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    /// The one thing a push service checks: the token is signed by the key the
    /// same header advertises, which is the key the browser pinned at subscribe
    /// time. Verified with the *verifier* rather than by re-signing, so the
    /// assertion cannot agree with a wrong signature by computing it the same
    /// wrong way.
    #[test]
    fn vapid_authorization_is_a_token_signed_by_the_advertised_key() {
        let key = VapidKey(
            p256::SecretKey::from_slice(&decode(rfc8291::AS_PRIVATE)).expect("a private key"),
        );

        let header = key.authorization(
            "https://push.example.net",
            "mailto:ops@example.com",
            1_700_000_000,
        );

        let (token, advertised) = header
            .strip_prefix("vapid t=")
            .expect("the vapid auth scheme")
            .split_once(", k=")
            .expect("t= and k=");
        assert_eq!(advertised, key.public_base64url());

        let mut parts = token.split('.');
        let (protected, claims, signature) = (
            parts.next().expect("header"),
            parts.next().expect("claims"),
            parts.next().expect("signature"),
        );
        assert_eq!(parts.next(), None, "a JWS has exactly three parts");

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&decode(protected)).expect("json"),
            serde_json::json!({ "typ": "JWT", "alg": "ES256" }),
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&decode(claims)).expect("json"),
            serde_json::json!({
                "aud": "https://push.example.net",
                "exp": 1_700_000_000,
                "sub": "mailto:ops@example.com",
            }),
        );

        use p256::ecdsa::signature::Verifier;
        let verifying = p256::ecdsa::VerifyingKey::from(key.0.public_key());
        let signature = p256::ecdsa::Signature::from_slice(&decode(signature))
            .expect("a 64-byte ES256 signature");
        verifying
            .verify(format!("{protected}.{claims}").as_bytes(), &signature)
            .expect("the token is signed by the advertised key");
    }

    /// A subscription is validated once, at the door: everything stored is
    /// something we can encrypt for and audience a token to.
    #[test]
    fn a_browsers_subscription_parses_into_what_a_message_needs() {
        let parsed = Subscription::parse(
            "https://push.example.net/push/abc",
            rfc8291::UA_PUBLIC,
            rfc8291::AUTH,
        )
        .expect("a well-formed subscription");

        assert_eq!(parsed.endpoint, "https://push.example.net/push/abc");
        assert_eq!(
            parsed.recipient.public_key,
            p256::PublicKey::from_sec1_bytes(&decode(rfc8291::UA_PUBLIC)).expect("key")
        );
        assert_eq!(parsed.recipient.auth.as_slice(), decode(rfc8291::AUTH));
    }

    #[rstest]
    #[case(
        "not-a-url",
        rfc8291::UA_PUBLIC,
        rfc8291::AUTH,
        InvalidSubscription::Endpoint
    )]
    #[case(
        "https://push.example.net/p",
        "not base64!",
        rfc8291::AUTH,
        InvalidSubscription::PublicKey
    )]
    // Valid base64url, but not a point on P-256.
    #[case(
        "https://push.example.net/p",
        "AAAA",
        rfc8291::AUTH,
        InvalidSubscription::PublicKey
    )]
    #[case(
        "https://push.example.net/p",
        rfc8291::UA_PUBLIC,
        "short",
        InvalidSubscription::Auth
    )]
    #[case(
        "https://push.example.net/p",
        rfc8291::UA_PUBLIC,
        "",
        InvalidSubscription::Auth
    )]
    fn an_unusable_subscription_is_refused_at_the_door(
        #[case] endpoint: &str,
        #[case] p256dh: &str,
        #[case] auth: &str,
        #[case] expected: InvalidSubscription,
    ) {
        assert_eq!(
            Subscription::parse(endpoint, p256dh, auth).unwrap_err(),
            expected
        );
    }

    /// What a push service actually receives. The body is proven decryptable
    /// end to end by `tests/push.rs` against a stub service; here the claim is
    /// about the envelope — the URL, every header the RFCs require, and that
    /// the encryption is freshly salted per message rather than reused.
    #[test]
    fn a_request_carries_the_endpoint_the_rfc_8030_headers_and_a_fresh_body() {
        let key = VapidKey::generate();
        let to = subscription("https://push.example.net/push/JzLQ3raZJfFBR0aqvOMsLrt54w4rJUsV");
        let payload = br#"{"t":"call"}"#;
        let message = || Message {
            payload,
            ttl: Duration::from_secs(60),
            topic: Some("t11-54241"),
        };

        let request = key.request("mailto:ops@example.com", &to, message(), 1_700_000_000_000);

        assert_eq!(request.url, to.endpoint);
        let header = |name| {
            request
                .headers
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("TTL"), Some("60"));
        assert_eq!(header("Content-Encoding"), Some("aes128gcm"));
        assert_eq!(header("Content-Type"), Some("application/octet-stream"));
        assert_eq!(header("Topic"), Some("t11-54241"));

        // The token is audienced to the endpoint's origin — a token minted for
        // one push service must not be replayable against another.
        let claims = header("Authorization")
            .and_then(|value| value.split('.').nth(1).map(decode))
            .expect("the vapid token's claims");
        let claims: serde_json::Value = serde_json::from_slice(&claims).expect("json");
        assert_eq!(claims["aud"], "https://push.example.net");
        // Exactly twelve hours on. A range assertion would let the lifetime
        // become nonsense — a token good for two minutes is refused by a push
        // service the moment a device's clock is a little off, and one past RFC
        // 8292's 24-hour ceiling is refused outright.
        assert_eq!(
            claims["exp"].as_i64().expect("an exp claim"),
            1_700_000_000 + 12 * 60 * 60
        );

        // A 16-byte salt, a 4-byte record size, a 1-byte key length and the
        // 65-byte ephemeral key, then the record: the payload, its padding
        // delimiter and the GCM tag.
        assert_eq!(request.body.len(), 86 + payload.len() + 1 + 16);
        let again = key.request("mailto:ops@example.com", &to, message(), 1_700_000_000_000);
        assert_ne!(
            again.body, request.body,
            "every message needs its own salt and ephemeral key"
        );
    }

    /// An endpoint whose origin can't be read cannot be audienced, and a token
    /// with a wrong `aud` is refused by the service anyway — so the failure
    /// belongs at parse time, before anything is stored.
    #[rstest]
    #[case("https://push.example.net/x", "https://push.example.net")]
    #[case("https://push.example.net:8443/x", "https://push.example.net:8443")]
    // No path at all is still an origin.
    #[case("https://push.example.net", "https://push.example.net")]
    fn the_audience_is_the_endpoint_origin(#[case] endpoint: &str, #[case] expected: &str) {
        assert_eq!(audience_of(endpoint), Some(expected.to_string()));
    }

    #[rstest]
    #[case("")]
    #[case("push.example.net/x")] // no scheme
    #[case("https://")] // no host
    #[case("://push.example.net/x")] // no scheme, but the separator is there
    fn an_endpoint_that_is_not_a_url_has_no_audience(#[case] endpoint: &str) {
        assert_eq!(audience_of(endpoint), None);
    }

    /// The auth secret is what keeps a push service from reading the messages
    /// it carries — a credential, and rule 2 has no exception for a `{:?}`.
    #[test]
    fn debugging_a_recipient_never_shows_its_auth_secret() {
        let recipient = Recipient {
            public_key: p256::PublicKey::from_sec1_bytes(&decode(rfc8291::UA_PUBLIC)).expect("key"),
            auth: decode(rfc8291::AUTH).try_into().expect("auth"),
        };

        let rendered = format!("{recipient:?}");

        assert!(!rendered.contains("BTBZ"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    /// Both refusals are things an operator or a browser has to act on, so both
    /// say what was expected — and the invalid-key one never quotes the value,
    /// because the value is the credential.
    #[test]
    fn the_refusals_explain_themselves() {
        assert_eq!(InvalidSubscription::Endpoint.to_string(), "bad-endpoint");
        assert_eq!(InvalidSubscription::PublicKey.to_string(), "bad-key");
        assert_eq!(InvalidSubscription::Auth.to_string(), "bad-auth");
        assert!(InvalidVapidKey.to_string().contains("P-256 private key"));
    }

    #[test]
    fn seals_the_rfc_8291_example() {
        let recipient = Recipient {
            public_key: p256::PublicKey::from_sec1_bytes(&decode(rfc8291::UA_PUBLIC))
                .expect("ua public key"),
            auth: decode(rfc8291::AUTH).try_into().expect("16-byte auth"),
        };
        let sender = p256::SecretKey::from_slice(&decode(rfc8291::AS_PRIVATE)).expect("as private");
        let salt: [u8; 16] = decode(rfc8291::SALT).try_into().expect("16-byte salt");

        let body = seal(&recipient, &sender, salt, rfc8291::PLAINTEXT.as_bytes());

        assert_eq!(B64.encode(body), rfc8291::BODY);
    }
}
