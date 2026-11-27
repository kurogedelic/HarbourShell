//! Meta Muse as an Agent provider (see docs/MUSE.md and docs/AGENT.md).
//!
//! Muse is not a chat-completions API. The Linux Device SDK (`musegadget`, Python) runs as its
//! own systemd service, pairs with the Muse phone app over BLE and keeps an encrypted session to
//! Muse. We only talk to that service, in two directions:
//!
//! * **Agent -> Muse** ([`MuseProvider`]): the user's line goes as one JSON line to
//!   `/run/musegadget/musegadget.sock` (`{"message","session_id","wait"}` ->
//!   `{"ok","reply","partial","detail"}`); Muse's text answer comes back. Muse keeps the
//!   conversation (a side chat, so the phone chat is not mixed in); we send only the last line.
//! * **Muse -> Harbour** ([`spawn_command_server`]): every Harbour tool is registered with Muse as
//!   a custom command (`harbour.<tool>`, files in `tools/muse_bridge/commands.d/`). Muse runs
//!   `tools/muse_bridge/harbour-muse-cmd`, which forwards to a user-only Unix socket served here.
//!   Each call becomes a [`RemoteCall`] for the Agent screen, which runs it through the same
//!   `tools::execute` and the same confirmation card as a model's tool call.
//!
//! The SDK token is never seen by the launcher: it stays in the SDK's root-only file.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::config::ProviderCfg;
use super::provider::Provider;
use super::tools;
use super::types::{ChatRequest, ProviderEvent, Role, ToolCall};

pub const DEFAULT_SOCKET: &str = "/run/musegadget/musegadget.sock";
pub const DEFAULT_WAIT_SECS: u32 = 60;
pub const MAX_WAIT_SECS: u32 = 600; // SDK limit
/// How long a Muse command may wait for the user (confirmation card) before it is answered "no".
pub const REMOTE_WAIT: Duration = Duration::from_secs(55);
const MAX_LINE: u64 = 256 * 1024;
const CMD_PREFIX: &str = "harbour.";

// ---- errors -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum MuseError {
    /// Socket missing / service not running: SDK not installed or stopped.
    SdkMissing,
    /// Socket exists but this user is not in the service's group.
    NoPermission,
    /// Service up but not registered with Muse (not paired, no token, or no network).
    Offline,
    /// Muse did not answer in time (`detail`).
    NoAnswer(String),
    /// The service could not reach Muse's cloud *and says the message was not sent* (stream
    /// error, HTTP 400/502, dropped connection). Safe to retry; usually clears in seconds.
    NotSent(String),
    /// The send may or may not have been delivered.
    Uncertain(String),
    Timeout,
    Remote(String),
    Protocol(String),
}

impl MuseError {
    /// Short plain text for the 320x170 screen, in the current UI language. Says what to do.
    pub fn user_message(&self) -> String {
        self.user_message_in(crate::launcher::i18n::get() == crate::launcher::i18n::Lang::Ja)
    }

    pub fn user_message_in(&self, ja: bool) -> String {
        let t = |en: String, jp: String| if ja { jp } else { en };
        match self {
            MuseError::SdkMissing => t(
                "Muse service is not running. Install/start musegadget (docs/MUSE.md)".into(),
                "Museサービスが動いていません。musegadget を入れて起動してください(docs/MUSE.md)".into(),
            ),
            MuseError::NoPermission => t(
                "No access to the Muse socket. Add this user to its group, then log in again".into(),
                "Museのソケットを使う権限がありません。このユーザーをそのグループに追加し、再ログインしてください".into(),
            ),
            MuseError::Offline => t(
                "Not connected to Muse. Pair it again or check Wi-Fi and token: sudo musegadget doctor".into(),
                "Museにつながっていません。ペアリング・Wi-Fi・トークンを確認: sudo musegadget doctor".into(),
            ),
            MuseError::NotSent(_) => t(
                "Muse could not be reached just now. Nothing was sent. Try again in a moment".into(),
                "いまMuseに届きませんでした。何も送られていません。少し待ってもう一度".into(),
            ),
            MuseError::NoAnswer(d) => t(format!("Muse gave no answer: {d}"), format!("Museから返事がありません: {d}")),
            MuseError::Uncertain(d) => t(format!("Maybe not delivered to Muse: {d}"), format!("Museに届いたか不明です: {d}")),
            MuseError::Timeout => t("Muse took too long. Try again".into(), "Museの返事が遅すぎました。もう一度".into()),
            MuseError::Remote(e) => format!("Muse: {e}"),
            MuseError::Protocol(e) => t(format!("Muse protocol error: {e}"), format!("Museとの通信エラー: {e}")),
        }
    }
}

