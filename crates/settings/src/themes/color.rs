//! Colors of a pack: 8-bit sRGB with an optional straight alpha.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 8-bit sRGB color. Serialized as `#rrggbb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `0xRRGGBB`.
    pub const fn from_u32(rgb: u32) -> Self {
        Self::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// `self` moved `t` (0..=1) of the way towards `other`, channel-wise, rounded.
    pub fn mix(self, other: Self, t: f32) -> Self {
        let channel = |a: u8, b: u8| {
            let a = f32::from(a);
            let b = f32::from(b);
            (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
        };
        Self::new(
            channel(self.r, other.r),
            channel(self.g, other.g),
            channel(self.b, other.b),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid color `{0}`; expected #rrggbb")]
pub struct ParseRgbError(String);

impl FromStr for Rgb {
    type Err = ParseRgbError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ParseRgbError(s.to_owned());
        let hex = s.strip_prefix('#').ok_or_else(bad)?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        let value = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
        Ok(Self::from_u32(value))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// An sRGB color with straight alpha in `0.0..=1.0`. A color read from a manifest has an alpha
/// of `byte / 255`; derived roles (`ui.sidebar.hover`, diff tints, ...) carry the exact
/// fraction their fallback names, so the built-ins draw exactly what the old literals drew.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected #rrggbb[aa]")]
pub struct ParseRgbaError;

impl Rgba {
    pub const fn opaque(rgb: Rgb) -> Self {
        Self {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
            a: 1.0,
        }
    }

    pub const fn rgb(self) -> Rgb {
        Rgb::new(self.r, self.g, self.b)
    }

    pub fn is_opaque(self) -> bool {
        self.a >= 1.0
    }

    /// The same color at `alpha` times its own alpha.
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self {
            a: (self.a * alpha).clamp(0.0, 1.0),
            ..self
        }
    }

    /// Channels moved `t` of the way towards `other`; the alpha stays.
    pub fn mix(self, other: Self, t: f32) -> Self {
        let rgb = self.rgb().mix(other.rgb(), t);
        Self {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
            a: self.a,
        }
    }

    /// The alpha as the byte `#rrggbbaa` writes.
    pub fn alpha_byte(self) -> u8 {
        (self.a * 255.0).round().clamp(0.0, 255.0) as u8
    }

    /// `self` painted over the opaque `ground`.
    pub fn over(self, ground: Rgb) -> Rgb {
        let ground = Self::opaque(ground);
        ground.mix(Self { a: 1.0, ..self }, self.a).rgb()
    }
}

impl FromStr for Rgba {
    type Err = ParseRgbaError;

    /// `#RRGGBB` or `#RRGGBBAA`, either case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s.strip_prefix('#').ok_or(ParseRgbaError)?;
        if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ParseRgbaError);
        }
        let value = u32::from_str_radix(hex, 16).map_err(|_| ParseRgbaError)?;
        Ok(if hex.len() == 6 {
            Self::opaque(Rgb::from_u32(value))
        } else {
            Self {
                r: (value >> 24) as u8,
                g: (value >> 16) as u8,
                b: (value >> 8) as u8,
                a: f32::from(value as u8) / 255.0,
            }
        })
    }
}

impl fmt::Display for Rgba {
    /// Lower case, `#rrggbb` when opaque, else `#rrggbbaa`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)?;
        if self.is_opaque() {
            Ok(())
        } else {
            write!(f, "{:02x}", self.alpha_byte())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_round_trips_through_hex() {
        let c: Rgb = "#1e1E1e".parse().unwrap();
        assert_eq!(c, Rgb::new(0x1e, 0x1e, 0x1e));
        assert_eq!(c.to_string(), "#1e1e1e");
        for bad in ["1e1e1e", "#1e1e1", "#1e1e1e1e", "#gggggg", "", "#"] {
            assert!(bad.parse::<Rgb>().is_err(), "{bad}");
        }
    }

    #[test]
    fn rgba_reads_six_and_eight_digits_in_either_case() {
        let c: Rgba = "#FFFFFF12".parse().unwrap();
        assert_eq!((c.r, c.g, c.b), (255, 255, 255));
        assert_eq!(c.alpha_byte(), 0x12);
        assert!((c.a - 18.0 / 255.0).abs() < 1e-6);
        assert!("#ea7a43".parse::<Rgba>().unwrap().is_opaque());
        assert_eq!(c.to_string(), "#ffffff12");
        assert_eq!("#EA7A43".parse::<Rgba>().unwrap().to_string(), "#ea7a43");
        for bad in [
            "ea7a43",
            "#ea7a4",
            "#ea7a43f",
            "#ea7a43ffff",
            "#ggg000",
            "rgb(1,2,3)",
            "",
            "#",
            "#ea7a43 ",
        ] {
            assert!(bad.parse::<Rgba>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn mixing_and_compositing_round_to_the_nearest_channel() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert_eq!(black.mix(white, 0.5), Rgb::new(128, 128, 128));
        assert_eq!(black.mix(white, 0.0), black);
        assert_eq!(black.mix(white, 1.0), white);
        let half = Rgba::opaque(white).with_alpha(0.5);
        assert_eq!(half.over(black), Rgb::new(128, 128, 128));
        assert_eq!(half.with_alpha(0.5).a, 0.25);
    }
}
