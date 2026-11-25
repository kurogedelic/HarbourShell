//! Harbour's own key types: `Keycode` (which key) and `Mod` (which modifiers are held).
//!
//! They carry the same values and the same names as SDL's, so the launcher and the apps read
//! the same as before, but nothing here depends on SDL. SDL events are converted at the edge
//! (`platform::sdl_keys`); the evdev table (`platform::keys`) and the headless script produce
//! these types directly.

use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Deref, Not};

/// A key, by what its label says (not where it sits). The number is SDL's key code: the
/// character for printable keys, the scancode with bit 30 set for the rest.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Keycode(i32);

#[allow(non_upper_case_globals)]
impl Keycode {
    /// A key that could not be told (SDL sent a key code of 0).
    pub const Unknown: Keycode = Keycode(0);

    pub const BACKSPACE: Keycode = Keycode(8);
    pub const TAB: Keycode = Keycode(9);
    pub const RETURN: Keycode = Keycode(13);
    pub const ESCAPE: Keycode = Keycode(27);
    pub const SPACE: Keycode = Keycode(32);
    pub const EXCLAIM: Keycode = Keycode(33);
    pub const QUOTEDBL: Keycode = Keycode(34);
    pub const HASH: Keycode = Keycode(35);
    pub const DOLLAR: Keycode = Keycode(36);
    pub const PERCENT: Keycode = Keycode(37);
    pub const AMPERSAND: Keycode = Keycode(38);
    pub const QUOTE: Keycode = Keycode(39);
    pub const LEFTPAREN: Keycode = Keycode(40);
    pub const RIGHTPAREN: Keycode = Keycode(41);
    pub const ASTERISK: Keycode = Keycode(42);
    pub const PLUS: Keycode = Keycode(43);
    pub const COMMA: Keycode = Keycode(44);
    pub const MINUS: Keycode = Keycode(45);
    pub const PERIOD: Keycode = Keycode(46);
    pub const SLASH: Keycode = Keycode(47);
    pub const NUM_0: Keycode = Keycode(48);
    pub const NUM_1: Keycode = Keycode(49);
    pub const NUM_2: Keycode = Keycode(50);
    pub const NUM_3: Keycode = Keycode(51);
    pub const NUM_4: Keycode = Keycode(52);
    pub const NUM_5: Keycode = Keycode(53);
    pub const NUM_6: Keycode = Keycode(54);
    pub const NUM_7: Keycode = Keycode(55);
    pub const NUM_8: Keycode = Keycode(56);
    pub const NUM_9: Keycode = Keycode(57);
    pub const COLON: Keycode = Keycode(58);
    pub const SEMICOLON: Keycode = Keycode(59);
    pub const LESS: Keycode = Keycode(60);
    pub const EQUALS: Keycode = Keycode(61);
    pub const GREATER: Keycode = Keycode(62);
    pub const QUESTION: Keycode = Keycode(63);
    pub const AT: Keycode = Keycode(64);
    pub const LEFTBRACKET: Keycode = Keycode(91);
    pub const BACKSLASH: Keycode = Keycode(92);
    pub const RIGHTBRACKET: Keycode = Keycode(93);
    pub const CARET: Keycode = Keycode(94);
    pub const UNDERSCORE: Keycode = Keycode(95);
    pub const BACKQUOTE: Keycode = Keycode(96);
    pub const A: Keycode = Keycode(97);
    pub const B: Keycode = Keycode(98);
    pub const C: Keycode = Keycode(99);
    pub const D: Keycode = Keycode(100);
    pub const E: Keycode = Keycode(101);
    pub const F: Keycode = Keycode(102);
    pub const G: Keycode = Keycode(103);
    pub const H: Keycode = Keycode(104);
    pub const I: Keycode = Keycode(105);
    pub const J: Keycode = Keycode(106);
    pub const K: Keycode = Keycode(107);
    pub const L: Keycode = Keycode(108);
    pub const M: Keycode = Keycode(109);
    pub const N: Keycode = Keycode(110);
    pub const O: Keycode = Keycode(111);
    pub const P: Keycode = Keycode(112);
    pub const Q: Keycode = Keycode(113);
    pub const R: Keycode = Keycode(114);
    pub const S: Keycode = Keycode(115);
    pub const T: Keycode = Keycode(116);
    pub const U: Keycode = Keycode(117);
    pub const V: Keycode = Keycode(118);
    pub const W: Keycode = Keycode(119);
    pub const X: Keycode = Keycode(120);
    pub const Y: Keycode = Keycode(121);
    pub const Z: Keycode = Keycode(122);
    pub const DELETE: Keycode = Keycode(127);
    pub const CAPSLOCK: Keycode = Keycode(1073741881);
    pub const F1: Keycode = Keycode(1073741882);
    pub const F2: Keycode = Keycode(1073741883);
    pub const F3: Keycode = Keycode(1073741884);
    pub const F4: Keycode = Keycode(1073741885);
    pub const F5: Keycode = Keycode(1073741886);
    pub const F6: Keycode = Keycode(1073741887);
    pub const F7: Keycode = Keycode(1073741888);
    pub const F8: Keycode = Keycode(1073741889);
    pub const F9: Keycode = Keycode(1073741890);
    pub const F10: Keycode = Keycode(1073741891);
    pub const F11: Keycode = Keycode(1073741892);
    pub const F12: Keycode = Keycode(1073741893);
    pub const PRINTSCREEN: Keycode = Keycode(1073741894);
    pub const SCROLLLOCK: Keycode = Keycode(1073741895);
    pub const PAUSE: Keycode = Keycode(1073741896);
    pub const INSERT: Keycode = Keycode(1073741897);
    pub const HOME: Keycode = Keycode(1073741898);
    pub const PAGEUP: Keycode = Keycode(1073741899);
    pub const END: Keycode = Keycode(1073741901);
    pub const PAGEDOWN: Keycode = Keycode(1073741902);
    pub const RIGHT: Keycode = Keycode(1073741903);
    pub const LEFT: Keycode = Keycode(1073741904);
    pub const DOWN: Keycode = Keycode(1073741905);
    pub const UP: Keycode = Keycode(1073741906);
    pub const NUMLOCKCLEAR: Keycode = Keycode(1073741907);
    pub const KP_DIVIDE: Keycode = Keycode(1073741908);
    pub const KP_MULTIPLY: Keycode = Keycode(1073741909);
    pub const KP_MINUS: Keycode = Keycode(1073741910);
    pub const KP_PLUS: Keycode = Keycode(1073741911);
    pub const KP_ENTER: Keycode = Keycode(1073741912);
    pub const KP_1: Keycode = Keycode(1073741913);
    pub const KP_2: Keycode = Keycode(1073741914);
    pub const KP_3: Keycode = Keycode(1073741915);
    pub const KP_4: Keycode = Keycode(1073741916);
    pub const KP_5: Keycode = Keycode(1073741917);
    pub const KP_6: Keycode = Keycode(1073741918);
    pub const KP_7: Keycode = Keycode(1073741919);
    pub const KP_8: Keycode = Keycode(1073741920);
    pub const KP_9: Keycode = Keycode(1073741921);
    pub const KP_0: Keycode = Keycode(1073741922);
    pub const KP_PERIOD: Keycode = Keycode(1073741923);
    pub const APPLICATION: Keycode = Keycode(1073741925);
    pub const POWER: Keycode = Keycode(1073741926);
    pub const KP_EQUALS: Keycode = Keycode(1073741927);
    pub const F13: Keycode = Keycode(1073741928);
    pub const F14: Keycode = Keycode(1073741929);
    pub const F15: Keycode = Keycode(1073741930);
    pub const F16: Keycode = Keycode(1073741931);
    pub const F17: Keycode = Keycode(1073741932);
    pub const F18: Keycode = Keycode(1073741933);
    pub const F19: Keycode = Keycode(1073741934);
    pub const F20: Keycode = Keycode(1073741935);
    pub const F21: Keycode = Keycode(1073741936);
    pub const F22: Keycode = Keycode(1073741937);
    pub const F23: Keycode = Keycode(1073741938);
    pub const F24: Keycode = Keycode(1073741939);
    pub const EXECUTE: Keycode = Keycode(1073741940);
    pub const HELP: Keycode = Keycode(1073741941);
    pub const MENU: Keycode = Keycode(1073741942);
    pub const SELECT: Keycode = Keycode(1073741943);
    pub const STOP: Keycode = Keycode(1073741944);
    pub const AGAIN: Keycode = Keycode(1073741945);
    pub const UNDO: Keycode = Keycode(1073741946);
    pub const CUT: Keycode = Keycode(1073741947);
    pub const COPY: Keycode = Keycode(1073741948);
    pub const PASTE: Keycode = Keycode(1073741949);
    pub const FIND: Keycode = Keycode(1073741950);
    pub const MUTE: Keycode = Keycode(1073741951);
    pub const VOLUMEUP: Keycode = Keycode(1073741952);
    pub const VOLUMEDOWN: Keycode = Keycode(1073741953);
    pub const KP_COMMA: Keycode = Keycode(1073741957);
    pub const KP_EQUALSAS400: Keycode = Keycode(1073741958);
    pub const ALTERASE: Keycode = Keycode(1073741977);
    pub const SYSREQ: Keycode = Keycode(1073741978);
    pub const CANCEL: Keycode = Keycode(1073741979);
    pub const CLEAR: Keycode = Keycode(1073741980);
    pub const PRIOR: Keycode = Keycode(1073741981);
    pub const RETURN2: Keycode = Keycode(1073741982);
    pub const SEPARATOR: Keycode = Keycode(1073741983);
    pub const OUT: Keycode = Keycode(1073741984);
    pub const OPER: Keycode = Keycode(1073741985);
    pub const CLEARAGAIN: Keycode = Keycode(1073741986);
    pub const CRSEL: Keycode = Keycode(1073741987);
    pub const EXSEL: Keycode = Keycode(1073741988);
    pub const KP_00: Keycode = Keycode(1073742000);
    pub const KP_000: Keycode = Keycode(1073742001);
    pub const DECIMALSEPARATOR: Keycode = Keycode(1073742003);
    pub const CURRENCYUNIT: Keycode = Keycode(1073742004);
    pub const CURRENCYSUBUNIT: Keycode = Keycode(1073742005);
    pub const KP_LEFTPAREN: Keycode = Keycode(1073742006);
    pub const KP_RIGHTPAREN: Keycode = Keycode(1073742007);
    pub const KP_LEFTBRACE: Keycode = Keycode(1073742008);
    pub const KP_RIGHTBRACE: Keycode = Keycode(1073742009);
    pub const KP_TAB: Keycode = Keycode(1073742010);
    pub const KP_BACKSPACE: Keycode = Keycode(1073742011);
    pub const KP_A: Keycode = Keycode(1073742012);
    pub const KP_B: Keycode = Keycode(1073742013);
    pub const KP_C: Keycode = Keycode(1073742014);
    pub const KP_D: Keycode = Keycode(1073742015);
    pub const KP_E: Keycode = Keycode(1073742016);
    pub const KP_F: Keycode = Keycode(1073742017);
    pub const KP_XOR: Keycode = Keycode(1073742018);
    pub const KP_POWER: Keycode = Keycode(1073742019);
    pub const KP_PERCENT: Keycode = Keycode(1073742020);
    pub const KP_LESS: Keycode = Keycode(1073742021);
    pub const KP_GREATER: Keycode = Keycode(1073742022);
    pub const KP_AMPERSAND: Keycode = Keycode(1073742023);
    pub const KP_DBLAMPERSAND: Keycode = Keycode(1073742024);
    pub const KP_VERTICALBAR: Keycode = Keycode(1073742025);
    pub const KP_DBLVERTICALBAR: Keycode = Keycode(1073742026);
    pub const KP_COLON: Keycode = Keycode(1073742027);
    pub const KP_HASH: Keycode = Keycode(1073742028);
    pub const KP_SPACE: Keycode = Keycode(1073742029);
    pub const KP_AT: Keycode = Keycode(1073742030);
    pub const KP_EXCLAM: Keycode = Keycode(1073742031);
    pub const KP_MEMSTORE: Keycode = Keycode(1073742032);
    pub const KP_MEMRECALL: Keycode = Keycode(1073742033);
    pub const KP_MEMCLEAR: Keycode = Keycode(1073742034);
    pub const KP_MEMADD: Keycode = Keycode(1073742035);
    pub const KP_MEMSUBTRACT: Keycode = Keycode(1073742036);
    pub const KP_MEMMULTIPLY: Keycode = Keycode(1073742037);
    pub const KP_MEMDIVIDE: Keycode = Keycode(1073742038);
    pub const KP_PLUSMINUS: Keycode = Keycode(1073742039);
    pub const KP_CLEAR: Keycode = Keycode(1073742040);
    pub const KP_CLEARENTRY: Keycode = Keycode(1073742041);
    pub const KP_BINARY: Keycode = Keycode(1073742042);
    pub const KP_OCTAL: Keycode = Keycode(1073742043);
    pub const KP_DECIMAL: Keycode = Keycode(1073742044);
    pub const KP_HEXADECIMAL: Keycode = Keycode(1073742045);
    pub const LCTRL: Keycode = Keycode(1073742048);
    pub const LSHIFT: Keycode = Keycode(1073742049);
    pub const LALT: Keycode = Keycode(1073742050);
    pub const LGUI: Keycode = Keycode(1073742051);
    pub const RCTRL: Keycode = Keycode(1073742052);
    pub const RSHIFT: Keycode = Keycode(1073742053);
    pub const RALT: Keycode = Keycode(1073742054);
    pub const RGUI: Keycode = Keycode(1073742055);
    pub const MODE: Keycode = Keycode(1073742081);
    pub const AUDIONEXT: Keycode = Keycode(1073742082);
    pub const AUDIOPREV: Keycode = Keycode(1073742083);
    pub const AUDIOSTOP: Keycode = Keycode(1073742084);
    pub const AUDIOPLAY: Keycode = Keycode(1073742085);
    pub const AUDIOMUTE: Keycode = Keycode(1073742086);
    pub const MEDIASELECT: Keycode = Keycode(1073742087);
    pub const WWW: Keycode = Keycode(1073742088);
    pub const MAIL: Keycode = Keycode(1073742089);
    pub const CALCULATOR: Keycode = Keycode(1073742090);
    pub const COMPUTER: Keycode = Keycode(1073742091);
    pub const AC_SEARCH: Keycode = Keycode(1073742092);
    pub const AC_HOME: Keycode = Keycode(1073742093);
    pub const AC_BACK: Keycode = Keycode(1073742094);
    pub const AC_FORWARD: Keycode = Keycode(1073742095);
    pub const AC_STOP: Keycode = Keycode(1073742096);
    pub const AC_REFRESH: Keycode = Keycode(1073742097);
    pub const AC_BOOKMARKS: Keycode = Keycode(1073742098);
    pub const BRIGHTNESSDOWN: Keycode = Keycode(1073742099);
    pub const BRIGHTNESSUP: Keycode = Keycode(1073742100);
    pub const DISPLAYSWITCH: Keycode = Keycode(1073742101);
    pub const KBDILLUMTOGGLE: Keycode = Keycode(1073742102);
    pub const KBDILLUMDOWN: Keycode = Keycode(1073742103);
    pub const KBDILLUMUP: Keycode = Keycode(1073742104);
    pub const EJECT: Keycode = Keycode(1073742105);
    pub const SLEEP: Keycode = Keycode(1073742106);