impl From<io::Error> for MuseError {
    fn from(e: io::Error) -> Self {
        use io::ErrorKind::*;
        match e.kind() {
            NotFound | ConnectionRefused => MuseError::SdkMissing,
            PermissionDenied => MuseError::NoPermission,
            TimedOut | WouldBlock => MuseError::Timeout,
            _ => MuseError::Protocol(e.to_string()),
        }
    }
}

// ---- settings ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct MuseSettings {
    pub socket: PathBuf,
    /// `None` = the main Muse chat.
    pub session_id: Option<String>,
    pub wait_secs: u32,
    pub commands_socket: PathBuf,
    /// Push-to-talk voice on/off (default on; needs `vm_id`).
    pub voice: bool,
    pub vm_id: Option<String>,
    /// The `musegadget` command (PATH name or absolute path); never run through a shell.
    pub sdk_command: String,
}

/// Default Harbour command socket: `$XDG_RUNTIME_DIR/harbour/muse-cmd.sock` (the same rule as
/// `harbour-muse-cmd`).
pub fn default_commands_socket() -> PathBuf {
    if let Ok(p) = std::env::var("HARBOUR_MUSE_CMD_SOCK") {
        if !p.is_empty() {
            return p.into();
        }
    }
    let rt = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    PathBuf::from(rt).join("harbour").join("muse-cmd.sock")
}

impl MuseSettings {
    /// From a provider entry of kind `muse`. No secret is read: the SDK keeps its own token.
    pub fn from_cfg(p: &ProviderCfg) -> Result<Self, String> {
        let m = p.muse.clone().unwrap_or_default();
        let abs = |v: Option<String>, key: &str, d: PathBuf| -> Result<PathBuf, String> {
            match v {
                None => Ok(d),
                Some(s) if Path::new(&s).is_absolute() => Ok(s.into()),
                Some(_) => Err(format!("muse.{key} must be an absolute path")),
            }
        };
        let session_id = match m.session_id {
            // Omitted or "" = the main Muse chat. (A side chat id such as "harbour" makes the SDK fail with
            // HTTP 400 on the device so far, so it is only used when asked for explicitly.)
            None => None,
            Some(s) if s.is_empty() => None,
            Some(s) => {
                if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Err("muse.session_id: letters, digits and dashes only".into());
                }
                Some(s)
            }
        };
        let wait_secs = m.wait_secs.unwrap_or(DEFAULT_WAIT_SECS);
        if !(1..=MAX_WAIT_SECS).contains(&wait_secs) {
            return Err(format!("muse.wait_secs must be 1..{MAX_WAIT_SECS}"));
        }
        let vm_id = match m.vm_id {
            None => None,
            Some(v) if v.trim().is_empty() => None,
            Some(v) => {
                let v = v.trim().to_string();
                if v.starts_with('-') || !v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')) {
                    return Err("muse.vm_id: letters, digits and - _ . : only".into());
                }
                Some(v)
            }
        };
        let sdk_command = m.sdk_command.unwrap_or_else(|| "musegadget".into());
        if sdk_command.is_empty() || sdk_command.chars().any(|c| c.is_whitespace() || c.is_control()) || sdk_command.starts_with('-') {
            return Err("muse.sdk_command: one command name or absolute path, no spaces".into());
        }
        Ok(Self {
            voice: m.voice.unwrap_or(true),
            vm_id,
            sdk_command,
            socket: abs(m.socket, "socket", DEFAULT_SOCKET.into())?,
            session_id,
            wait_secs,
            commands_socket: abs(m.commands_socket, "commands_socket", default_commands_socket())?,
        })
    }
}

// ---- transport --------------------------------------------------------------

/// One request line in, one response line out. Faked in tests.
pub trait Link: Send + Sync {
    fn roundtrip(&self, request: &str, timeout: Duration) -> io::Result<String>;
}

pub struct UnixLink(pub PathBuf);

impl Link for UnixLink {
    fn roundtrip(&self, request: &str, timeout: Duration) -> io::Result<String> {
        let mut s = UnixStream::connect(&self.0)?;
        s.set_read_timeout(Some(timeout))?;
        s.set_write_timeout(Some(Duration::from_secs(10)))?;
        s.write_all(request.as_bytes())?;
        s.write_all(b"\n")?;
        let mut line = String::new();
        BufReader::new(s.take(MAX_LINE)).read_line(&mut line)?;
        if line.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "empty reply"));
        }
        Ok(line)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AskReply {
    pub text: String,
    /// Muse was still answering when the wait ran out.
    pub partial: bool,
}

