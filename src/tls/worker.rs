//! The TLS **Worker** (#76): keeps the certificate being served current.
//!
//! For an Operator's own files that means reading them again every
//! [`FILES_RECHECK`] and serving what changed; for ACME, issuing when there is
//! nothing worth serving and renewing when the schedule says so
//! ([`super::acme`]). Either way it is the **Delay** release Worker's shape: no
//! timer of its own, a pass that answers "when should I look next", and a sleep
//! on the Instance's [`crate::Clock`] until then — so a test passes a minute or a
//! month by moving the clock and settling, never by sleeping.

use std::path::PathBuf;

use tracing::{error, info};

use super::certificate;
use super::{FILES_RECHECK, Source, Tls};
use crate::AppState;
use crate::worker::Worker;

/// The name it is registered under on the status page.
pub const WORKER: &str = "tls";

/// Start the Worker, unless TLS is off or one is already running.
pub fn spawn(state: AppState) -> Option<Worker> {
    let tls = state.tls.clone();
    // Which keeper is decided once, here, from the configuration that cannot
    // change under a running Instance — so neither pass carries an arm for the
    // other's source.
    let mut keeper = match tls.source().clone() {
        Source::Off => return None,
        Source::Files { cert, key } => Keeper::Files(Files::new(cert, key)),
        Source::Acme(domains) => Keeper::Acme(super::acme::Acme::new(domains)),
    };
    tls.wake_up().claim()?;
    // Owed from boot: an ACME Instance's first issuance happens on it, and
    // nothing may read idle before it has been tried.
    tls.wake_up().owes_a_first_pass();
    Some(Worker::start(
        WORKER,
        tls.wake_up().meter(),
        move |mut stop| async move {
            loop {
                // Read before the pass, settled after it — `WakeUp::caught_up`.
                let owed = tls.wake_up().outstanding();
                let now_ms = state.clock.now_ms();
                // **A pass can be stopped.** An issuance waits on a CA, and a
                // CA can be slow or silent; a stop, a restart or a SIGTERM must
                // not wait it out. Dropping the pass mid-order costs nothing the
                // next boot does not redo.
                let next_ms = tokio::select! {
                    next_ms = keeper.pass(&tls, now_ms) => next_ms,
                    _ = stop.cancelled() => break,
                };
                tls.wake_up().caught_up(owed);
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tls.wake_up().woken() => {}
                    _ = state.clock.sleep_until(next_ms) => {}
                }
            }
        },
    ))
}

/// What keeps the certificate current, for the source this Instance has.
enum Keeper {
    Files(Files),
    Acme(super::acme::Acme),
}

impl Keeper {
    /// One look, answering when to look again.
    async fn pass(&mut self, tls: &Tls, now_ms: i64) -> i64 {
        match self {
            Keeper::Files(files) => files.pass(tls, now_ms),
            Keeper::Acme(acme) => acme.pass(tls, now_ms).await,
        }
    }
}

/// What the files pass remembers between looks: what it last found, so a pair
/// that has not changed is not parsed again and a broken one — unreadable or
/// unservable — is reported **once** rather than every minute (ADR-0011 rule 8).
struct Files {
    cert: PathBuf,
    key: PathBuf,
    last: Option<Result<certificate::Pem, certificate::Unreadable>>,
}

impl Files {
    fn new(cert: PathBuf, key: PathBuf) -> Self {
        Files {
            cert,
            key,
            last: None,
        }
    }

    /// Read the Operator's files and serve them if they changed. Answers when
    /// to look again.
    fn pass(&mut self, tls: &Tls, now_ms: i64) -> i64 {
        let next = now_ms.saturating_add(FILES_RECHECK.as_millis() as i64);
        let found = certificate::read_pem(&self.cert, &self.key);
        if self.last.as_ref() == Some(&found) {
            return next;
        }
        let served = found
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|pem| certificate::parse(&self.cert, &pem.chain, &pem.key));
        match served {
            Ok(certificate) => {
                let replaced = tls
                    .served()
                    .is_some_and(|served| served.leaf() != certificate.leaf());
                tls.serve(certificate);
                tls.health_mut(|health| health.last_error = None);
                if replaced {
                    info!(path = %self.cert.display(), "serving the replaced certificate");
                }
            }
            // Gone, unreadable, or not a certificate: whichever, the one being
            // served is still good, and an Operator who renewed by hand — or
            // whose certbot left the key readable by root alone — needs to hear
            // it now rather than when the old one expires.
            Err(unreadable) => {
                error!(error = %unreadable, "the certificate files cannot be served; still serving the previous one");
                tls.health_mut(|health| {
                    health.last_error = Some(super::LastError {
                        at_ms: now_ms,
                        message: unreadable.to_string(),
                    })
                });
            }
        }
        self.last = Some(found);
        next
    }
}
