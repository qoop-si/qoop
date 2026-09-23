//! The generation floor, pinned by frozen fixtures.
//!
//! The handshake promises that generation 1 stays decodable and negotiable
//! forever. These fixtures are generation 1 hello and welcome payloads
//! checked in as files, so a change that silently alters the floor fails
//! here and forces a deliberate generation bump instead.

use qoro_proto::{
    CODEC_GENERATION, ClientHello, HandshakeOutcome, ServerCapabilities, ServerWelcome, negotiate,
};

const HELLO_V1: &str = include_str!("fixtures/hello-v1.json");
const WELCOME_V1: &str = include_str!("fixtures/welcome-v1.json");

/// The checked-in hello still decodes, still names generation 1 exactly, and
/// still negotiates to a welcome with the exact generation 1 codecs.
#[test]
fn the_frozen_hello_still_negotiates_the_floor() {
    let hello: ClientHello = serde_json::from_str(HELLO_V1).expect("fixture must decode");
    assert_eq!(hello.codec_generation_min, 1);
    assert_eq!(hello.codec_generation_max, 1);
    assert_eq!(hello.codecs.control, ["control.json.v1"]);
    assert_eq!(hello.codecs.event, ["event.json.v1"]);
    assert_eq!(hello.codecs.terminal, ["terminal.frame.v1"]);
    assert_eq!(hello.codecs.blob, ["blob.raw.v1"]);
    match negotiate(&hello, &ServerCapabilities::first_generation()) {
        HandshakeOutcome::Welcome(w) => {
            assert_eq!(w.codec_generation, CODEC_GENERATION);
            assert_eq!(w.selected.control, "control.json.v1");
            assert_eq!(w.selected.event, "event.json.v1");
            assert_eq!(w.selected.terminal, "terminal.frame.v1");
            assert_eq!(w.selected.blob, "blob.raw.v1");
        }
        other => panic!("the frozen floor hello must be welcomed, got {other:?}"),
    }
}

/// The checked-in welcome still decodes and still carries generation 1 with
/// the exact generation 1 codec strings.
#[test]
fn the_frozen_welcome_still_decodes_the_floor() {
    let welcome: ServerWelcome = serde_json::from_str(WELCOME_V1).expect("fixture must decode");
    assert_eq!(welcome.codec_generation, 1);
    assert_eq!(welcome.selected.control, "control.json.v1");
    assert_eq!(welcome.selected.event, "event.json.v1");
    assert_eq!(welcome.selected.terminal, "terminal.frame.v1");
    assert_eq!(welcome.selected.blob, "blob.raw.v1");
}
