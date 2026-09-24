//! **A fake Trunk Recorder**, dialing the status socket for real (#71, spec
//! US 50).
//!
//! Spec §"Primary seam" names this outright — *a fake TR statusServer client
//! over the existing WS helpers* — and the reason is `tests/uploadscript.rs`'s
//! one artifact along: a committed fixture claiming to be what a recorder sends
//! stays green when the side that *reads* it moves. This one connects to a
//! running [`super::TestApp`] over a real WebSocket on a real port and sends
//! what `stat_socket.cc` sends, in the order `Stat_Socket::on_open` sends it.
//!
//! **Every scalar is a string**, because the recorder serialises through
//! `boost::property_tree::write_json`, which has no JSON numbers. A fake that
//! sent native numbers would prove the parser against the one dialect no real
//! recorder speaks.

use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::ws::Ws;

/// A connected fake recorder.
///
/// Holds the socket open: dropping it is the recorder hanging up, which is what
/// the "disconnect is visible" criterion is tested by.
pub struct FakeRecorder {
    socket: Ws,
}

impl FakeRecorder {
    /// Wrap an already-connected socket.
    pub fn over(socket: Ws) -> Self {
        FakeRecorder { socket }
    }

    /// Send one frame verbatim.
    pub async fn send(&mut self, frame: &str) {
        self.socket
            .send(WsMessage::Text(frame.into()))
            .await
            .expect("send a status frame");
    }

    /// The opening exchange: what this recorder *is*, its Systems, and its
    /// demodulators — `Stat_Socket::on_open`'s own order.
    pub async fn open(&mut self, name: &str) {
        self.send(&config_frame(name)).await;
        self.send(SYSTEMS).await;
        self.send(RECORDERS).await;
    }

    /// One `rates` frame, which a real recorder sends every three seconds
    /// whatever is happening.
    pub async fn rates(&mut self, decode_rate: &str) {
        self.send(&format!(
            r#"{{"rates":[{{"id":"0","decoderate":"{decode_rate}"}}],
               "type":"rates","instanceId":"","instanceKey":""}}"#
        ))
        .await;
    }

    /// The whole active-call list, which is what the recorder re-sends whenever
    /// a call starts or ends.
    pub async fn calls_active(&mut self, calls: &str) {
        self.send(&format!(
            r#"{{"calls":[{calls}],"type":"calls_active","instanceId":"","instanceKey":""}}"#
        ))
        .await;
    }

    /// Close the socket the way a recorder shutting down does.
    /// Read (and so answer every ping) until the Instance closes the socket,
    /// or `budget` passes. `true` if it was the Instance that hung up.
    pub async fn hung_up_on_within(&mut self, budget: std::time::Duration) -> bool {
        super::ws::drain_until(&mut self.socket, budget, |message| {
            matches!(message, WsMessage::Close(_))
        })
        .await
            != super::ws::Drained::TimedOut
    }

    pub async fn hang_up(mut self) {
        let _ = self.socket.close(None).await;
    }
}

/// One call in the shape `Call_impl::get_stats` builds — every value a string.
///
/// `mon_state` is the why-not-recorded reason: `0` for a call being recorded,
/// and one of `trunk-recorder/state.h`'s seven otherwise.
pub fn call_frame(id: &str, talkgroup: i64, state: i64, mon_state: i64) -> String {
    format!(
        r#"{{"id":"{id}","freq":"774031250","sysNum":"0","shortName":"butco",
            "talkgroup":"{talkgroup}","talkgrouptag":"FIRE DISPATCH",
            "elapsed":"4","length":"4.2","state":"{state}","monState":"{mon_state}",
            "phase2":"false","conventional":"false","encrypted":"false",
            "emergency":"false","startTime":"1669740338","recNum":"1","srcNum":"0",
            "recState":"3","analog":"false"}}"#
    )
}

/// The `config` frame, which is the only one carrying the recorder's own name.
fn config_frame(name: &str) -> String {
    format!(
        r#"{{"sources":[{{"source_num":"0","antenna":"","silence_frames":"0",
            "min_hz":"773000000","max_hz":"775000000","center":"774000000",
            "rate":"2400000","driver":"osmosdr","device":"rtl=0","error":"0",
            "gain":"40","analog_recorders":"0","digital_recorders":"4",
            "debug_recorders":"0","sigmf_recorders":"0"}}],
            "systems":[{{"audioArchive":"true","systemType":"p25",
            "shortName":"butco","sysNum":"0","uploadScript":"",
            "recordUnkown":"true","callLog":"true","talkgroupsFile":"",
            "analog_levels":"8","digital_levels":"1","qpsk":"true",
            "squelch_db":"-160","channels":["774031250"]}}],
            "captureDir":"/captures","uploadServer":"","callTimeout":"3",
            "logFile":"false","instanceId":"{name}","instanceKey":"",
            "type":"config"}}"#
    )
}

const SYSTEMS: &str = r#"{"systems":[{"id":"0","name":"butco","type":"p25",
    "sysid":"123","wacn":"456","nac":"789"}],
    "type":"systems","instanceId":"","instanceKey":""}"#;

const RECORDERS: &str = r#"{"recorders":[
    {"id":"0_0","type":"P25","srcNum":"0","recNum":"0","count":"6",
     "duration":"76.86","state":"1"},
    {"id":"0_1","type":"P25","srcNum":"0","recNum":"1","count":"0",
     "duration":"1.5147569426240483e-314","state":"7"}],
    "type":"recorders","instanceId":"","instanceKey":""}"#;
