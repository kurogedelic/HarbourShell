//! A colour with 8-bit channels, named like `sdl2::pixels::Color`.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[allow(non_snake_case)]
impl Color {
    /// Opaque colour.
    pub const fn RGB(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }

    /// Colour with alpha.
    pub const fn RGBA(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }

    /// Pack into RGB565 by dropping low bits (the same as SDL's `SDL_MapRGB`).
    pub const fn to_rgb565(self) -> u16 {
        (((self.r as u16) >> 3) << 11) | (((self.g as u16) >> 2) << 5) | ((self.b as u16) >> 3)
    }

    /// Unpack RGB565 to 8-bit channels, scaling 0..31 (0..63) to 0..255 and rounding
    /// down, which is what SDL's pixel conversion does.
    pub const fn from_rgb565(v: u16) -> Color {
        let r = ((v >> 11) & 31) as u32;
        let g = ((v >> 5) & 63) as u32;
        let b = (v & 31) as u32;
        Color::RGB((r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8)
    }
}

impl From<(u8, u8, u8)> for Color {
    fn from((r, g, b): (u8, u8, u8)) -> Color {
        Color::RGB(r, g, b)
    }
}

impl From<(u8, u8, u8, u8)> for Color {
    fn from((r, g, b, a): (u8, u8, u8, u8)) -> Color {
        Color::RGBA(r, g, b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb565_round_trip_is_stable() {
        for v in [0u16, 0xFFFF, 0x1234, 0xF800, 0x07E0, 0x001F] {
            assert_eq!(Color::from_rgb565(v).to_rgb565(), v);
        }
        assert_eq!(Color::from_rgb565(0xFFFF), Color::RGB(255, 255, 255));
    }
}
