//! The match setup sheet's colour picker: the kit colours its first row
//! offers, and the maths between a `#rrggbb` string and the hue, saturation
//! and brightness its strip and area are drawn and read in.
//!
//! **All of it is here rather than in the `.slint`.** What the project stores
//! is the hex string ([`match_panel::hex`] / [`parse_hex`]), so the picker has
//! to land on exactly those bytes: a conversion of Slint's own (`Colors.hsv`)
//! would be a second rounding, and the swatch, the field and the scoreboard
//! could then disagree in the last bit. [`hsv_of_hex`] and [`hex_of_hsv`]
//! round-trip every 8-bit colour, which the tests pin.

use video_coach_core::stroke::Rgba;

use crate::match_panel::{hex, parse_hex};

/// A colour as the picker holds it: `hue` in degrees (0..360), `sat` and
/// `val` in 0..1. Floats, and `f32`, because that is what crosses into Slint
/// and back — the round-trip has to hold in the type the UI actually uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsv {
    pub hue: f32,
    pub sat: f32,
    pub val: f32,
}

/// One swatch of the picker's first row.
pub struct KitColor {
    /// What it's called, for the swatch's tooltip.
    pub name: &'static str,
    /// `#rrggbb`, lower case: written into the field as-is.
    pub hex: &'static str,
}

/// The row of ready-made colours: ordinary football kits, plus the
/// neutrals — white, black and two greys — that a *text* colour actually
/// wants. Nothing is near-black or near-white but black and white
/// themselves, so every one of them reads on the scoreboard's cells.
pub const KIT_COLORS: &[KitColor] = &[
    KitColor {
        name: "White",
        hex: "#ffffff",
    },
    KitColor {
        name: "Silver",
        hex: "#c9ccd1",
    },
    KitColor {
        name: "Slate",
        hex: "#5c6672",
    },
    KitColor {
        name: "Black",
        hex: "#000000",
    },
    KitColor {
        name: "Red",
        hex: "#d5202c",
    },
    KitColor {
        name: "Claret",
        hex: "#6d2135",
    },
    KitColor {
        name: "Orange",
        hex: "#f07818",
    },
    KitColor {
        name: "Gold",
        hex: "#f2c200",
    },
    KitColor {
        name: "Green",
        hex: "#1a7f45",
    },
    KitColor {
        name: "Sky blue",
        hex: "#4ea8de",
    },
    KitColor {
        name: "Royal blue",
        hex: "#1e4fd8",
    },
    KitColor {
        name: "Navy",
        hex: "#14224b",
    },
    KitColor {
        name: "Purple",
        hex: "#5e2a84",
    },
    KitColor {
        name: "Pink",
        hex: "#e262a0",
    },
];

/// A field's text as the picker's position; `None` for anything
/// [`parse_hex`] refuses, which the sheet already marks.
///
/// Grey has no hue and black has neither hue nor saturation: both come back
/// as zero. The sheet keeps the hue it was showing in that case rather than
/// swinging the strip to red — see `MatchSetupSheet::sync-from-text`.
pub fn hsv_of_hex(text: &str) -> Option<Hsv> {
    parse_hex(text).map(hsv_of_rgb)
}

/// The picker's position as the field's text, `#rrggbb`.
pub fn hex_of_hsv(hue: f32, sat: f32, val: f32) -> String {
    hex(rgb_of_hsv(hue, sat, val))
}

/// The textbook conversion, on the channels [`parse_hex`] produces.
fn hsv_of_rgb(color: Rgba) -> Hsv {
    let (r, g, b) = (color.r as f32, color.g as f32, color.b as f32);
    let max = r.max(g).max(b);
    let chroma = max - r.min(g).min(b);
    // Six 60° sectors, named by whichever channel is on top. The `rem_euclid`
    // is what wraps the red sector's negative half round to 300..360.
    let hue = if chroma == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    };
    Hsv {
        hue,
        sat: if max == 0.0 { 0.0 } else { chroma / max },
        val: max,
    }
}