pub fn parse_reply(line: &str) -> Result<AskReply, MuseError> {
    let v: Value = serde_json::from_str(line).map_err(|e| MuseError::Protocol(e.to_string()))?;
    let detail = v.get("detail").and_then(Value::as_str).unwrap_or("").to_string();
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = v.get("error").and_then(Value::as_str).unwrap_or("unknown error");
        return Err(if v.get("sent").and_then(Value::as_str) == Some("unknown") {
            MuseError::Uncertain(err.to_string())
        } else if err.contains("not connected") {
            MuseError::Offline
        } else if err.contains("was not sent") {
            MuseError::NotSent(err.to_string())
        } else {
            MuseError::Remote(err.to_string())
        });
    }
    match v.get("reply").and_then(Value::as_str) {
        Some(t) => Ok(AskReply { text: t.to_string(), partial: v.get("partial").and_then(Value::as_bool).unwrap_or(false) }),
        None => Err(MuseError::NoAnswer(if detail.is_empty() { "no detail".into() } else { detail })),
    }
}

// ---- provider ---------------------------------------------------------------

pub struct MuseProvider<L: Link> {
    name: String,
    cfg: MuseSettings,
    link: L,
    retry_delay: Duration,
}

impl MuseProvider<UnixLink> {
    pub fn new(name: &str, cfg: MuseSettings) -> Self {
        let link = UnixLink(cfg.socket.clone());
        Self { name: name.to_string(), cfg, link, retry_delay: Duration::from_secs(3) }
    }
}

impl<L: Link> MuseProvider<L> {
    pub fn with_link(name: &str, cfg: MuseSettings, link: L) -> Self {
        Self { name: name.to_string(), cfg, link, retry_delay: Duration::from_secs(3) }
    }

    fn request(&self, text: &str) -> String {
        let mut v = json!({"message": text, "wait": self.cfg.wait_secs});
        if let Some(s) = &self.cfg.session_id {
            v["session_id"] = json!(s);
        }
        v.to_string()
    }

    pub fn ask(&self, text: &str) -> Result<AskReply, MuseError> {
        if text.trim().is_empty() {
            return Err(MuseError::Remote("empty message".into()));
        }
        // The SDK bounds the wait itself; add slack for its cleanup.
        let t = Duration::from_secs(self.cfg.wait_secs as u64 + 30);
        let r = self.link.roundtrip(&self.request(text), t).map_err(MuseError::from).and_then(|l| parse_reply(&l));
        // The service says "the message was not sent": nothing reached Muse, so one retry cannot
        // double-send. These are brief cloud hiccups (stream reset, HTTP 400/502).
        if matches!(r, Err(MuseError::NotSent(_))) {
            std::thread::sleep(self.retry_delay);
            return self.link.roundtrip(&self.request(text), t).map_err(MuseError::from).and_then(|l| parse_reply(&l));
        }
        r
    }
}

impl<L: Link> Provider for MuseProvider<L> {
    fn label(&self) -> String {
        if self.name.eq_ignore_ascii_case("muse") { "Muse".into() } else { format!("{}/Muse", self.name) }
    }

    /// Muse keeps the conversation and picks its own tools (the `harbour.*` commands), so only
    /// the last user line is sent and no tool call is ever emitted.
    fn chat(&self, req: &ChatRequest, emit: &mut dyn FnMut(ProviderEvent)) {
        let Some(last) = req.messages.iter().rev().find(|m| m.role == Role::User) else {
            emit(ProviderEvent::Error("nothing to send to Muse".into()));
            return;
        };
        match self.ask(&last.text) {
            Ok(r) => {
                let mut t = r.text;
                if r.partial {
                    t.push_str(" (Muse was still answering)");
                }
                emit(ProviderEvent::TextDelta(t));
                emit(ProviderEvent::Done);
            }
            Err(e) => emit(ProviderEvent::Error(e.user_message())),
        }
    }
}

// ---- connection test --------------------------------------------------------

/// What "Test connection" found. Nothing is sent to Muse; only the local socket is probed.
#[derive(Debug, Clone, PartialEq)]
pub enum Diagnosis {
    /// Socket answered: the service is up and this user may use it.
    ServiceOk,
    Failed(MuseError),
}

