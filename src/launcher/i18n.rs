//! Display language (English / 日本語). UI code keeps English literals and wraps what it draws in
//! [`tr`]: with Japanese selected the table below supplies the text, and any string without an
//! entry falls back to the English original, so an untranslated label never breaks a screen.
//!
//! The language is a process-wide setting (`harbour_lang` in config.json: 0 = English, 1 = 日本語),
//! loaded at start-up and switched from Settings > Display > Language.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ja,
}

impl Lang {
    pub fn from_index(i: i64) -> Lang {
        if i == 1 { Lang::Ja } else { Lang::En }
    }
    pub fn index(self) -> i64 {
        match self {
            Lang::En => 0,
            Lang::Ja => 1,
        }
    }
}

/// Names as shown in the Language pop-up (always in their own language).
pub const LANG_NAMES: [&str; 2] = ["English", "日本語"];

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set(l: Lang) {
    CURRENT.store(l.index() as u8, Ordering::Relaxed);
}

pub fn get() -> Lang {
    Lang::from_index(CURRENT.load(Ordering::Relaxed) as i64)
}

/// Japanese table: (English original, Japanese). Keep entries short: they must fit the same pixel
/// slots as the English (Japanese glyphs are 12 px wide, Latin ones about 5).
pub const JA: &[(&str, &str)] = &[
    // Settings: sections and page titles
    ("Settings", "設定"),
    ("Settings: Display", "設定: 表示"),
    ("Settings: Audio", "設定: サウンド"),
    ("Settings: Sound", "設定: サウンド"),
    ("Settings: Theme", "設定: テーマ"),
    ("Settings: Lock Screen", "設定: ロック画面"),
    ("Settings: Network", "設定: ネットワーク"),
    ("Settings: Bluetooth", "設定: Bluetooth"),
    ("Settings: Devices", "設定: デバイス"),
    ("Settings: System", "設定: システム"),
    ("Settings: Apps", "設定: アプリ"),
        ("Settings: Agent", "設定: エージェント"),
    ("Wi-Fi Networks", "Wi-Fi 一覧"),
    ("Wi-Fi Password", "Wi-Fi パスワード"),
    ("Bluetooth Devices", "Bluetooth 機器"),
    ("About Harbour", "Harbourについて"),
    ("Agent: Connect", "エージェント: 接続"),
    ("Restart", "再起動"),
    ("Power Off", "電源オフ"),
    ("Official Launcher", "公式ランチャー"),
    ("Display", "表示"),
    ("Audio", "サウンド"),
    ("Sound", "サウンド"),
    ("Network", "ネットワーク"),
    ("Devices", "デバイス"),
    ("System", "システム"),
    ("Apps", "アプリ"),
    ("Software", "ソフトウェア"),
    ("Info", "情報"),
    ("Shutdown", "電源"),
    ("Agent", "エージェント"),
    // Display
    ("Brightness", "明るさ"),
    ("Screen off", "画面オフ"),
    ("Lock screen off", "ロック画面オフ"),
    ("Theme", "テーマ"),
    ("Title font", "タイトル書体"),
    ("Invert", "反転"),
    ("Status bar", "ステータスバー"),
    ("Language", "言語"),
    // Lock Screen
    ("Style", "スタイル"),
    ("Auto lock", "自動ロック"),
    ("Show date", "日付を表示"),
    ("Show battery", "電池を表示"),
    ("Wall clock", "壁掛け時計"),
    ("ASCII art", "ASCIIアート"),
    ("7-segment", "7セグ"),
    ("Default", "標準"),
    ("Black", "黒"),
    ("White", "白"),
    // Audio
    ("Volume", "音量"),
    ("Mute", "ミュート"),
    ("Muted", "ミュート中"),
    // Network / Bluetooth
    ("Wi-Fi", "Wi-Fi"),
    ("IP address", "IPアドレス"),
    ("Signal", "電波"),
    ("Ethernet", "有線LAN"),
    ("Scan Wi-Fi", "Wi-Fi検索"),
    ("Scan devices", "機器を検索"),
    ("Not connected", "未接続"),
    ("Connected", "接続済み"),
    ("Saved", "保存済み"),
    ("Paired", "ペア済み"),
    ("Grove 5V out", "Grove 5V出力"),
    ("Cap 5V out", "Cap 5V出力"),
    ("Not available", "利用不可"),
    // System
    ("Battery", "バッテリー"),
    ("Voltage", "電圧"),
    ("Time", "時刻"),
    ("Time zone", "タイムゾーン"),
    ("Clock sync", "時計同期"),
    ("Power off", "電源オフ"),
    ("Yes", "はい"),
    ("No", "いいえ"),
    ("Nothing here", "何もありません"),
    // Software / Agent
    ("Check updates", "更新を確認"),
    ("Up to date", "最新です"),
    ("Official launcher", "公式ランチャー"),
    ("About", "このデバイス"),
    ("Device", "デバイス"),
    ("Model", "モデル"),
    ("Host", "ホスト"),
    ("Kernel", "カーネル"),
    ("Uptime", "稼働時間"),
    ("Memory", "メモリ"),
    ("Disk", "ディスク"),
    ("Temperature", "温度"),
    ("Load", "負荷"),
    ("Credits", "クレジット"),
    ("Wi-Fi IP", "Wi-Fi IP"),
    ("Ethernet IP", "有線LAN IP"),
    ("Setup guide", "設定ガイド"),
    ("Enabled", "有効"),
    ("Provider", "プロバイダ"),
    ("Status", "状態"),
    ("Connect", "接続"),
    ("Never", "なし"),
    ("As screen off", "画面オフと同じ"),
    // Confirm / password dialogs
    ("Restart the Cardputer?", "Cardputerを再起動しますか？"),
    ("Unsaved work in apps is lost.", "保存していない内容は失われます。"),
    ("Power off the Cardputer?", "Cardputerの電源を切りますか？"),
    ("Press the power button to start.", "電源ボタンで起動します。"),
    ("Switch to the official launcher?", "公式ランチャーに切り替えますか？"),
    ("Harbour will close.", "Harbourは終了します。"),
    ("Switch", "切替"),
    ("Password for", "パスワード:"),
    // Progress / notes
    ("Checking network", "ネットワーク確認中"),
    ("Scanning", "検索中"),
    ("Updating", "更新中"),
    ("Connecting", "接続中"),
    ("Disconnecting", "切断中"),
    ("Checking", "確認中"),
    ("Restarting", "再起動中"),
    ("Powering off", "電源オフ中"),
    ("Switching", "切替中"),
    ("No Wi-Fi networks found", "Wi-Fiが見つかりません"),
    ("No devices found", "機器が見つかりません"),
    ("Already connected", "接続済みです"),
    ("Turn Bluetooth on first", "先にBluetoothをオン"),
    ("Password needs 8+ characters", "パスワードは8文字以上"),
    ("Disconnected", "切断しました"),
    ("Working...", "処理中..."),
    ("Switching...", "切替中..."),
    ("Save changes to", "変更を保存しますか:"),
    ("Your changes are lost if you don't save.", "保存しないと変更は失われます。"),
    ("It is removed for good.", "完全に削除されます。"),
    ("Get it back from Go to Trash.", "「ゴミ箱へ移動」で戻せます。"),
    ("Get it back from Files > Go to Trash.", "Files の「ゴミ箱へ移動」で戻せます。"),
    ("Its apps go back to the home screen.", "中のアプリはホームに戻ります。"),
    ("Organize apps into folders?", "アプリをフォルダに整理しますか？"),
    ("No files here", "ファイルがありません"),
    ("Type a file name", "ファイル名を入力"),
    ("That name is a folder", "その名前はフォルダです"),
    ("Don't Save", "保存しない"),
    ("Save", "保存"),
    ("Open", "開く"),
    ("Open:", "開く:"),
    ("Save as:", "保存:"),
    ("New sheet:", "新規:"),
    ("New folder name:", "新しいフォルダ名:"),
    ("Rename to:", "新しい名前:"),
    ("Rename folder to:", "新しい名前:"),
    ("Move", "移動"),
    ("Trash", "ゴミ箱"),
    ("Replace", "置換"),
    ("Organize", "整理"),
    ("Run", "実行"),
    ("Stop", "停止"),
    ("Create", "作成"),
    ("Scan Again", "再検索"),
    ("New file:", "新規:"),
    ("Type a name", "名前を入力"),
    ("That name is not allowed", "その名前は使えません"),
    ("That name is too long", "名前が長すぎます"),
    ("A name cannot contain / or control characters", "名前に / や制御文字は使えません"),
    ("That item cannot be changed", "この項目は変更できません"),
    ("The home folder cannot be changed", "ホームフォルダは変更できません"),
    ("A folder cannot be copied into itself", "フォルダを自身にコピーできません"),
    ("A folder cannot be moved into itself", "フォルダを自身に移動できません"),
    ("Already in the Trash", "すでにゴミ箱にあります"),
    ("It is gone", "見つかりません"),
    ("Not a file", "ファイルではありません"),
    ("Not a text file (not UTF-8)", "テキストではありません(UTF-8以外)"),
    ("Text Editor", "テキスト"),
    ("Images", "画像"),
    ("Sheet", "表計算"),
    ("Music", "音楽"),
    // Menu bands (common)
    ("Quit", "終了"),
    ("Settings...", "設定..."),
    ("Clear", "クリア"),
    ("Undo", "元に戻す"),
    ("Redo", "やり直し"),
    ("Cut", "切り取り"),
    ("Copy", "コピー"),
    ("Paste", "貼り付け"),
    ("Select All", "すべて選択"),
    ("New...", "新規..."),
    ("New", "新規"),
    ("Open...", "開く..."),
    ("Save As...", "別名で保存..."),
    ("Rename...", "名前変更..."),
    ("Rename", "名前変更"),
    ("Delete...", "削除..."),
    ("Reload", "再読込"),
    ("Refresh", "更新"),
    ("Back", "戻る"),
    ("Forward", "進む"),
    ("Home", "ホーム"),
    ("Help", "ヘルプ"),
    ("History", "履歴"),
    ("Search...", "検索..."),
    ("Reset", "リセット"),
    ("Play", "再生"),
    ("Previous City", "前の都市"),
    ("Next City", "次の都市"),
    ("Clear Screen", "画面クリア"),
    ("Go up", "上へ"),
    ("Go Home", "ホームへ"),
    ("Go to Trash", "ゴミ箱へ移動"),
    ("Open With...", "アプリで開く..."),
    ("New Folder...", "新規フォルダ..."),
    ("Move to Folder...", "フォルダへ移動..."),
    ("Rename Folder...", "フォルダ名変更..."),
    ("Delete Folder", "フォルダ削除"),
    ("Auto-organize...", "自動整理..."),
    ("Remove from Folder", "フォルダから出す"),
    ("New Chat", "新規チャット"),
    ("Clear History", "履歴を消去"),
    ("Provider...", "プロバイダ..."),
    ("About Harbour", "Harbourについて"),
    ("Lock Screen", "画面ロック"),
    ("Restart...", "再起動..."),
    ("Power Off...", "電源オフ..."),
    // Dialog buttons
    ("OK", "OK"),
    ("Cancel", "キャンセル"),
    ("Delete", "削除"),
    ("Close", "閉じる"),
];