/// The inverse. Out-of-range arguments are clamped rather than refused: they
/// come from a pointer inside a rectangle, and a drag that leaves it should
/// pin to the edge, not produce a colour nobody asked for.
fn rgb_of_hsv(hue: f32, sat: f32, val: f32) -> Rgba {
    let (sat, val) = (sat.clamp(0.0, 1.0), val.clamp(0.0, 1.0));
    let sector = hue.rem_euclid(360.0) / 60.0;
    let fraction = sector - sector.floor();
    let (p, q, t) = (
        val * (1.0 - sat),
        val * (1.0 - fraction * sat),
        val * (1.0 - (1.0 - fraction) * sat),
    );
    let (r, g, b) = match sector as u32 {
        0 => (val, t, p),
        1 => (q, val, p),
        2 => (p, val, t),
        3 => (p, q, val),
        4 => (t, p, val),
        // `rem_euclid` keeps `sector` under 6, so this is the last sector and
        // not a fallback.
        _ => (val, p, q),
    };
    Rgba {
        r: r as f64,
        g: g as f64,
        b: b as f64,
        a: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one invariant the picker rests on: a colour that goes out to the
    /// picker and back is the same colour. Without it, opening the popup on a
    /// saved kit colour and touching nothing would still rewrite the field.
    #[test]
    fn hex_round_trips_through_hsv() {
        let round_trip = |text: &str| {
            let hsv = hsv_of_hex(text).unwrap_or_else(|| panic!("{text} parses"));
            hex_of_hsv(hsv.hue, hsv.sat, hsv.val)
        };
        // Every grey, every pure channel and every edge of the cube at full
        // resolution — that is where the sectors meet and where a rounding
        // slips — then a coarse net through the middle. The maths is per
        // channel, so a few thousand interior points say as much as the
        // 16.7M do, and they say it in a test that finishes.
        for v in 0..=255u8 {
            for text in [
                format!("#{v:02x}{v:02x}{v:02x}"),
                format!("#{v:02x}0000"),
                format!("#00{v:02x}00"),
                format!("#0000{v:02x}"),
                format!("#{v:02x}ff00"),
                format!("#00{v:02x}ff"),
                format!("#ff00{v:02x}"),
            ] {
                assert_eq!(round_trip(&text), text, "{text}");
            }
        }
        for r in (0..=255u8).step_by(17) {
            for g in (0..=255u8).step_by(17) {
                for b in (0..=255u8).step_by(17) {
                    let text = format!("#{r:02x}{g:02x}{b:02x}");
                    assert_eq!(round_trip(&text), text, "{text}");
                }
            }
        }
    }

    /// The hues the strip is labelled by, so a drag to a third of the way
    /// along really does land on green.
    #[test]
    fn hue_reads_off_the_sectors() {
        for (text, hue) in [
            ("#ff0000", 0.0),
            ("#ffff00", 60.0),
            ("#00ff00", 120.0),
            ("#00ffff", 180.0),
            ("#0000ff", 240.0),
            ("#ff00ff", 300.0),
        ] {
            let hsv = hsv_of_hex(text).unwrap();
            assert_eq!((hsv.hue, hsv.sat, hsv.val), (hue, 1.0, 1.0), "{text}");
        }
        // No hue and no saturation: the sheet holds the strip where it was.
        assert_eq!(
            hsv_of_hex("#808080"),
            Some(Hsv {
                hue: 0.0,
                sat: 0.0,
                val: 128.0 / 255.0
            })
        );
    }

    /// A pointer that leaves the area pins to the edge (and `hue` wraps),
    /// rather than producing a colour off the end of a sector table.
    #[test]
    fn out_of_range_clamps() {
        assert_eq!(hex_of_hsv(30.0, 2.0, 2.0), hex_of_hsv(30.0, 1.0, 1.0));
        assert_eq!(hex_of_hsv(30.0, -1.0, 0.5), hex_of_hsv(30.0, 0.0, 0.5));
        assert_eq!(hex_of_hsv(-30.0, 1.0, 1.0), hex_of_hsv(330.0, 1.0, 1.0));
        assert_eq!(hex_of_hsv(360.0, 1.0, 1.0), "#ff0000");
    }

    /// The row the popup opens with: each one has to be something the field
    /// can hold, and something the coach can tell from its neighbour.
    #[test]
    fn the_kit_row_is_usable() {
        for kit in KIT_COLORS {
            assert!(parse_hex(kit.hex).is_some(), "{} parses", kit.name);
            assert_eq!(hex(parse_hex(kit.hex).unwrap()), kit.hex, "{}", kit.name);
        }
        let mut seen: Vec<&str> = KIT_COLORS.iter().map(|k| k.hex).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "no colour is offered twice");
        // The two a text colour is nearly always one of.
        assert!(seen.contains(&"#ffffff") && seen.contains(&"#000000"));
    }
}