/// Probe the local service with a request it rejects at once (no message reaches Muse). This
/// separates "service missing" and "no permission" from a healthy service. Whether the gadget is
/// paired, has a valid token or is online cannot be read without root: the first real message
/// then reports `Offline`/`NotSent`, and the text points at `sudo musegadget doctor`.
pub fn diagnose_with(link: &dyn Link) -> Diagnosis {
    match link.roundtrip("{}", Duration::from_secs(5)) {
        Ok(line) => match serde_json::from_str::<Value>(&line) {
            Ok(v) if v.get("ok").is_some() => Diagnosis::ServiceOk,
            _ => Diagnosis::Failed(MuseError::Protocol("unexpected answer".into())),
        },
        Err(e) => Diagnosis::Failed(e.into()),
    }
}

pub fn diagnose(cfg: &MuseSettings) -> Diagnosis {
    diagnose_with(&UnixLink(cfg.socket.clone()))
}

pub fn diagnosis_text(d: &Diagnosis, ja: bool) -> String {
    match d {
        Diagnosis::ServiceOk => if ja {
            "Museサービスは動いていて使えます。つながらない時は: sudo musegadget doctor".into()
        } else {
            "Muse service is running and usable. If it still fails: sudo musegadget doctor".to_string()
        },
        Diagnosis::Failed(e) => e.user_message_in(ja),
    }
}

// ---- Muse -> Harbour: custom commands --------------------------------------

/// `(command name, drop-in spec)` for every Harbour tool, in the SDK's format. The files in
/// `tools/muse_bridge/commands.d/` must equal these (tested; see `write_command_files`).
pub fn command_specs() -> Vec<(String, Value)> {
    tools::specs()
        .iter()
        .map(|t| {
            let req: Vec<&str> = t.schema["required"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let (mut required, mut optional) = (serde_json::Map::new(), serde_json::Map::new());
            if let Some(props) = t.schema["properties"].as_object() {
                for (k, v) in props {
                    let ty = v.get("type").and_then(Value::as_str).unwrap_or("string");
                    let p = json!({"type": ty, "description": format!("The {k} argument; see the command description.")});
                    if req.contains(&k.as_str()) { required.insert(k.clone(), p) } else { optional.insert(k.clone(), p) };
                }
            }
            let mut spec = json!({
                "description": format!("Harbour Shell: {}", t.description),
                "exec": ["/usr/local/bin/harbour-muse-cmd", t.name],
                "required": required,
                // A side-effect command waits for the user's confirmation card on the device.
                "timeout_ms": if tools::needs_confirm(t.name) { 60000 } else { 15000 },
            });
            if !optional.is_empty() {
                spec["optional"] = Value::Object(optional);
            }
            (format!("{CMD_PREFIX}{}", t.name), spec)
        })
        .collect()
}

/// A command from Muse, waiting for the Agent screen. `deadline` is when the socket side gives up:
/// after that the call must not run any more.
pub struct RemoteCall {
    pub call: ToolCall,
    pub reply: Sender<Result<String, String>>,
    pub deadline: Instant,
}

/// Parse `{"command":"harbour.<tool>","params":{...}}` into a tool call; only real Harbour tools pass.
pub fn parse_command(line: &str, id: &str) -> Result<ToolCall, String> {
    let v: Value = serde_json::from_str(line).map_err(|_| "not JSON".to_string())?;
    let name = v.get("command").and_then(Value::as_str).and_then(|c| c.strip_prefix(CMD_PREFIX)).ok_or("unknown command")?;
    if !tools::specs().iter().any(|t| t.name == name) {
        return Err("unknown command".into());
    }
    let args = match v.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(p) if p.is_object() => p.clone(),
        Some(_) => return Err("params must be an object".into()),
    };
    Ok(ToolCall { id: id.into(), name: name.into(), args })
}

/// Bind the command socket: parent dir 0700, socket 0600, a stale file removed.
pub fn bind_command_socket(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Serve the command socket on a thread. Calls arrive on the returned channel; whoever takes one
/// answers through `RemoteCall::reply`. `wait` bounds how long a connection is held.
pub fn spawn_command_server(path: &Path, wait: Duration) -> io::Result<Receiver<RemoteCall>> {
    let listener = bind_command_socket(path)?;
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut n = 0u64;
        for conn in listener.incoming().flatten() {
            n += 1;
            let tx = tx.clone();
            std::thread::spawn(move || serve_conn(conn, &tx, wait, n));
        }
    });
    Ok(rx)
}

