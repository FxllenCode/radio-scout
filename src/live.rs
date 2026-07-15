//! The live feed: a raw WebSocket (ADR-0004) that pushes call metadata to
//! subscribed listeners. Audio never rides the socket — only compact JSON.
//!
//! Skeleton scope: a broadcast hub plus per-connection subscription filtering.
//! Ticket #9 layers on access scope, patches, heartbeat, and reconnect.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::AppState;
use crate::call::StoredCall;

/// Channel capacity for the fanout broadcast. Ample for the low-hundreds of
/// listeners this targets; a slow client that lags is simply skipped forward.
const LIVE_FEED_CAPACITY: usize = 1024;

/// A clonable handle to the live-feed fanout. Cloning shares one channel.
#[derive(Clone)]
pub struct LiveFeed {
    tx: broadcast::Sender<Arc<StoredCall>>,
}

impl LiveFeed {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(LIVE_FEED_CAPACITY);
        LiveFeed { tx }
    }

    fn subscribe(&self) -> broadcast::Receiver<Arc<StoredCall>> {
        self.tx.subscribe()
    }

    /// Publish a stored call to all connected listeners. Sending with no
    /// receivers is not an error — it just means nobody is connected.
    pub fn publish(&self, call: Arc<StoredCall>) {
        let _ = self.tx.send(call);
    }
}

impl Default for LiveFeed {
    fn default() -> Self {
        Self::new()
    }
}

/// The listener's subscription matrix: `systemRef -> talkgroupRef -> enabled`.
/// JSON object keys are strings, so refs are compared as strings. `all` is the
/// spec's global all-on (story 21) — handy for a "monitor everything" client.
#[derive(Debug, Default)]
struct Subscription {
    selection: HashMap<String, HashMap<String, bool>>,
    all: bool,
}

impl Subscription {
    /// Does this subscription want the given call? (Skeleton: global all-on, or
    /// exact system+talkgroup match. Patches and access scope are ticket #9.)
    fn matches(&self, call: &StoredCall) -> bool {
        if self.all {
            return true;
        }
        self.selection
            .get(&call.system_ref.to_string())
            .and_then(|talkgroups| talkgroups.get(&call.talkgroup_ref.to_string()))
            .copied()
            .unwrap_or(false)
    }
}

/// Messages a client sends to the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum ClientMessage {
    /// Replace the subscription matrix.
    #[serde(rename = "sub")]
    Sub {
        #[serde(default)]
        sel: HashMap<String, HashMap<String, bool>>,
        #[serde(default)]
        all: bool,
    },
}

/// `GET /api/live` — upgrade to a WebSocket and run the per-connection loop.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut receiver = state.live.subscribe();
    let mut subscription = Subscription::default();

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(ClientMessage::Sub { sel, all }) =
                            serde_json::from_str::<ClientMessage>(text.as_str())
                        {
                            subscription.selection = sel;
                            subscription.all = all;
                            // Ack so the client knows the subscription is live
                            // before it relies on receiving matching calls.
                            if socket
                                .send(Message::Text(r#"{"t":"subscribed"}"#.into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ignore ping/pong/binary in the skeleton
                    Some(Err(_)) => break,
                }
            }
            broadcasted = receiver.recv() => {
                match broadcasted {
                    Ok(call) => {
                        if subscription.matches(&call) {
                            let payload = serde_json::json!({ "t": "call", "call": &*call });
                            if socket
                                .send(Message::Text(payload.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                    // A lagging slow client skips ahead rather than dying.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(system_ref: i64, talkgroup_ref: i64) -> StoredCall {
        StoredCall {
            id: 1,
            system_ref,
            system_label: None,
            talkgroup_ref,
            talkgroup_label: None,
            talkgroup_group: None,
            talkgroup_tag: None,
            frequency: None,
            source: None,
            date_time: None,
            timestamp: None,
            audio_mime: None,
            object_key: String::new(),
            audio_url: String::new(),
        }
    }

    fn subscribed_to(pairs: &[(&str, &str)]) -> Subscription {
        let mut selection: HashMap<String, HashMap<String, bool>> = HashMap::new();
        for (system, talkgroup) in pairs {
            selection
                .entry((*system).to_string())
                .or_default()
                .insert((*talkgroup).to_string(), true);
        }
        Subscription {
            selection,
            all: false,
        }
    }

    #[test]
    fn matches_exact_system_and_talkgroup() {
        let sub = subscribed_to(&[("11", "54241")]);
        assert!(sub.matches(&call(11, 54241)));
    }

    #[test]
    fn does_not_match_other_talkgroup_or_system() {
        let sub = subscribed_to(&[("11", "54241")]);
        assert!(!sub.matches(&call(11, 99999)), "wrong talkgroup");
        assert!(!sub.matches(&call(22, 54241)), "wrong system");
    }

    #[test]
    fn explicitly_disabled_talkgroup_does_not_match() {
        let mut sub = subscribed_to(&[]);
        sub.selection
            .entry("11".to_string())
            .or_default()
            .insert("54241".to_string(), false);
        assert!(!sub.matches(&call(11, 54241)));
    }

    #[test]
    fn all_matches_everything() {
        let sub = Subscription {
            selection: HashMap::new(),
            all: true,
        };
        assert!(sub.matches(&call(1, 2)));
        assert!(sub.matches(&call(999, 888)));
    }

    #[test]
    fn empty_subscription_matches_nothing() {
        assert!(!Subscription::default().matches(&call(11, 54241)));
    }
}