    pub const Backspace: Keycode = Keycode::BACKSPACE;
    pub const Tab: Keycode = Keycode::TAB;
    pub const Return: Keycode = Keycode::RETURN;
    pub const Escape: Keycode = Keycode::ESCAPE;
    pub const Space: Keycode = Keycode::SPACE;
    pub const Exclaim: Keycode = Keycode::EXCLAIM;
    pub const Quotedbl: Keycode = Keycode::QUOTEDBL;
    pub const Hash: Keycode = Keycode::HASH;
    pub const Dollar: Keycode = Keycode::DOLLAR;
    pub const Percent: Keycode = Keycode::PERCENT;
    pub const Ampersand: Keycode = Keycode::AMPERSAND;
    pub const Quote: Keycode = Keycode::QUOTE;
    pub const LeftParen: Keycode = Keycode::LEFTPAREN;
    pub const RightParen: Keycode = Keycode::RIGHTPAREN;
    pub const Asterisk: Keycode = Keycode::ASTERISK;
    pub const Plus: Keycode = Keycode::PLUS;
    pub const Comma: Keycode = Keycode::COMMA;
    pub const Minus: Keycode = Keycode::MINUS;
    pub const Period: Keycode = Keycode::PERIOD;
    pub const Slash: Keycode = Keycode::SLASH;
    pub const Num0: Keycode = Keycode::NUM_0;
    pub const Num1: Keycode = Keycode::NUM_1;
    pub const Num2: Keycode = Keycode::NUM_2;
    pub const Num3: Keycode = Keycode::NUM_3;
    pub const Num4: Keycode = Keycode::NUM_4;
    pub const Num5: Keycode = Keycode::NUM_5;
    pub const Num6: Keycode = Keycode::NUM_6;
    pub const Num7: Keycode = Keycode::NUM_7;
    pub const Num8: Keycode = Keycode::NUM_8;
    pub const Num9: Keycode = Keycode::NUM_9;
    pub const Colon: Keycode = Keycode::COLON;
    pub const Semicolon: Keycode = Keycode::SEMICOLON;
    pub const Less: Keycode = Keycode::LESS;
    pub const Equals: Keycode = Keycode::EQUALS;
    pub const Greater: Keycode = Keycode::GREATER;
    pub const Question: Keycode = Keycode::QUESTION;
    pub const At: Keycode = Keycode::AT;
    pub const LeftBracket: Keycode = Keycode::LEFTBRACKET;
    pub const Backslash: Keycode = Keycode::BACKSLASH;
    pub const RightBracket: Keycode = Keycode::RIGHTBRACKET;
    pub const Caret: Keycode = Keycode::CARET;
    pub const Underscore: Keycode = Keycode::UNDERSCORE;
    pub const Backquote: Keycode = Keycode::BACKQUOTE;
    pub const Delete: Keycode = Keycode::DELETE;
    pub const CapsLock: Keycode = Keycode::CAPSLOCK;
    pub const PrintScreen: Keycode = Keycode::PRINTSCREEN;
    pub const ScrollLock: Keycode = Keycode::SCROLLLOCK;
    pub const Pause: Keycode = Keycode::PAUSE;
    pub const Insert: Keycode = Keycode::INSERT;
    pub const Home: Keycode = Keycode::HOME;
    pub const PageUp: Keycode = Keycode::PAGEUP;
    pub const End: Keycode = Keycode::END;
    pub const PageDown: Keycode = Keycode::PAGEDOWN;
    pub const Right: Keycode = Keycode::RIGHT;
    pub const Left: Keycode = Keycode::LEFT;
    pub const Down: Keycode = Keycode::DOWN;
    pub const Up: Keycode = Keycode::UP;
    pub const NumLockClear: Keycode = Keycode::NUMLOCKCLEAR;
    pub const KpDivide: Keycode = Keycode::KP_DIVIDE;
    pub const KpMultiply: Keycode = Keycode::KP_MULTIPLY;
    pub const KpMinus: Keycode = Keycode::KP_MINUS;
    pub const KpPlus: Keycode = Keycode::KP_PLUS;
    pub const KpEnter: Keycode = Keycode::KP_ENTER;
    pub const Kp1: Keycode = Keycode::KP_1;
    pub const Kp2: Keycode = Keycode::KP_2;
    pub const Kp3: Keycode = Keycode::KP_3;
    pub const Kp4: Keycode = Keycode::KP_4;
    pub const Kp5: Keycode = Keycode::KP_5;
    pub const Kp6: Keycode = Keycode::KP_6;
    pub const Kp7: Keycode = Keycode::KP_7;
    pub const Kp8: Keycode = Keycode::KP_8;
    pub const Kp9: Keycode = Keycode::KP_9;
    pub const Kp0: Keycode = Keycode::KP_0;
    pub const KpPeriod: Keycode = Keycode::KP_PERIOD;
    pub const Application: Keycode = Keycode::APPLICATION;
    pub const Power: Keycode = Keycode::POWER;
    pub const KpEquals: Keycode = Keycode::KP_EQUALS;
    pub const Execute: Keycode = Keycode::EXECUTE;
    pub const Help: Keycode = Keycode::HELP;
    pub const Menu: Keycode = Keycode::MENU;
    pub const Select: Keycode = Keycode::SELECT;
    pub const Stop: Keycode = Keycode::STOP;
    pub const Again: Keycode = Keycode::AGAIN;
    pub const Undo: Keycode = Keycode::UNDO;
    pub const Cut: Keycode = Keycode::CUT;
    pub const Copy: Keycode = Keycode::COPY;
    pub const Paste: Keycode = Keycode::PASTE;
    pub const Find: Keycode = Keycode::FIND;
    pub const Mute: Keycode = Keycode::MUTE;
    pub const VolumeUp: Keycode = Keycode::VOLUMEUP;
    pub const VolumeDown: Keycode = Keycode::VOLUMEDOWN;
    pub const KpComma: Keycode = Keycode::KP_COMMA;
    pub const KpEqualsAS400: Keycode = Keycode::KP_EQUALSAS400;
    pub const AltErase: Keycode = Keycode::ALTERASE;
    pub const Sysreq: Keycode = Keycode::SYSREQ;
    pub const Cancel: Keycode = Keycode::CANCEL;
    pub const Clear: Keycode = Keycode::CLEAR;
    pub const Prior: Keycode = Keycode::PRIOR;
    pub const Return2: Keycode = Keycode::RETURN2;
    pub const Separator: Keycode = Keycode::SEPARATOR;
    pub const Out: Keycode = Keycode::OUT;
    pub const Oper: Keycode = Keycode::OPER;
    pub const ClearAgain: Keycode = Keycode::CLEARAGAIN;
    pub const CrSel: Keycode = Keycode::CRSEL;
    pub const ExSel: Keycode = Keycode::EXSEL;
    pub const Kp00: Keycode = Keycode::KP_00;
    pub const Kp000: Keycode = Keycode::KP_000;
    pub const DecimalSeparator: Keycode = Keycode::DECIMALSEPARATOR;
    pub const CurrencyUnit: Keycode = Keycode::CURRENCYUNIT;
    pub const CurrencySubUnit: Keycode = Keycode::CURRENCYSUBUNIT;
    pub const KpLeftParen: Keycode = Keycode::KP_LEFTPAREN;
    pub const KpRightParen: Keycode = Keycode::KP_RIGHTPAREN;
    pub const KpLeftBrace: Keycode = Keycode::KP_LEFTBRACE;
    pub const KpRightBrace: Keycode = Keycode::KP_RIGHTBRACE;
    pub const KpTab: Keycode = Keycode::KP_TAB;
    pub const KpBackspace: Keycode = Keycode::KP_BACKSPACE;
    pub const KpA: Keycode = Keycode::KP_A;
    pub const KpB: Keycode = Keycode::KP_B;
    pub const KpC: Keycode = Keycode::KP_C;
    pub const KpD: Keycode = Keycode::KP_D;
    pub const KpE: Keycode = Keycode::KP_E;
    pub const KpF: Keycode = Keycode::KP_F;
    pub const KpXor: Keycode = Keycode::KP_XOR;
    pub const KpPower: Keycode = Keycode::KP_POWER;
    pub const KpPercent: Keycode = Keycode::KP_PERCENT;
    pub const KpLess: Keycode = Keycode::KP_LESS;
    pub const KpGreater: Keycode = Keycode::KP_GREATER;
    pub const KpAmpersand: Keycode = Keycode::KP_AMPERSAND;
    pub const KpDblAmpersand: Keycode = Keycode::KP_DBLAMPERSAND;
    pub const KpVerticalBar: Keycode = Keycode::KP_VERTICALBAR;
    pub const KpDblVerticalBar: Keycode = Keycode::KP_DBLVERTICALBAR;
    pub const KpColon: Keycode = Keycode::KP_COLON;
    pub const KpHash: Keycode = Keycode::KP_HASH;
    pub const KpSpace: Keycode = Keycode::KP_SPACE;
    pub const KpAt: Keycode = Keycode::KP_AT;
    pub const KpExclam: Keycode = Keycode::KP_EXCLAM;
    pub const KpMemStore: Keycode = Keycode::KP_MEMSTORE;
    pub const KpMemRecall: Keycode = Keycode::KP_MEMRECALL;
    pub const KpMemClear: Keycode = Keycode::KP_MEMCLEAR;
    pub const KpMemAdd: Keycode = Keycode::KP_MEMADD;
    pub const KpMemSubtract: Keycode = Keycode::KP_MEMSUBTRACT;
    pub const KpMemMultiply: Keycode = Keycode::KP_MEMMULTIPLY;
    pub const KpMemDivide: Keycode = Keycode::KP_MEMDIVIDE;
    pub const KpPlusMinus: Keycode = Keycode::KP_PLUSMINUS;
    pub const KpClear: Keycode = Keycode::KP_CLEAR;
    pub const KpClearEntry: Keycode = Keycode::KP_CLEARENTRY;
    pub const KpBinary: Keycode = Keycode::KP_BINARY;
    pub const KpOctal: Keycode = Keycode::KP_OCTAL;
    pub const KpDecimal: Keycode = Keycode::KP_DECIMAL;
    pub const KpHexadecimal: Keycode = Keycode::KP_HEXADECIMAL;
    pub const LCtrl: Keycode = Keycode::LCTRL;
    pub const LShift: Keycode = Keycode::LSHIFT;
    pub const LAlt: Keycode = Keycode::LALT;
    pub const LGui: Keycode = Keycode::LGUI;
    pub const RCtrl: Keycode = Keycode::RCTRL;
    pub const RShift: Keycode = Keycode::RSHIFT;
    pub const RAlt: Keycode = Keycode::RALT;
    pub const RGui: Keycode = Keycode::RGUI;
    pub const Mode: Keycode = Keycode::MODE;
    pub const AudioNext: Keycode = Keycode::AUDIONEXT;
    pub const AudioPrev: Keycode = Keycode::AUDIOPREV;
    pub const AudioStop: Keycode = Keycode::AUDIOSTOP;
    pub const AudioPlay: Keycode = Keycode::AUDIOPLAY;
    pub const AudioMute: Keycode = Keycode::AUDIOMUTE;
    pub const MediaSelect: Keycode = Keycode::MEDIASELECT;
    pub const Www: Keycode = Keycode::WWW;
    pub const Mail: Keycode = Keycode::MAIL;
    pub const Calculator: Keycode = Keycode::CALCULATOR;
    pub const Computer: Keycode = Keycode::COMPUTER;
    pub const AcSearch: Keycode = Keycode::AC_SEARCH;
    pub const AcHome: Keycode = Keycode::AC_HOME;
    pub const AcBack: Keycode = Keycode::AC_BACK;
    pub const AcForward: Keycode = Keycode::AC_FORWARD;
    pub const AcStop: Keycode = Keycode::AC_STOP;
    pub const AcRefresh: Keycode = Keycode::AC_REFRESH;
    pub const AcBookmarks: Keycode = Keycode::AC_BOOKMARKS;
    pub const BrightnessDown: Keycode = Keycode::BRIGHTNESSDOWN;
    pub const BrightnessUp: Keycode = Keycode::BRIGHTNESSUP;
    pub const DisplaySwitch: Keycode = Keycode::DISPLAYSWITCH;
    pub const KbdIllumToggle: Keycode = Keycode::KBDILLUMTOGGLE;
    pub const KbdIllumDown: Keycode = Keycode::KBDILLUMDOWN;
    pub const KbdIllumUp: Keycode = Keycode::KBDILLUMUP;
    pub const Eject: Keycode = Keycode::EJECT;
    pub const Sleep: Keycode = Keycode::SLEEP;
}

