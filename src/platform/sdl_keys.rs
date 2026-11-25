//! SDL key types → Harbour's own (`crate::keys`). This is the one place that knows both;
//! the SDL event loop calls it and everything behind it only sees Harbour's types.
//!
//! The two use the same key codes and flag bits, so the conversion copies the numbers over.

use crate::keys::{Keycode, Mod};

impl From<sdl2::keyboard::Keycode> for Keycode {
    fn from(k: sdl2::keyboard::Keycode) -> Keycode {
        // SDL never sends 0 for a real key; anything it cannot name becomes `Unknown`.
        Keycode::from_i32(k.into_i32()).unwrap_or(Keycode::Unknown)
    }
}

impl From<sdl2::keyboard::Mod> for Mod {
    fn from(m: sdl2::keyboard::Mod) -> Mod {
        Mod::from_bits_truncate(m.bits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdl2::keyboard::Keycode as Sk;

    #[test]
    fn keycodes_and_names_match_sdl() {
        // Every key code SDL knows (the printable range and the scancode-based ones).
        let codes = (1..=0x7f).chain((0..=0x200).map(|sc| sc | (1 << 30)));
        for n in codes {
            let Some(s) = Sk::from_i32(n) else { continue };
            let ours: Keycode = s.into();
            assert_eq!(ours.into_i32(), n);
            // SDL names a few scancode-style codes that no real key produces ("A" for scancode 4);
            // Harbour only names the keys that exist.
            if n & (1 << 30) == 0 || !ours.name().is_empty() {
                assert_eq!(ours.name(), s.name(), "name of key code {n}");
            }
            let name = s.name();
            if !name.is_empty() && ours.name() == name {
                // Looking a name up gives the same key as SDL does.
                assert_eq!(Keycode::from_name(&name).map(|k| k.into_i32()), Sk::from_name(&name).map(|k| k.into_i32()), "from_name({name:?})");
            }
        }
        for name in ["a", "A", "1", "+", ",", "Space", "space", "RETURN", "Keypad Enter", "left shift", "F24", "nonsense", "", "É"] {
            assert_eq!(Keycode::from_name(name).map(|k| k.into_i32()), Sk::from_name(name).map(|k| k.into_i32()), "from_name({name:?})");
        }
    }

    #[test]
    fn modifiers_keep_their_bits() {
        use sdl2::keyboard::Mod as Sm;
        for m in [Sm::NOMOD, Sm::LSHIFTMOD, Sm::RCTRLMOD | Sm::LALTMOD, Sm::all()] {
            assert_eq!(Mod::from(m).bits(), m.bits());
        }
        assert_eq!(Mod::from(Sm::LCTRLMOD), Mod::LCTRLMOD);
    }
}
