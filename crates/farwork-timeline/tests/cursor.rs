mod support;

use farwork_timeline::*;

#[test]
fn cursor_round_trips_through_the_wire_form() {
    assert_eq!(Cursor::ZERO.to_wire(), "seq:0");
    assert_eq!(Cursor::ZERO.sequence(), 0);
    assert_eq!(Cursor::parse("seq:0").unwrap(), Cursor::ZERO);
    assert_eq!(Cursor::parse("seq:42").unwrap().sequence(), 42);
    assert_eq!(Cursor::new(7).unwrap().to_string(), "seq:7");
    assert_eq!("seq:7".parse::<Cursor>().unwrap(), Cursor::new(7).unwrap());
    assert_eq!(Cursor::default(), Cursor::ZERO);
}

#[test]
fn cursor_serialises_as_a_wire_string() {
    let encoded = serde_json::to_string(&Cursor::new(11).unwrap()).unwrap();
    assert_eq!(encoded, "\"seq:11\"");
    let decoded: Cursor = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.sequence(), 11);
    assert!(serde_json::from_str::<Cursor>("\"11\"").is_err());
}

#[test]
fn cursor_parsing_rejects_everything_that_is_not_canonical() {
    for raw in [
        "", "seq", "seq:", "seq:-1", "seq:+1", "seq:01", "seq:1.0", "seq: 1", "seq:1 ", "0", "1",
        "SEQ:1", "seq:0x1", "seq:abc",
    ] {
        let error = Cursor::parse(raw).unwrap_err();
        assert!(
            matches!(error, TimelineError::InvalidCursor { .. }),
            "{raw:?} should be an invalid cursor, got {error:?}"
        );
    }
    let too_big = Cursor::parse("seq:9007199254740992").unwrap_err();
    assert!(matches!(too_big, TimelineError::SequenceOutOfRange { .. }));
    assert!(Cursor::new(-1).is_err());
    assert_eq!(
        Cursor::parse("seq:9007199254740991").unwrap().sequence(),
        PROTOCOL_MAX_REVISION
    );
}

#[test]
fn cursors_order_by_sequence() {
    assert!(Cursor::new(1).unwrap() < Cursor::new(2).unwrap());
    assert_eq!(Cursor::ZERO, Cursor::new(0).unwrap());
}