impl Keycode {
    pub fn into_i32(&self) -> i32 {
        self.0
    }

    /// `None` for 0 (no key).
    pub fn from_i32(n: i32) -> Option<Keycode> {
        if n != 0 {
            Some(Keycode(n))
        } else {
            None
        }
    }

    /// The key a name stands for: one character is that character's key (letters in either
    /// case), anything longer is a key name such as "Return" or "Left Shift", any case.
    pub fn from_name(name: &str) -> Option<Keycode> {
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.clone().next()) {
            let c = c.to_ascii_lowercase();
            return Keycode::from_i32(c as i32);
        }
        NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(v, _)| Keycode(*v))
    }

    /// The key's name: "Return", "Left Shift", "F1", or its character in capitals ("A").
    pub fn name(self) -> String {
        if let Some((_, n)) = NAMES.iter().find(|(v, _)| *v == self.0) {
            return (*n).to_string();
        }
        match u32::try_from(self.0).ok().and_then(char::from_u32) {
            Some(c) if self.0 > 0 => c.to_uppercase().collect(),
            _ => String::new(),
        }
    }
}

impl fmt::Display for Keycode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Deref for Keycode {
    type Target = i32;

    fn deref(&self) -> &i32 {
        &self.0
    }
}

impl From<Keycode> for i32 {
    fn from(val: Keycode) -> Self {
        val.0
    }
}

