//! HarbourTalk's board: the posts, the settings and cache on disk, and the talk with the server.
//!
//! The server is a small HTTP + JSON service (`server/harbourtalk/`, a Cloudflare Worker):
//!
//! - `GET  <server>/rooms`  -> `{"rooms": [Room, ...]}`: the public rooms (the server's owner picks
//!   them; there are no private messages)
//! - `GET  <server>/posts?room=R&limit=N`  -> `{"posts": [Post, ...]}` newest first
//! - `POST <server>/posts`  body `{"room", "name", "dev", "text"}` -> `{"post": Post}`, or an error
//!   status with `{"error": "..."}` (429 when the device or address posted too recently)
//!
//! Every request carries the app key (`X-Harbour-Key`) and the device id (`X-Harbour-Device`):
//! the server answers only Harbour (in practice; the key is in the source, so it keeps casual
//! visitors out and can be changed on the server when it leaks).
//!
//! On the device and the Mac the request goes through curl on a worker thread; the web build has
//! no processes or threads, so it uses the browser's own XMLHttpRequest (synchronous, through
//! `emscripten_run_script_string`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Longest post (characters).
pub const MAX_TEXT: usize = 200;
/// Longest handle name (characters).
pub const MAX_NAME: usize = 16;
/// Shortest time between two posts from this device (seconds). The server checks it too.
pub const MIN_GAP: i64 = 30;
/// Posts asked for and kept in the offline copy.
pub const KEEP: usize = 100;
/// Biggest answer read from the server (bytes).
const LIMIT: usize = 512 * 1024;
/// The address used when the settings name none: the board in `server/harbourtalk/`, deployed.
/// The setting `demo` picks the demo board on this device instead.
pub const DEFAULT_SERVER: &str = "https://harbourtalk.kurogedelic.workers.dev";
/// The app key sent with every request (see the module notes: a door sign, not a lock).
pub const DEFAULT_KEY: &str = "harbourtalk-1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    /// The room it was posted in.
    #[serde(default)]
    pub room: String,
    /// The server's id (a number as text); local posts of the demo board count from 1 too.
    pub id: String,
    pub name: String,
    /// The poster's device id (shown as its first 4 characters: `kuro#3fa2`).
    pub dev: String,
    pub text: String,
    /// Seconds since the epoch.
    pub time: i64,
}

impl Post {
    /// `name#3fa2`.
    pub fn author(&self) -> String {
        format!("{}#{}", self.name, short_dev(&self.dev))
    }
}

pub fn short_dev(dev: &str) -> String {
    dev.chars().take(4).collect()
}

/// The handle name as it will be sent: trimmed, no control characters, at most [`MAX_NAME`].
pub fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && *c != '#').collect::<String>().trim().chars().take(MAX_NAME).collect()
}

/// A post's text as it will be sent: trimmed, newlines and tabs as spaces, at most [`MAX_TEXT`].
pub fn clean_text(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    t.trim().chars().take(MAX_TEXT).collect()
}

/// A public room: everyone who reaches the board reads and writes the same posts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    pub id: String,
    pub name: String,
    /// Posts kept in it.
    #[serde(default)]
    pub count: u32,
    /// When the last one was posted (seconds since the epoch, 0 = never).
    #[serde(default)]
    pub last: i64,
}