/// Templates for text with a name or number in it: one `{}` on each side.
pub const JA_PAT: &[(&str, &str)] = &[
    ("Delete \"{}\"?", "「{}」を削除？"),
    ("Delete {}?", "{}を削除？"),
    ("Replace {}?", "{}を置き換え？"),
    ("Move \"{}\" to the Trash?", "「{}」をゴミ箱へ？"),
    ("Delete the folder \"{}\"?", "フォルダ「{}」を削除？"),
    ("Open \"{}\" with:", "「{}」を開くアプリ:"),
    ("Move \"{}\" to:", "「{}」の移動先:"),
    ("in {}", "{} 内"),
    ("Connected to {}", "{}に接続しました"),
    ("Scan failed: {}", "検索に失敗: {}"),
    ("Update check failed: {}", "更新確認に失敗: {}"),
    ("No app opens .{} files", ".{}を開くアプリがありません"),
];

mod ja_camera;
mod ja_files;
mod ja_infoscope;
mod ja_media;
mod ja_talk;
mod ja_tools;

/// Every table: the core one above plus one per group of apps (`i18n/ja_*.rs`, each exporting
/// `JA` and `JA_PAT`).
fn all_ja() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    JA.iter().chain(ja_files::JA).chain(ja_infoscope::JA).chain(ja_media::JA).chain(ja_tools::JA).chain(ja_camera::JA).chain(ja_talk::JA)
}