/// Key names, as SDL spells them (value, name). Only keys with a name of their own.
const NAMES: &[(i32, &str)] = &[
    (8, "Backspace"),
    (9, "Tab"),
    (13, "Return"),
    (27, "Escape"),
    (32, "Space"),
    (33, "!"),
    (34, "\""),
    (35, "#"),
    (36, "$"),
    (37, "%"),
    (38, "&"),
    (39, "'"),
    (40, "("),
    (41, ")"),
    (42, "*"),
    (43, "+"),
    (44, ","),
    (45, "-"),
    (46, "."),
    (47, "/"),
    (48, "0"),
    (49, "1"),
    (50, "2"),
    (51, "3"),
    (52, "4"),
    (53, "5"),
    (54, "6"),
    (55, "7"),
    (56, "8"),
    (57, "9"),
    (58, ":"),
    (59, ";"),
    (60, "<"),
    (61, "="),
    (62, ">"),
    (63, "?"),
    (64, "@"),
    (91, "["),
    (92, "\\"),
    (93, "]"),
    (94, "^"),
    (95, "_"),
    (96, "`"),
    (97, "A"),
    (98, "B"),
    (99, "C"),
    (100, "D"),
    (101, "E"),
    (102, "F"),
    (103, "G"),
    (104, "H"),
    (105, "I"),
    (106, "J"),
    (107, "K"),
    (108, "L"),
    (109, "M"),
    (110, "N"),
    (111, "O"),
    (112, "P"),
    (113, "Q"),
    (114, "R"),
    (115, "S"),
    (116, "T"),
    (117, "U"),
    (118, "V"),
    (119, "W"),
    (120, "X"),
    (121, "Y"),
    (122, "Z"),
    (127, "Delete"),
    (1073741881, "CapsLock"),
    (1073741882, "F1"),
    (1073741883, "F2"),
    (1073741884, "F3"),
    (1073741885, "F4"),
    (1073741886, "F5"),
    (1073741887, "F6"),
    (1073741888, "F7"),
    (1073741889, "F8"),
    (1073741890, "F9"),
    (1073741891, "F10"),
    (1073741892, "F11"),
    (1073741893, "F12"),
    (1073741894, "PrintScreen"),
    (1073741895, "ScrollLock"),
    (1073741896, "Pause"),
    (1073741897, "Insert"),
    (1073741898, "Home"),
    (1073741899, "PageUp"),
    (1073741901, "End"),
    (1073741902, "PageDown"),
    (1073741903, "Right"),
    (1073741904, "Left"),
    (1073741905, "Down"),
    (1073741906, "Up"),
    (1073741907, "Numlock"),
    (1073741908, "Keypad /"),
    (1073741909, "Keypad *"),
    (1073741910, "Keypad -"),
    (1073741911, "Keypad +"),
    (1073741912, "Keypad Enter"),
    (1073741913, "Keypad 1"),
    (1073741914, "Keypad 2"),
    (1073741915, "Keypad 3"),
    (1073741916, "Keypad 4"),
    (1073741917, "Keypad 5"),
    (1073741918, "Keypad 6"),
    (1073741919, "Keypad 7"),
    (1073741920, "Keypad 8"),
    (1073741921, "Keypad 9"),
    (1073741922, "Keypad 0"),
    (1073741923, "Keypad ."),
    (1073741925, "Application"),
    (1073741926, "Power"),
    (1073741927, "Keypad ="),
    (1073741928, "F13"),
    (1073741929, "F14"),
    (1073741930, "F15"),
    (1073741931, "F16"),
    (1073741932, "F17"),
    (1073741933, "F18"),
    (1073741934, "F19"),
    (1073741935, "F20"),
    (1073741936, "F21"),
    (1073741937, "F22"),
    (1073741938, "F23"),
    (1073741939, "F24"),
    (1073741940, "Execute"),
    (1073741941, "Help"),
    (1073741942, "Menu"),
    (1073741943, "Select"),
    (1073741944, "Stop"),
    (1073741945, "Again"),
    (1073741946, "Undo"),
    (1073741947, "Cut"),
    (1073741948, "Copy"),
    (1073741949, "Paste"),
    (1073741950, "Find"),
    (1073741951, "Mute"),
    (1073741952, "VolumeUp"),
    (1073741953, "VolumeDown"),
    (1073741957, "Keypad ,"),
    (1073741958, "Keypad = (AS400)"),
    (1073741977, "AltErase"),
    (1073741978, "SysReq"),
    (1073741979, "Cancel"),
    (1073741980, "Clear"),
    (1073741981, "Prior"),
    (1073741982, "Return"),
    (1073741983, "Separator"),
    (1073741984, "Out"),
    (1073741985, "Oper"),
    (1073741986, "Clear / Again"),
    (1073741987, "CrSel"),
    (1073741988, "ExSel"),
    (1073742000, "Keypad 00"),
    (1073742001, "Keypad 000"),
    (1073742003, "DecimalSeparator"),
    (1073742004, "CurrencyUnit"),
    (1073742005, "CurrencySubUnit"),
    (1073742006, "Keypad ("),
    (1073742007, "Keypad )"),
    (1073742008, "Keypad {"),
    (1073742009, "Keypad }"),
    (1073742010, "Keypad Tab"),
    (1073742011, "Keypad Backspace"),
    (1073742012, "Keypad A"),
    (1073742013, "Keypad B"),
    (1073742014, "Keypad C"),
    (1073742015, "Keypad D"),
    (1073742016, "Keypad E"),
    (1073742017, "Keypad F"),
    (1073742018, "Keypad XOR"),
    (1073742019, "Keypad ^"),
    (1073742020, "Keypad %"),
    (1073742021, "Keypad <"),
    (1073742022, "Keypad >"),
    (1073742023, "Keypad &"),
    (1073742024, "Keypad &&"),
    (1073742025, "Keypad |"),
    (1073742026, "Keypad ||"),
    (1073742027, "Keypad :"),
    (1073742028, "Keypad #"),
    (1073742029, "Keypad Space"),
    (1073742030, "Keypad @"),
    (1073742031, "Keypad !"),
    (1073742032, "Keypad MemStore"),
    (1073742033, "Keypad MemRecall"),
    (1073742034, "Keypad MemClear"),
    (1073742035, "Keypad MemAdd"),
    (1073742036, "Keypad MemSubtract"),
    (1073742037, "Keypad MemMultiply"),
    (1073742038, "Keypad MemDivide"),
    (1073742039, "Keypad +/-"),
    (1073742040, "Keypad Clear"),
    (1073742041, "Keypad ClearEntry"),
    (1073742042, "Keypad Binary"),
    (1073742043, "Keypad Octal"),
    (1073742044, "Keypad Decimal"),
    (1073742045, "Keypad Hexadecimal"),
    (1073742048, "Left Ctrl"),
    (1073742049, "Left Shift"),
    (1073742050, "Left Alt"),
    (1073742051, "Left GUI"),
    (1073742052, "Right Ctrl"),
    (1073742053, "Right Shift"),
    (1073742054, "Right Alt"),
    (1073742055, "Right GUI"),
    (1073742081, "ModeSwitch"),
    (1073742082, "AudioNext"),
    (1073742083, "AudioPrev"),
    (1073742084, "AudioStop"),
    (1073742085, "AudioPlay"),
    (1073742086, "AudioMute"),
    (1073742087, "MediaSelect"),
    (1073742088, "WWW"),
    (1073742089, "Mail"),
    (1073742090, "Calculator"),
    (1073742091, "Computer"),
    (1073742092, "AC Search"),
    (1073742093, "AC Home"),
    (1073742094, "AC Back"),
    (1073742095, "AC Forward"),
    (1073742096, "AC Stop"),
    (1073742097, "AC Refresh"),
    (1073742098, "AC Bookmarks"),
    (1073742099, "BrightnessDown"),
    (1073742100, "BrightnessUp"),
    (1073742101, "DisplaySwitch"),
    (1073742102, "KBDIllumToggle"),
    (1073742103, "KBDIllumDown"),
    (1073742104, "KBDIllumUp"),
    (1073742105, "Eject"),
    (1073742106, "Sleep"),
];