fn serve_conn(mut c: UnixStream, tx: &Sender<RemoteCall>, wait: Duration, n: u64) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let read = BufReader::new((&c).take(MAX_LINE)).read_line(&mut line);
    let reply = match (read, parse_command(&line, &format!("muse_{n}"))) {
        (Err(_), _) => json!({"ok": false, "error": "read failed"}),
        (_, Err(e)) => json!({"ok": false, "error": e}),
        (_, Ok(call)) => {
            let (rtx, rrx) = channel();
            let rc = RemoteCall { call, reply: rtx, deadline: Instant::now() + wait };
            if tx.send(rc).is_err() {
                json!({"ok": false, "error": "Harbour Agent is not running"})
            } else {
                match rrx.recv_timeout(wait) {
                    Ok(Ok(text)) => json!({"ok": true, "result": text}),
                    Ok(Err(e)) => json!({"ok": false, "error": e}),
                    Err(_) => json!({"ok": false, "error": "Harbour Agent did not answer: is the Agent screen open on the device?"}),
                }
            }
        }
    };
    let _ = c.write_all(reply.to_string().as_bytes());
    let _ = c.write_all(b"\n");
}

/// Start the command server when any provider is of kind `muse`. `Err` = why it could not start.
pub fn start_for(cfg: &super::config::AgentConfig) -> Option<Result<Receiver<RemoteCall>, String>> {
    let (name, p) = cfg.providers.iter().find(|(_, p)| p.kind == "muse")?;
    Some(
        MuseSettings::from_cfg(p)
            .map_err(|e| format!("{name}: {e}"))
            .and_then(|s| spawn_command_server(&s.commands_socket, REMOTE_WAIT).map_err(|e| format!("{name}: command socket: {e}"))),
    )
}