fn all_pat() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    JA_PAT.iter().chain(ja_files::JA_PAT).chain(ja_infoscope::JA_PAT).chain(ja_media::JA_PAT).chain(ja_tools::JA_PAT).chain(ja_camera::JA_PAT).chain(ja_talk::JA_PAT)
}

fn pattern(english: &str) -> Option<String> {
    for (en, ja) in all_pat() {
        let Some((pre, post)) = en.split_once("{}") else { continue };
        if english.len() >= pre.len() + post.len() && english.starts_with(pre) && english.ends_with(post) {
            let mid = &english[pre.len()..english.len() - post.len()];
            return Some(ja.replacen("{}", mid, 1));
        }
    }
    None
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| all_ja().copied().collect())
}

/// Whether `english` has a Japanese entry (tests: Help rows must be translated).
#[cfg(test)]
pub fn has_ja(english: &str) -> bool {
    table().contains_key(english)
}

/// `english` in language `lang`; the English text itself when there is no entry.
pub fn tr_in(lang: Lang, english: &'static str) -> &'static str {
    match lang {
        Lang::En => english,
        Lang::Ja => table().get(english).copied().unwrap_or(english),
    }
}

/// Section names in the 56 px Settings side list (four Japanese characters at most).
pub fn tr_side(english: &'static str) -> &'static str {
    if get() == Lang::Ja {
        match english {
            "Network" => return "ネット",
            // The Settings section (a noun); the system menu's "Lock Screen" is the action 画面ロック.
            "Lock Screen" => return "ロック画面",
            _ => {}
        }
    } else if english == "Lock Screen" {
        // The narrow (unfocused) sidebar has room for 9 letters: the page title keeps the full name.
        return "Lock";
    }
    tr(english)
}

/// Names of Harbour's own apps, as shown in tiles and title bars. Internal names (folder keys, `goto:`)
/// stay as they are; external apps are never passed through here.
/// Display name of a built-in app, from its package (`Name` / `Name[ja]` in
/// `apps/<id>/<id>.desktop`). Takes the id ("voicememo") or an old internal name ("Recorder").
/// Names that are no package ("Agent", "World Clock") go through the ordinary table.
pub fn app_name(id_or_name: &str) -> String {
    match crate::launcher::packages::find(id_or_name) {
        Some(p) => p.shown_name(),
        None => tr_s(id_or_name),
    }
}

/// `english` in the current language.
pub fn tr(english: &'static str) -> &'static str {
    tr_in(get(), english)
}

/// Like [`tr`] for text that is not a `'static` literal (row labels, values): returns an owned
/// string, the input unchanged when it has no entry.
pub fn tr_s(english: &str) -> String {
    match get() {
        Lang::En => english.to_string(),
        Lang::Ja => match table().get(english) {
            Some(s) => s.to_string(),
            None if pattern(english).is_some() => pattern(english).unwrap_or_default(),
            // "76% Discharging": battery text is a number plus a status word.
            None => english.replace(" sec", "秒").replace(" min", "分").replace(" available", "件あり").replace("Discharging", "放電中").replace("Charging", "充電中").replace("Full", "満充電"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_english() {
        assert_eq!(tr_in(Lang::Ja, "Zzz untranslated"), "Zzz untranslated");
        assert_eq!(tr_in(Lang::Ja, "Brightness"), "明るさ");
        assert_eq!(tr_in(Lang::En, "Brightness"), "Brightness");
    }

    #[test]
    fn no_conflicting_duplicate_keys() {
        let mut seen = std::collections::HashMap::new();
        for (en, ja) in all_ja() {
            if let Some(prev) = seen.insert(*en, *ja) {
                assert_eq!(prev, *ja, "{en:?} has two different translations");
            }
        }
    }

    #[test]
    fn templates_have_one_placeholder_each_side() {
        for (en, ja) in all_pat() {
            assert_eq!(en.matches("{}").count(), 1, "{en}");
            assert_eq!(ja.matches("{}").count(), 1, "{ja}");
        }
    }

    #[test]
    fn lang_index_round_trips() {
        assert_eq!(Lang::from_index(Lang::Ja.index()), Lang::Ja);
        assert_eq!(Lang::from_index(0), Lang::En);
        assert_eq!(Lang::from_index(9), Lang::En);
    }

    /// Every Japanese character must exist in the body font, or it would draw as a blank box.
    #[test]
    fn every_glyph_exists_in_the_font() {
        let font = crate::bdf::BdfFont::load("assets/fonts/shnmk12.bdf").expect("shnmk12.bdf");
        let latin = crate::bdf::BdfFont::load("assets/fonts/shnm6x12r.bdf").expect("shnm6x12r.bdf");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(crate::launcher::packages::APPS_ROOT);
        let apps: Vec<(String, String)> = crate::launcher::packages::scan(&root).into_iter().filter_map(|p| Some((p.name, p.name_ja?))).collect();
        let mut pairs: Vec<(&str, &str)> = all_ja().chain(all_pat()).copied().collect();
        pairs.extend(apps.iter().map(|(a, b)| (a.as_str(), b.as_str())));
        for (en, ja) in pairs {
            for ch in ja.chars() {
                let ok = if ch.is_ascii() { latin.glyph_unicode(ch as u32).is_some() || ch == ' ' } else { crate::jis::unicode_to_jis(ch).and_then(|j| font.glyph_jis(j as u32)).is_some() };
                assert!(ok, "'{ch}' in {en:?} -> {ja:?} has no glyph");
            }
        }
    }
}