/// Which modifier keys are held (a set of flags, same bit values as SDL's).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
pub struct Mod(u16);

impl Mod {
    pub const NOMOD: Mod = Mod(0x0000);
    pub const LSHIFTMOD: Mod = Mod(0x0001);
    pub const RSHIFTMOD: Mod = Mod(0x0002);
    pub const LCTRLMOD: Mod = Mod(0x0040);
    pub const RCTRLMOD: Mod = Mod(0x0080);
    pub const LALTMOD: Mod = Mod(0x0100);
    pub const RALTMOD: Mod = Mod(0x0200);
    pub const LGUIMOD: Mod = Mod(0x0400);
    pub const RGUIMOD: Mod = Mod(0x0800);
    pub const NUMMOD: Mod = Mod(0x1000);
    pub const CAPSMOD: Mod = Mod(0x2000);
    pub const MODEMOD: Mod = Mod(0x4000);
    pub const RESERVEDMOD: Mod = Mod(0x8000);

    const ALL: u16 = 0x0001 | 0x0002 | 0x0040 | 0x0080 | 0x0100 | 0x0200 | 0x0400 | 0x0800 | 0x1000 | 0x2000 | 0x4000 | 0x8000;

    pub const fn empty() -> Mod {
        Mod(0)
    }
    pub const fn all() -> Mod {
        Mod(Mod::ALL)
    }
    pub const fn bits(&self) -> u16 {
        self.0
    }
    /// The flags in `bits` that exist; any other bit is dropped.
    pub const fn from_bits_truncate(bits: u16) -> Mod {
        Mod(bits & Mod::ALL)
    }
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }
    /// True when every flag in `other` is set.
    pub const fn contains(&self, other: Mod) -> bool {
        self.0 & other.0 == other.0
    }
    /// True when at least one flag in `other` is set.
    pub const fn intersects(&self, other: Mod) -> bool {
        self.0 & other.0 != 0
    }
    pub fn insert(&mut self, other: Mod) {
        self.0 |= other.0;
    }
    pub fn remove(&mut self, other: Mod) {
        self.0 &= !other.0;
    }
}

