//! Detach key handling: ctrl+\ then `d`, with sequences split at arbitrary read boundaries.

use hutan_cli::DetachDetector;

const P: u8 = 0x1c;

/// Feeds `chunks` in order; returns the forwarded bytes and whether the detach completed.
fn run(chunks: &[&[u8]]) -> (Vec<u8>, bool) {
    let mut detector = DetachDetector::new();
    let mut forwarded = Vec::new();
    for chunk in chunks {
        if detector.feed(chunk, &mut forwarded) {
            return (forwarded, true);
        }
    }
    (forwarded, false)
}

#[test]
fn plain_input_passes_through() {
    assert_eq!(
        run(&[b"hello \x1b[A world"]),
        (b"hello \x1b[A world".to_vec(), false)
    );
}

#[test]
fn prefix_then_d_detaches() {
    assert_eq!(run(&[&[P, b'd']]), (vec![], true));
}

#[test]
fn bytes_before_the_sequence_are_forwarded_and_after_are_dropped() {
    assert_eq!(run(&[&[b'a', b'b', P, b'd', b'c']]), (b"ab".to_vec(), true));
}

#[test]
fn sequence_split_across_reads() {
    assert_eq!(run(&[&[b'x', P], b"d"]), (b"x".to_vec(), true));
}

#[test]
fn prefix_is_held_back_until_the_next_byte() {
    let mut detector = DetachDetector::new();
    let mut out = Vec::new();
    assert!(!detector.feed(&[b'a', P], &mut out));
    assert_eq!(out, b"a");
    assert!(detector.prefix_pending());
    assert!(!detector.feed(b"z", &mut out));
    assert_eq!(out, [b'a', P, b'z']);
    assert!(!detector.prefix_pending());
}

#[test]
fn double_prefix_sends_one_literal_prefix() {
    assert_eq!(run(&[&[P, P]]), (vec![P], false));
    assert_eq!(run(&[&[P], &[P]]), (vec![P], false));
}

#[test]
fn double_prefix_does_not_chain_into_a_detach() {
    // prefix prefix d: literal prefix, then a plain `d`.
    assert_eq!(run(&[&[P, P, b'd']]), (vec![P, b'd'], false));
    assert_eq!(run(&[&[P], &[P], b"d"]), (vec![P, b'd'], false));
}

#[test]
fn prefix_then_other_key_forwards_both() {
    assert_eq!(run(&[&[P, b'x']]), (vec![P, b'x'], false));
    assert_eq!(run(&[&[P], b"D"]), (vec![P, b'D'], false));
    assert_eq!(run(&[&[P], b"\x1b[A"]), (vec![P, 0x1b, b'[', b'A'], false));
}

#[test]
fn every_split_point_of_a_mixed_stream_gives_the_same_result() {
    let stream = [b'a', P, P, b'b', P, b'x', P, b'd', b'z'];
    let expected = (vec![b'a', P, b'b', P, b'x'], true);
    for split in 0..=stream.len() {
        let (a, b) = stream.split_at(split);
        assert_eq!(run(&[a, b]), expected, "split at {split}");
    }
    // One byte per read.
    let singles: Vec<&[u8]> = stream.chunks(1).collect();
    assert_eq!(run(&singles), expected);
}

#[test]
fn detector_is_reusable_after_a_literal_prefix() {
    let mut detector = DetachDetector::new();
    let mut out = Vec::new();
    assert!(!detector.feed(&[P, P], &mut out));
    assert!(detector.feed(&[P, b'd'], &mut out));
    assert_eq!(out, [P]);
}
