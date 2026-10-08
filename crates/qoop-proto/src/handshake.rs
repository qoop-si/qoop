//! The bootstrap handshake on channel 0.
//!
//! Channel 0 is the answer to "what decodes the first frame before a codec is negotiated".
//! Its payload is JSON, forever, with no negotiation, because something has to be fixed for
//! negotiation to be expressible.
//!
//! ADR-004 originally said CBOR. Writing the decoder changed the answer: this frame is sent
//! once per connection, so compactness buys nothing measurable, and a bootstrap that must
//! never change is worth being readable in a hexdump and decodable by every language a
//! plugin or a mobile client might be written in. That is the kind of decision the plan
//! predicted would only surface when the code was written.

use serde::{Deserialize, Serialize};

/// The compatibility floor for the whole negotiated codec set. It moves only when a
/// codec's first generation changes shape, never when a codec is added and never on an
/// application release.
pub const CODEC_GENERATION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecOffers {
    pub control: Vec<String>,
    pub event: Vec<String>,
    pub terminal: Vec<String>,
    pub blob: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub codec_generation_min: u32,
    pub codec_generation_max: u32,
    /// Self-asserted, and a dedupe key rather than a credential. It is namespaced by the
    /// peer uid, which the socket layer does authenticate. Two same-uid processes can
    /// collide on it, and the worst that buys them is suppressing each other's retries,
    /// which is authority they already have over each other.
    pub client_id: String,
    pub client_name: String,
    pub client_version: String,
    pub codecs: CodecOffers,
    /// Unknown fields are ignored on both sides, which is what lets optional behaviour be
    /// added without a generation bump.
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerWelcome {
    pub codec_generation: u32,
    pub selected: SelectedCodecs,
    pub capabilities: Vec<String>,
    pub server_version: String,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedCodecs {
    pub control: String,
    pub event: String,
    pub terminal: String,
    pub blob: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefuseReason {
    NoCommonCodec,
    GenerationTooOld,
    GenerationTooNew,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRefuse {
    pub reason: RefuseReason,
    /// Which of the four categories failed, so a client knows what to offer next.
    pub unsatisfied_category: Option<String>,
    pub server_codec_generation_min: u32,
    pub server_codec_generation_max: u32,
    pub human_message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandshakeOutcome {
    Welcome(ServerWelcome),
    Refuse(ServerRefuse),
}

/// What this build supports.
pub struct ServerCapabilities {
    pub control: Vec<String>,
    pub event: Vec<String>,
    pub terminal: Vec<String>,
    pub blob: Vec<String>,
    pub capabilities: Vec<String>,
    pub version: String,
}

impl ServerCapabilities {
    pub fn first_generation() -> Self {
        Self {
            control: vec!["control.json.v1".into()],
            event: vec!["event.json.v1".into()],
            terminal: vec!["terminal.frame.v1".into()],
            blob: vec!["blob.raw.v1".into()],
            capabilities: vec![],
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

/// Negotiate, or refuse with enough detail for the client to act on.
///
/// There is deliberately no fallback to a default codec and no partial success: proceeding
/// with a partially negotiated set is how two peers end up disagreeing about what a payload
/// means, which is worse than not connecting.
pub fn negotiate(hello: &ClientHello, server: &ServerCapabilities) -> HandshakeOutcome {
    if hello.codec_generation_max < CODEC_GENERATION {
        return HandshakeOutcome::Refuse(ServerRefuse {
            reason: RefuseReason::GenerationTooOld,
            unsatisfied_category: None,
            server_codec_generation_min: CODEC_GENERATION,
            server_codec_generation_max: CODEC_GENERATION,
            human_message: "this client predates the supported codec generation".into(),
        });
    }
    if hello.codec_generation_min > CODEC_GENERATION {
        return HandshakeOutcome::Refuse(ServerRefuse {
            reason: RefuseReason::GenerationTooNew,
            unsatisfied_category: None,
            server_codec_generation_min: CODEC_GENERATION,
            server_codec_generation_max: CODEC_GENERATION,
            human_message: "this daemon predates the client's codec generation".into(),
        });
    }

    let pick = |offered: &[String], supported: &[String]| -> Option<String> {
        offered.iter().find(|c| supported.contains(c)).cloned()
    };

    let categories: [(&str, &Vec<String>, &Vec<String>); 4] = [
        ("control", &hello.codecs.control, &server.control),
        ("event", &hello.codecs.event, &server.event),
        ("terminal", &hello.codecs.terminal, &server.terminal),
        ("blob", &hello.codecs.blob, &server.blob),
    ];
    let mut chosen = Vec::new();
    for (name, offered, supported) in categories {
        match pick(offered, supported) {
            Some(c) => chosen.push(c),
            None => {
                return HandshakeOutcome::Refuse(ServerRefuse {
                    reason: RefuseReason::NoCommonCodec,
                    unsatisfied_category: Some(name.to_string()),
                    server_codec_generation_min: CODEC_GENERATION,
                    server_codec_generation_max: CODEC_GENERATION,
                    human_message: format!(
                        "no common {name} codec; this daemon supports {supported:?}"
                    ),
                });
            }
        }
    }

    HandshakeOutcome::Welcome(ServerWelcome {
        codec_generation: CODEC_GENERATION,
        selected: SelectedCodecs {
            control: chosen[0].clone(),
            event: chosen[1].clone(),
            terminal: chosen[2].clone(),
            blob: chosen[3].clone(),
        },
        capabilities: server.capabilities.clone(),
        server_version: server.version.clone(),
        extra: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> ClientHello {
        ClientHello {
            codec_generation_min: 1,
            codec_generation_max: 1,
            client_id: "c1".into(),
            client_name: "qoop".into(),
            client_version: "0".into(),
            codecs: CodecOffers {
                control: vec!["control.json.v1".into()],
                event: vec!["event.json.v1".into()],
                terminal: vec!["terminal.frame.v1".into()],
                blob: vec!["blob.raw.v1".into()],
            },
            extra: Default::default(),
        }
    }

    #[test]
    fn a_matching_hello_is_welcomed() {
        let out = negotiate(&hello(), &ServerCapabilities::first_generation());
        match out {
            HandshakeOutcome::Welcome(w) => {
                assert_eq!(w.codec_generation, CODEC_GENERATION);
                assert_eq!(w.selected.terminal, "terminal.frame.v1");
            }
            other => panic!("expected a welcome, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_codec_in_one_category_refuses_and_names_it() {
        let mut h = hello();
        h.codecs.terminal = vec!["terminal.something.v9".into()];
        match negotiate(&h, &ServerCapabilities::first_generation()) {
            HandshakeOutcome::Refuse(r) => {
                assert_eq!(r.reason, RefuseReason::NoCommonCodec);
                assert_eq!(r.unsatisfied_category.as_deref(), Some("terminal"));
                // The refusal has to say what the daemon does support, or the client has
                // nothing to act on.
                assert!(r.human_message.contains("terminal.frame.v1"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn there_is_no_partial_negotiation() {
        // Three of four categories match. Proceeding would leave the two peers disagreeing
        // about what a terminal payload means, which is worse than not connecting.
        let mut h = hello();
        h.codecs.blob = vec![];
        assert!(matches!(
            negotiate(&h, &ServerCapabilities::first_generation()),
            HandshakeOutcome::Refuse(_)
        ));
    }

    #[test]
    fn a_generation_mismatch_reports_the_supported_range() {
        let mut h = hello();
        h.codec_generation_min = 5;
        h.codec_generation_max = 7;
        match negotiate(&h, &ServerCapabilities::first_generation()) {
            HandshakeOutcome::Refuse(r) => {
                assert_eq!(r.reason, RefuseReason::GenerationTooNew);
                assert_eq!(r.server_codec_generation_max, CODEC_GENERATION);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_from_a_newer_peer_are_ignored_not_fatal() {
        // This is what lets optional behaviour ship without a generation bump.
        let json = r#"{
            "codec_generation_min": 1, "codec_generation_max": 1,
            "client_id": "c", "client_name": "n", "client_version": "v",
            "codecs": {"control":["control.json.v1"],"event":["event.json.v1"],
                       "terminal":["terminal.frame.v1"],"blob":["blob.raw.v1"]},
            "a_field_from_the_future": {"nested": true}
        }"#;
        let h: ClientHello = serde_json::from_str(json).expect("must decode");
        assert!(h.extra.contains_key("a_field_from_the_future"));
        assert!(matches!(
            negotiate(&h, &ServerCapabilities::first_generation()),
            HandshakeOutcome::Welcome(_)
        ));
    }

    #[test]
    fn the_hello_round_trips_through_json() {
        let h = hello();
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<ClientHello>(&s).unwrap(), h);
    }
}