impl BitOr for Mod {
    type Output = Mod;
    fn bitor(self, rhs: Mod) -> Mod {
        Mod(self.0 | rhs.0)
    }
}
impl BitAnd for Mod {
    type Output = Mod;
    fn bitand(self, rhs: Mod) -> Mod {
        Mod(self.0 & rhs.0)
    }
}
impl BitOrAssign for Mod {
    fn bitor_assign(&mut self, rhs: Mod) {
        self.0 |= rhs.0;
    }
}
impl BitAndAssign for Mod {
    fn bitand_assign(&mut self, rhs: Mod) {
        self.0 &= rhs.0;
    }
}
impl Not for Mod {
    type Output = Mod;
    fn not(self) -> Mod {
        Mod(!self.0 & Mod::ALL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!(Keycode::from_name("Return"), Some(Keycode::Return));
        assert_eq!(Keycode::from_name("left shift"), Some(Keycode::LShift));
        assert_eq!(Keycode::from_name("A"), Some(Keycode::A));
        assert_eq!(Keycode::from_name("a"), Some(Keycode::A));
        assert_eq!(Keycode::from_name("nonsense"), None);
        assert_eq!(Keycode::Escape.name(), "Escape");
        assert_eq!(Keycode::A.name(), "A");
    }

    #[test]
    fn mod_flags_behave_like_a_bit_set() {
        let m = Mod::LCTRLMOD | Mod::LSHIFTMOD;
        assert!(m.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD));
        assert!(!m.intersects(Mod::LALTMOD));
        assert!(m.contains(Mod::LSHIFTMOD));
        assert!(!m.contains(Mod::LCTRLMOD | Mod::RCTRLMOD));
        assert!(Mod::NOMOD.is_empty() && Mod::empty() == Mod::NOMOD);
        assert_eq!(m & Mod::LCTRLMOD, Mod::LCTRLMOD);
        assert_eq!(Mod::from_bits_truncate(Mod::LCTRLMOD.bits() | 0x0004), Mod::LCTRLMOD);
    }
}