/// Room ids go into addresses: lower-case letters, digits and `-`, at most 24.
pub fn is_room_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 24 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What is kept in `~/.config/harbour/harbourtalk.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Handle name; empty until the first post asks for one.
    pub name: String,
    /// This device's id: 8 hex digits made on first use.
    pub dev: String,
    /// Server address; empty = [`DEFAULT_SERVER`], `demo` = the demo board.
    pub server: String,
    /// App key; empty = [`DEFAULT_KEY`].
    pub key: String,
    /// When this device last posted (seconds since the epoch), for the client-side gap.
    pub last_post: i64,
}

impl Settings {
    pub fn server(&self) -> &str {
        if self.server.trim().is_empty() { DEFAULT_SERVER } else { self.server.trim() }
    }
    pub fn key(&self) -> &str {
        if self.key.is_empty() { DEFAULT_KEY } else { &self.key }
    }
    /// Seconds to wait before this device may post again (0 = now).
    pub fn wait(&self, now: i64) -> i64 {
        if self.last_post <= 0 {
            return 0;
        }
        (self.last_post + MIN_GAP - now).clamp(0, MIN_GAP)
    }
}

/// 8 hex digits from the process's random hash keys and the clock (no random crate needed).
pub fn new_device_id() -> String {
    if crate::rt::frozen() {
        return "0badc0de".into();
    }
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.write_u32(std::process::id());
    format!("{:08x}", h.finish() as u32)
}

/// Where the settings and the offline copy live (`None`: in memory only, for mock runs and tests).
#[derive(Clone, Debug)]
pub struct Store {
    pub dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    /// The server the posts came from (a new server starts from nothing).
    pub server: String,
    pub fetched: Option<i64>,
    pub rooms: Vec<Room>,
    /// Each room's newest posts, by room id.
    pub posts: std::collections::BTreeMap<String, Vec<Post>>,
}

impl Store {
    pub fn home() -> Store {
        Store { dir: std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/harbour")) }
    }
    fn path(&self, name: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(name))
    }
    pub fn load_settings(&self) -> Settings {
        self.path("harbourtalk.json").and_then(|p| read_json(&p)).unwrap_or_default()
    }
    pub fn save_settings(&self, s: &Settings) -> Result<(), String> {
        self.path("harbourtalk.json").map_or(Ok(()), |p| write_json(&p, s))
    }
    pub fn load_cache(&self) -> Cache {
        self.path("harbourtalk-cache.json").and_then(|p| read_json(&p)).unwrap_or_default()
    }
    pub fn save_cache(&self, c: &Cache) -> Result<(), String> {
        self.path("harbourtalk-cache.json").map_or(Ok(()), |p| write_json(&p, c))
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(p: &std::path::Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn write_json<T: Serialize>(p: &std::path::Path, v: &T) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())
}

/// Where the posts come from: the demo board (mock runs, or no server set) or a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Board {
    Demo,
    Server { url: String, key: String, dev: String },
}

impl Board {
    pub fn for_settings(s: &Settings, mock: bool) -> Board {
        let url = s.server().trim_end_matches('/').to_string();
        if mock || url.is_empty() || url == "demo" {
            Board::Demo
        } else {
            Board::Server { url, key: s.key().to_string(), dev: s.dev.clone() }
        }
    }
}

/// What came back from the board (on the worker).
#[derive(Debug)]
pub enum Reply {
    Rooms(Result<Vec<Room>, String>),
    /// A room's posts (by room id).
    Posts(String, Result<Vec<Post>, String>),
    Posted(Result<Post, String>),
}

/// Read the list of rooms (call it on a worker; the demo board answers at once).
pub fn load_rooms(board: &Board, now: i64) -> Result<Vec<Room>, String> {
    match board {
        Board::Demo => Ok(demo_rooms(&demo_posts(now))),
        Board::Server { url, key, dev } => {
            let (status, body) = request("GET", &format!("{url}/rooms"), key, dev, None)?;
            if status != 200 {
                return Err(error_text(status, &body));
            }
            parse_rooms(&body)
        }
    }
}

/// Read a room's newest posts (call it on a worker; the demo board answers at once).
pub fn load(board: &Board, room: &str, now: i64) -> Result<Vec<Post>, String> {
    if !is_room_id(room) {
        return Err("No such room".into());
    }
    match board {
        Board::Demo => Ok(demo_posts(now).into_iter().filter(|p| p.room == room).collect()),
        Board::Server { url, key, dev } => {
            let (status, body) = request("GET", &format!("{url}/posts?room={room}&limit={KEEP}"), key, dev, None)?;
            if status != 200 {
                return Err(error_text(status, &body));
            }
            parse_posts(&body)
        }
    }
}

/// Send one post (call it on a worker). The demo board makes the post here.
pub fn send(board: &Board, room: &str, name: &str, dev: &str, text: &str, now: i64) -> Result<Post, String> {
    if !is_room_id(room) {
        return Err("No such room".into());
    }
    match board {
        Board::Demo => Ok(Post { room: room.into(), id: format!("local-{now}"), name: name.into(), dev: dev.into(), text: text.into(), time: now }),
        Board::Server { url, key, dev: d } => {
            let body = serde_json::json!({ "room": room, "name": name, "dev": dev, "text": text }).to_string();
            let (status, reply) = request("POST", &format!("{url}/posts"), key, d, Some(&body))?;
            if status != 200 && status != 201 {
                return Err(error_text(status, &reply));
            }
            #[derive(Deserialize)]
            struct One {
                post: Post,
            }
            serde_json::from_str::<One>(&reply).map(|o| o.post).map_err(|_| "The server's answer was not understood".into())
        }
    }
}

pub fn parse_rooms(body: &str) -> Result<Vec<Room>, String> {
    #[derive(Deserialize)]
    struct Many {
        rooms: Vec<Room>,
    }
    let v = serde_json::from_str::<Many>(body).map_err(|_| "The server's answer was not understood".to_string())?.rooms;
    Ok(v.into_iter().filter(|r| is_room_id(&r.id)).collect())
}

pub fn parse_posts(body: &str) -> Result<Vec<Post>, String> {
    #[derive(Deserialize)]
    struct Many {
        posts: Vec<Post>,
    }
    let mut v = serde_json::from_str::<Many>(body).map_err(|_| "The server's answer was not understood".to_string())?.posts;
    v.sort_by(|a, b| b.time.cmp(&a.time));
    v.truncate(KEEP);
    Ok(v)
}

/// A short reason for a failed request: the server's own `error` when it gave one.
pub fn error_text(status: u16, body: &str) -> String {
    #[derive(Deserialize)]
    struct E {
        error: String,
    }
    if let Ok(e) = serde_json::from_str::<E>(body) {
        if !e.error.is_empty() {
            return e.error.chars().take(80).collect();
        }
    }
    match status {
        401 | 403 => "The server refused this app".into(),
        429 => "Posting too fast. Wait a little".into(),
        _ => format!("Server error {status}"),
    }
}

/// The seconds in the server's "Wait 12 s to post again".
pub fn wait_seconds(e: &str) -> Option<i64> {
    let n: i64 = e.strip_prefix("Wait ")?.split(' ').next()?.parse().ok()?;
    Some(n.clamp(1, MIN_GAP))
}

pub fn is_web_address(url: &str) -> bool {
    let u = url.trim();
    (u.starts_with("https://") || u.starts_with("http://")) && u.len() > 10 && !u.chars().any(|c| c.is_whitespace())
}

/// One HTTP request: (status, body).
#[cfg(not(target_os = "emscripten"))]
fn request(method: &str, url: &str, key: &str, dev: &str, body: Option<&str>) -> Result<(u16, String), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if !is_web_address(url) {
        return Err("Not a web address".into());
    }
    let mut cmd = Command::new("curl");
    cmd.args(["--silent", "--show-error", "--max-time", "15", "--connect-timeout", "8", "--max-filesize", "600000",
              "--proto", "=http,https", "--compressed", "--user-agent", "HarbourTalk/1",
              "--write-out", "\n%{http_code}", "--request", method])
        .arg("--header").arg(format!("X-Harbour-Key: {key}"))
        .arg("--header").arg(format!("X-Harbour-Device: {dev}"));
    if body.is_some() {
        cmd.args(["--header", "Content-Type: application/json", "--data-binary", "@-"]);
    }
    cmd.arg("--").arg(url).stdin(if body.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("curl: {e}"))?;
    if let (Some(b), Some(mut sin)) = (body, child.stdin.take()) {
        let _ = sin.write_all(b.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    split_status(&String::from_utf8_lossy(&out.stdout))
}

/// One HTTP request through the browser (synchronous XMLHttpRequest): (status, body).
#[cfg(target_os = "emscripten")]
fn request(method: &str, url: &str, key: &str, dev: &str, body: Option<&str>) -> Result<(u16, String), String> {
    use std::ffi::{CStr, CString};
    extern "C" {
        fn emscripten_run_script_string(script: *const std::os::raw::c_char) -> *const std::os::raw::c_char;
    }
    if !is_web_address(url) {
        return Err("Not a web address".into());
    }
    let q = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let script = format!(
        "(function(){{try{{var x=new XMLHttpRequest();x.open({m},{u},false);\
         x.setRequestHeader('X-Harbour-Key',{k});x.setRequestHeader('X-Harbour-Device',{d});\
         var b={b};if(b!==null)x.setRequestHeader('Content-Type','application/json');\
         x.send(b);return x.responseText+'\\n'+x.status;}}catch(e){{return '\\n0';}}}})()",
        m = q(method), u = q(url), k = q(key), d = q(dev), b = body.map(q).unwrap_or_else(|| "null".into())
    );
    let c = CString::new(script).map_err(|_| "Bad request".to_string())?;
    let out = unsafe {
        let p = emscripten_run_script_string(c.as_ptr());
        if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    };
    split_status(&out)
}

/// curl's output with `--write-out "\n%{http_code}"`: the body, a newline, the status.
pub fn split_status(out: &str) -> Result<(u16, String), String> {
    let (body, code) = out.rsplit_once('\n').unwrap_or(("", out));
    let status: u16 = code.trim().parse().unwrap_or(0);
    if status == 0 {
        return Err("Offline".into());
    }
    if body.len() > LIMIT {
        return Err("The answer is too large".into());
    }
    Ok((status, body.to_string()))
}

/// The demo board's posts (mock runs and a device with no server set), newest first.
pub fn demo_posts(now: i64) -> Vec<Post> {
    let p = |id: u32, room: &str, name: &str, dev: &str, ago: i64, text: &str| Post {
        room: room.into(),
        id: id.to_string(),
        name: name.into(),
        dev: dev.into(),
        text: text.into(),
        time: now - ago,
    };
    vec![
        p(8, "lobby", "kuro", "3fa29c01", 40, "HarbourTalk のテストです。日本語も書けます。"),
        p(7, "lobby", "mio", "b7e104aa", 5 * 60, "Wi-Fi つながった! 港の掲示板へようこそ。"),
        p(6, "music", "harbor", "0c55e2d9", 32 * 60, "Anyone tried the new Yacht sequencer? Sixteen steps is just enough for a bassline."),
        p(5, "cardputer", "sora", "91d3f7b2", 2 * 3600, "通勤中に Cardputer で書いています。キーが小さいけど慣れると速い。"),
        p(4, "chat", "mio", "b7e104aa", 5 * 3600, "Infoscope でニュースを読んで、気になったことをここに書く流れが便利。"),
        p(3, "lobby", "deckhand", "5a0e7c43", 26 * 3600, "Tip: Ctrl+R reloads the timeline. Posts are kept for offline reading too."),
        p(2, "cardputer", "kuro", "3fa29c01", 3 * 86400, "長い投稿はこんなふうに折り返されます。一つの投稿は二百文字まで。連続投稿は三十秒あけてください。Enter で全文を読めます。"),
        p(1, "lobby", "harbor", "0c55e2d9", 6 * 86400, "First post on the board. Be kind."),
        p(9, "music", "sora", "91d3f7b2", 50 * 3600, "AcidZero のベースライン、夜に聴くと最高。"),
        p(10, "chat", "deckhand", "5a0e7c43", 30 * 3600, "Rainy day at the harbour. Coffee and a small screen."),
    ]
}

/// The demo board's rooms (names in English; the app shows them translated), counted from `posts`.
pub fn demo_rooms(posts: &[Post]) -> Vec<Room> {
    [("lobby", "Lobby"), ("cardputer", "Cardputer"), ("music", "Music"), ("chat", "Chitchat")]
        .iter()
        .map(|(id, name)| {
            let mine = posts.iter().filter(|p| p.room == *id);
            Room { id: id.to_string(), name: name.to_string(), count: mine.clone().count() as u32, last: mine.map(|p| p.time).max().unwrap_or(0) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_texts_are_cleaned() {
        assert_eq!(clean_name("  kuro#x\n "), "kurox");
        assert_eq!(clean_name("abcdefghijklmnopqrstu").chars().count(), MAX_NAME);
        assert_eq!(clean_text(" a\nb\t "), "a b");
        assert_eq!(clean_text(&"あ".repeat(300)).chars().count(), MAX_TEXT);
    }

    #[test]
    fn device_ids_are_eight_hex_digits_and_shown_as_four() {
        let d = new_device_id();
        assert_eq!(d.len(), 8);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
        let p = Post { room: "lobby".into(), id: "1".into(), name: "kuro".into(), dev: "3fa29c01".into(), text: "x".into(), time: 0 };
        assert_eq!(p.author(), "kuro#3fa2");
    }

    #[test]
    fn the_gap_between_posts_counts_down() {
        let s = Settings { last_post: 1000, ..Default::default() };
        assert_eq!(s.wait(1000), MIN_GAP);
        assert_eq!(s.wait(1010), MIN_GAP - 10);
        assert_eq!(s.wait(1000 + MIN_GAP), 0);
        assert_eq!(Settings::default().wait(5), 0);
    }

    #[test]
    fn the_board_is_the_demo_without_a_server_or_in_mock_runs() {
        let mut s = Settings { dev: "abcd1234".into(), ..Default::default() };
        assert!(matches!(Board::for_settings(&s, false), Board::Server { ref url, .. } if url == DEFAULT_SERVER), "no server set: the standard board");
        assert_eq!(Board::for_settings(&s, true), Board::Demo, "mock runs never go out");
        s.server = "demo".into();
        assert_eq!(Board::for_settings(&s, false), Board::Demo);
        s.server = "https://talk.example.net/".into();
        assert_eq!(Board::for_settings(&s, true), Board::Demo);
        assert_eq!(
            Board::for_settings(&s, false),
            Board::Server { url: "https://talk.example.net".into(), key: DEFAULT_KEY.into(), dev: "abcd1234".into() }
        );
    }

    #[test]
    fn server_answers_are_read() {
        let rooms = parse_rooms(r#"{"rooms":[{"id":"lobby","name":"Lobby","count":3,"last":9},{"id":"Bad id","name":"x"}]}"#).unwrap();
        assert_eq!(rooms, [Room { id: "lobby".into(), name: "Lobby".into(), count: 3, last: 9 }], "bad ids are dropped");
        let body = r#"{"posts":[{"id":"1","name":"a","dev":"11112222","text":"old","time":5},
                                {"id":"2","name":"b","dev":"33334444","text":"new","time":9}]}"#;
        let v = parse_posts(body).unwrap();
        assert_eq!(v.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["new", "old"], "newest first");
        assert!(parse_posts("<html>").is_err());
        assert_eq!(error_text(429, r#"{"error":"Wait 12 s"}"#), "Wait 12 s");
        assert_eq!(error_text(429, ""), "Posting too fast. Wait a little");
        assert_eq!(error_text(500, "oops"), "Server error 500");
        assert_eq!(split_status("{\"a\":1}\n200").unwrap(), (200, "{\"a\":1}".into()));
        assert_eq!(split_status("line1\nline2\n404").unwrap(), (404, "line1\nline2".into()));
        assert_eq!(split_status("\n000"), Err("Offline".into()));
        assert_eq!(wait_seconds("Wait 12 s to post again"), Some(12));
        assert_eq!(wait_seconds("Server error 500"), None);
    }

    #[test]
    fn requests_refuse_non_web_addresses() {
        let b = Board::Server { url: "file:///etc".into(), key: "k".into(), dev: "d".into() };
        assert_eq!(load(&b, "lobby", 0), Err("Not a web address".into()));
        assert_eq!(load(&b, "../x", 0), Err("No such room".into()), "room ids never reach an address unchecked");
        assert!(is_room_id("cardputer") && is_room_id("a-1") && !is_room_id("A") && !is_room_id("a b") && !is_room_id(""));
        assert!(!is_web_address("https://a b.c/x"));
        assert!(is_web_address("https://talk.example.net"));
    }

    #[test]
    fn the_demo_board_has_posts_and_takes_new_ones() {
        let rooms = load_rooms(&Board::Demo, 100_000).unwrap();
        assert_eq!(rooms.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["lobby", "cardputer", "music", "chat"]);
        assert_eq!(rooms[0].count, 4);
        let v = load(&Board::Demo, "lobby", 100_000).unwrap();
        assert_eq!(v.len(), 4);
        assert!(v.iter().all(|p| p.room == "lobby"));
        assert!(v.windows(2).all(|w| w[0].time >= w[1].time));
        let p = send(&Board::Demo, "music", "kuro", "3fa29c01", "hello", 7).unwrap();
        assert_eq!((p.room.as_str(), p.text.as_str(), p.time), ("music", "hello", 7));
    }

    #[test]
    fn settings_and_cache_survive_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("harbour-talk-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = Store { dir: Some(dir.clone()) };
        assert_eq!(st.load_settings(), Settings::default());
        let s = Settings { name: "kuro".into(), dev: "3fa29c01".into(), last_post: 9, ..Default::default() };
        st.save_settings(&s).unwrap();
        assert_eq!(st.load_settings(), s);
        let posts = demo_posts(1000);
        let c = Cache { server: "x".into(), fetched: Some(3), rooms: demo_rooms(&posts), posts: [("lobby".to_string(), posts)].into() };
        st.save_cache(&c).unwrap();
        assert_eq!(st.load_cache(), c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