pub fn provider(name: &str, p: &ProviderCfg) -> Result<Arc<dyn Provider>, String> {
    let s = MuseSettings::from_cfg(p).map_err(|e| format!("{name}: {e}"))?;
    Ok(Arc::new(MuseProvider::new(name, s)))
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::agent::provider::collect;
    use crate::launcher::agent::types::{ChatOpts, Message};
    use std::sync::Mutex;

    struct Fake {
        sent: Arc<Mutex<Vec<String>>>,
        kind: Option<io::ErrorKind>,
        resp: String,
    }
    impl Link for Fake {
        fn roundtrip(&self, r: &str, _t: Duration) -> io::Result<String> {
            self.sent.lock().unwrap().push(r.to_string());
            match self.kind {
                Some(k) => Err(io::Error::new(k, "x")),
                None => Ok(self.resp.clone()),
            }
        }
    }
    fn settings() -> MuseSettings {
        MuseSettings::from_cfg(&pcfg(None)).unwrap()
    }
    fn pcfg(m: Option<crate::launcher::agent::config::MuseCfg>) -> ProviderCfg {
        ProviderCfg { kind: "muse".into(), base_url: None, model: None, api_key_env: None, api_key: None, max_tokens: None, muse: m }
    }
    fn fake(resp: &str) -> (MuseProvider<Fake>, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(vec![]));
        (MuseProvider::with_link("muse", settings(), Fake { sent: sent.clone(), kind: None, resp: resp.into() }), sent)
    }
    fn failing(k: io::ErrorKind) -> MuseProvider<Fake> {
        MuseProvider::with_link("muse", settings(), Fake { sent: Default::default(), kind: Some(k), resp: String::new() })
    }
    fn run(p: &dyn Provider, text: &str) -> Result<String, String> {
        let msgs = [Message::user(text)];
        let opts = ChatOpts::default();
        collect(p, &ChatRequest { system: "", messages: &msgs, tools: &[], opts: &opts }).map(|(t, calls)| {
            assert!(calls.is_empty(), "Muse never emits tool calls");
            t
        })
    }

    #[test]
    fn settings_defaults_and_validation() {
        let s = settings();
        assert_eq!(s.socket, PathBuf::from(DEFAULT_SOCKET));
        assert_eq!((s.session_id.as_deref(), s.wait_secs), (None, 60), "the main chat by default");
        use crate::launcher::agent::config::MuseCfg;
        let m = |f: &dyn Fn(&mut MuseCfg)| {
            let mut c = MuseCfg::default();
            f(&mut c);
            MuseSettings::from_cfg(&pcfg(Some(c)))
        };
        assert_eq!(m(&|c| c.session_id = Some(String::new())).unwrap().session_id, None);
        assert_eq!(m(&|c| c.session_id = Some("side-1".into())).unwrap().session_id.as_deref(), Some("side-1"), "explicit side chat kept");
        assert_eq!(m(&|c| c.wait_secs = Some(5)).unwrap().wait_secs, 5);
        assert!(m(&|c| c.wait_secs = Some(0)).is_err());
        assert!(m(&|c| c.wait_secs = Some(601)).is_err());
        assert!(m(&|c| c.session_id = Some("a b".into())).is_err());
        assert!(m(&|c| c.socket = Some("rel/p".into())).is_err());
        // Voice: on by default, but only usable with a vm id; the SDK command defaults to PATH lookup.
        assert_eq!((s.voice, s.vm_id.as_deref(), s.sdk_command.as_str()), (true, None, "musegadget"));
        assert_eq!(m(&|c| c.vm_id = Some(" vm-1_a.b:c ".into())).unwrap().vm_id.as_deref(), Some("vm-1_a.b:c"));
        assert_eq!(m(&|c| c.vm_id = Some(String::new())).unwrap().vm_id, None);
        assert!(m(&|c| c.vm_id = Some("--x".into())).is_err() && m(&|c| c.vm_id = Some("a b".into())).is_err());
        assert!(!m(&|c| c.voice = Some(false)).unwrap().voice);
        assert_eq!(m(&|c| c.sdk_command = Some("/opt/musegadget/venv/bin/musegadget".into())).unwrap().sdk_command, "/opt/musegadget/venv/bin/musegadget");
        assert!(m(&|c| c.sdk_command = Some("sh -c x".into())).is_err());
    }

    #[test]
    fn ask_sends_only_the_last_user_line_to_the_main_chat() {
        let (p, sent) = fake(r#"{"ok":true,"reply":"Hello!"}"#);
        let msgs = [Message::user("first"), Message::assistant("a", vec![]), Message::user("second")];
        let opts = ChatOpts::default();
        let (t, calls) = collect(&p, &ChatRequest { system: "", messages: &msgs, tools: &[], opts: &opts }).unwrap();
        assert_eq!((t.as_str(), calls.len()), ("Hello!", 0));
        let v: Value = serde_json::from_str(&sent.lock().unwrap()[0]).unwrap();
        assert_eq!(v, json!({"message": "second", "wait": 60}), "no session_id: the main chat");
        assert_eq!(p.label(), "Muse");
    }

    #[test]
    fn a_muse_entry_in_agent_json_builds_a_provider() {
        let c = crate::launcher::agent::config::AgentConfig::parse(
            r#"{"default":"muse","providers":{"muse":{"kind":"muse","muse":{"wait_secs":30,"session_id":"x1"}},"bad":{"kind":"muse","muse":{"wait_secs":0}}}}"#,
        )
        .unwrap();
        let p = crate::launcher::agent::provider::build(&c, "muse", None).unwrap();
        assert_eq!(p.label(), "Muse");
        assert!(crate::launcher::agent::provider::build(&c, "bad", None).is_err());
    }

    #[test]
    fn the_example_file_has_a_working_muse_entry() {
        let c = crate::launcher::agent::config::AgentConfig::parse(include_str!("../../../docs/agent.example.json")).unwrap();
        assert_eq!(c.providers["muse"].kind, "muse");
        assert!(crate::launcher::agent::provider::build(&c, "muse", None).is_ok());
    }

    #[test]
    fn partial_and_empty_answers() {
        let (p, _) = fake(r#"{"ok":true,"reply":"half","partial":true,"detail":"x"}"#);
        assert!(run(&p, "q").unwrap().starts_with("half"));
        let (p, _) = fake(r#"{"ok":true,"reply":null,"detail":"no answer within 60 seconds"}"#);
        assert!(run(&p, "q").unwrap_err().contains("no answer within 60 seconds"));
    }

    struct Seq(Mutex<Vec<io::Result<String>>>, Arc<Mutex<u32>>);
    impl Link for Seq {
        fn roundtrip(&self, _r: &str, _t: Duration) -> io::Result<String> {
            *self.1.lock().unwrap() += 1;
            self.0.lock().unwrap().remove(0)
        }
    }
    const NOT_SENT: &str = r#"{"ok":false,"error":"could not follow Muse's replies (HTTP 400); the message was not sent"}"#;

    #[test]
    fn a_not_sent_error_is_classified_and_retried_once() {
        assert!(matches!(parse_reply(NOT_SENT), Err(MuseError::NotSent(_))));
        let n = Arc::new(Mutex::new(0));
        let ok = r#"{"ok":true,"reply":"hi"}"#.to_string();
        let mk = |v: Vec<io::Result<String>>| {
            let mut p = MuseProvider::with_link("muse", settings(), Seq(Mutex::new(v), n.clone()));
            p.retry_delay = Duration::ZERO;
            p
        };
        // hiccup, then fine: one retry, the user sees the answer
        assert_eq!(mk(vec![Ok(NOT_SENT.into()), Ok(ok.clone())]).ask("x").unwrap().text, "hi");
        assert_eq!(*n.lock().unwrap(), 2);
        // two hiccups: a clear "nothing was sent" error after exactly one retry
        *n.lock().unwrap() = 0;
        let e = mk(vec![Ok(NOT_SENT.into()), Ok(NOT_SENT.into())]).ask("x").unwrap_err();
        assert!(matches!(e, MuseError::NotSent(_)));
        assert_eq!(*n.lock().unwrap(), 2);
        assert!(e.user_message_in(false).contains("Nothing was sent"));
        assert!(e.user_message_in(true).contains("何も送られていません"));
        // "maybe delivered" is never retried (it could double-send)
        *n.lock().unwrap() = 0;
        let u = r#"{"ok":false,"error":"late","sent":"unknown"}"#.to_string();
        assert!(matches!(mk(vec![Ok(u)]).ask("x"), Err(MuseError::Uncertain(_))));
        assert_eq!(*n.lock().unwrap(), 1);
    }

    #[test]
    fn test_connection_tells_the_states_apart_in_both_languages() {
        let f = |k: io::ErrorKind| diagnose_with(&Fake { sent: Default::default(), kind: Some(k), resp: String::new() });
        assert_eq!(f(io::ErrorKind::NotFound), Diagnosis::Failed(MuseError::SdkMissing));
        assert_eq!(f(io::ErrorKind::PermissionDenied), Diagnosis::Failed(MuseError::NoPermission));
        let ok = diagnose_with(&Fake { sent: Default::default(), kind: None, resp: r#"{"ok": false, "error": "expected {\"message\": \"...\"}"}"#.into() });
        assert_eq!(ok, Diagnosis::ServiceOk);
        let junk = diagnose_with(&Fake { sent: Default::default(), kind: None, resp: "nope".into() });
        assert!(matches!(junk, Diagnosis::Failed(MuseError::Protocol(_))));
        for ja in [false, true] {
            let mut all = vec![diagnosis_text(&ok, ja)];
            for e in [MuseError::SdkMissing, MuseError::NoPermission, MuseError::Offline, MuseError::NotSent("x".into())] {
                all.push(diagnosis_text(&Diagnosis::Failed(e), ja));
            }
            all.sort();
            all.dedup();
            assert_eq!(all.len(), 5, "every state has its own words");
        }
        assert!(diagnosis_text(&Diagnosis::Failed(MuseError::NoPermission), false).contains("group"));
    }

    #[test]
    fn error_states_have_plain_messages() {
        let e = |resp: &str| fake(resp).0.ask("q").unwrap_err();
        assert_eq!(e(r#"{"ok":false,"error":"not connected to the Muse"}"#), MuseError::Offline);
        assert_eq!(e(r#"{"ok":false,"error":"late","sent":"unknown"}"#), MuseError::Uncertain("late".into()));
        assert_eq!(e(r#"{"ok":false,"error":"boom"}"#), MuseError::Remote("boom".into()));
        assert!(matches!(e("garbage"), MuseError::Protocol(_)));
        for (k, want) in [
            (io::ErrorKind::NotFound, MuseError::SdkMissing),
            (io::ErrorKind::ConnectionRefused, MuseError::SdkMissing),
            (io::ErrorKind::PermissionDenied, MuseError::NoPermission),
            (io::ErrorKind::TimedOut, MuseError::Timeout),
        ] {
            assert_eq!(failing(k).ask("q"), Err(want.clone()));
            assert_eq!(run(&failing(k), "q").unwrap_err(), want.user_message());
        }
        assert!(MuseError::SdkMissing.user_message_in(false).contains("docs/MUSE.md"));
        assert!(fake("{}").0.ask("  ").is_err());
        // no user line at all
        let opts = ChatOpts::default();
        assert!(collect(&fake("{}").0, &ChatRequest { system: "", messages: &[], tools: &[], opts: &opts }).is_err());
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hl-muse-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn unix_link_talks_to_a_fake_sdk_socket() {
        let dir = tmp("sdk");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("s.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&c).read_line(&mut line).unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            writeln!(c, "{}", json!({"ok": true, "reply": format!("echo {}", v["message"].as_str().unwrap())})).unwrap();
        });
        let mut s = settings();
        s.socket = sock;
        let p = MuseProvider::new("muse", s);
        assert_eq!(p.ask("yo").unwrap().text, "echo yo");
        t.join().unwrap();
        std::fs::remove_file(&p.cfg.socket).unwrap();
        assert_eq!(p.ask("x"), Err(MuseError::SdkMissing)); // the socket is gone
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commands_parse_only_known_harbour_tools() {
        let c = parse_command(r#"{"command":"harbour.launch_app","params":{"name":"Files"}}"#, "i").unwrap();
        assert_eq!((c.name.as_str(), c.args["name"].as_str()), ("launch_app", Some("Files")));
        assert!(parse_command(r#"{"command":"harbour.list_apps"}"#, "i").is_ok());
        for bad in [r#"{"command":"system.run"}"#, r#"{"command":"harbour.rm"}"#, r#"{"command":"harbour.list_apps","params":3}"#, "zzz", "{}"] {
            assert!(parse_command(bad, "i").is_err(), "{bad}");
        }
    }

    #[test]
    fn command_server_hands_calls_to_the_app_and_returns_its_answer() {
        let dir = tmp("cmd");
        let sock = dir.join("run").join("muse-cmd.sock");
        let rx = spawn_command_server(&sock, Duration::from_secs(5)).unwrap();
        assert_eq!(std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(sock.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        let s2 = sock.clone();
        let cl = std::thread::spawn(move || UnixLink(s2).roundtrip(r#"{"command":"harbour.launch_app","params":{"name":"Files"}}"#, Duration::from_secs(5)).unwrap());
        let rc = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(rc.call.name, "launch_app");
        assert!(rc.deadline > Instant::now());
        rc.reply.send(Ok("Launched Files.".into())).unwrap();
        let v: Value = serde_json::from_str(&cl.join().unwrap()).unwrap();
        assert_eq!(v, json!({"ok": true, "result": "Launched Files."}));
        // unknown command is refused without reaching the app
        let v: Value = serde_json::from_str(&UnixLink(sock.clone()).roundtrip(r#"{"command":"system.run"}"#, Duration::from_secs(5)).unwrap()).unwrap();
        assert_eq!(v["ok"], false);
        assert!(rx.try_recv().is_err());
        // nobody answers: the connection ends with an error, not a hang
        let s3 = sock.clone();
        let cl = std::thread::spawn(move || UnixLink(s3).roundtrip(r#"{"command":"harbour.list_apps"}"#, Duration::from_secs(8)).unwrap());
        let _held = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(_held.reply); // app dropped the call
        let v: Value = serde_json::from_str(&cl.join().unwrap()).unwrap();
        assert_eq!(v["ok"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn command_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/muse_bridge/commands.d")
    }

    /// The drop-in files are generated from `tools::specs()`. After changing a tool:
    /// `HARBOUR_WRITE_MUSE_COMMANDS=1 cargo test --lib muse::tests::dropin` rewrites them.
    #[test]
    fn dropin_files_match_the_tool_list_and_the_sdk_rules() {
        let specs = command_specs();
        assert_eq!(specs.len(), tools::specs().len());
        if std::env::var("HARBOUR_WRITE_MUSE_COMMANDS").is_ok() {
            let _ = std::fs::remove_dir_all(command_dir());
            std::fs::create_dir_all(command_dir()).unwrap();
            for (name, spec) in &specs {
                std::fs::write(command_dir().join(format!("{name}.json")), serde_json::to_string_pretty(spec).unwrap() + "\n").unwrap();
            }
        }
        let on_disk = std::fs::read_dir(command_dir()).unwrap().count();
        assert_eq!(on_disk, specs.len(), "stale or missing files in tools/muse_bridge/commands.d");
        for (name, spec) in &specs {
            let text = std::fs::read_to_string(command_dir().join(format!("{name}.json"))).unwrap_or_default();
            assert_eq!(&serde_json::from_str::<Value>(&text).unwrap_or(Value::Null), spec, "{name}: run with HARBOUR_WRITE_MUSE_COMMANDS=1");
            // SDK rules: lowercase dotted name <= 64, not reserved, absolute exec, typed params with descriptions.
            assert!(name.len() <= 64 && name.chars().all(|c| c.is_ascii_lowercase() || c == '.' || c == '_'));
            assert!(!["system.", "file.", "device.", "link."].iter().any(|p| name.starts_with(p)));
            assert!(spec["exec"][0].as_str().unwrap().starts_with('/'));
            for k in ["required", "optional"] {
                for (_, p) in spec.get(k).and_then(Value::as_object).into_iter().flatten() {
                    assert_eq!(p.as_object().unwrap().len(), 2);
                    assert!(p["type"].is_string() && p["description"].is_string());
                }
            }
        }
    }
}
