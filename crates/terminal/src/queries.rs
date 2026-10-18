//! The colour tables behind the colour replies. The queries themselves (OSC 4/10/11/12 `?`, and
//! `CSI 16 t`, which the engine ignores) are found in the raw byte stream by
//! [`crate::sniffer::StreamScanner`], so [`crate::Emulator`] can answer them in stream order.

/// Colour (red, green, blue) as reported in `rgb:` replies.
pub(crate) type Rgb = (u8, u8, u8);

/// ANSI colours 0–15 as xterm reports them.
const ANSI: [Rgb; 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// Levels of the 6×6×6 colour cube (indices 16–231).
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The standard xterm 256-colour palette.
pub(crate) fn xterm_color(index: u8) -> Rgb {
    match index {
        0..=15 => ANSI[usize::from(index)],
        16..=231 => {
            let cube = usize::from(index - 16);
            (CUBE[cube / 36], CUBE[(cube / 6) % 6], CUBE[cube % 6])
        }
        232..=255 => {
            let gray = 8 + 10 * (index - 232);
            (gray, gray, gray)
        }
    }
}

/// True for an OSC payload that queries a colour: `4;N;?…`, `10;?…`, `11;?…`, `12;?…`.
pub(crate) fn is_color_query(payload: &[u8]) -> bool {
    let mut parts = payload.split(|&b| b == b';');
    matches!(parts.next(), Some(b"4" | b"10" | b"11" | b"12")) && parts.any(|part| part == b"?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_queries_are_recognized() {
        for query in [&b"4;1;?"[..], b"10;?", b"11;?", b"12;?", b"4;1;?;2;?"] {
            assert!(is_color_query(query), "{query:?}");
        }
        for other in [
            &b"4;1;rgb:ff/00/00"[..],
            b"10;#fff",
            b"104",
            b"8;;http://x/?",
            b"0;?",
            b"11",
        ] {
            assert!(!is_color_query(other), "{other:?}");
        }
    }

    #[test]
    fn xterm_palette_corners() {
        assert_eq!(xterm_color(1), (205, 0, 0));
        assert_eq!(xterm_color(15), (255, 255, 255));
        assert_eq!(xterm_color(16), (0, 0, 0));
        assert_eq!(xterm_color(21), (0, 0, 255));
        assert_eq!(xterm_color(196), (255, 0, 0));
        assert_eq!(xterm_color(231), (255, 255, 255));
        assert_eq!(xterm_color(232), (8, 8, 8));
        assert_eq!(xterm_color(255), (238, 238, 238));
    }
}
