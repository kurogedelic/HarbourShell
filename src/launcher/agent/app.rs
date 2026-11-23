//! The Agent chat screen and the conversation state machine.
//!
//! The chat is an overlay on top of the live screen: the app behind it keeps running, and the
//! tools (`launch_app`, `press_keys`, `current_screen`) act on that app. Esc closes the chat.
//!
//! Layout (under the 16 px menu strip, 9 rows of 16 px): 7 rows of transcript, a status row and
//! the input row with the white Send button. The scroll bar is always drawn.
//!
//! A provider call runs on a worker thread and answers through a channel; the tool loop runs on
//! the UI thread (tools need the screen) and starts the next round, at most [`tools::MAX_ROUNDS`]
//! per user message.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use sdl2::keyboard::{Keycode, Mod};
use sdl2::rect::Rect;
use sdl2::render::Canvas;
use sdl2::video::Window;

use super::config::{self, AgentConfig};
use super::history::{self, Chat, SEND_WINDOW};
use super::input::InputSource;
use super::muse::{self, RemoteCall};
use super::provider::{self, Provider};
use super::tools::{self, Exec, HostReq, ToolEnv, MAX_ROUNDS};
use super::voice::{FakeVoice, SystemVoice, VoiceBackend, VoiceController, VoiceEvent, VoiceState};
use super::types::{truncate_for_send, ChatOpts, ChatRequest, Message, ProviderEvent, Role, ToolCall};
use crate::assets::AssetStore;
use crate::grid;
use crate::launcher::app::{AppAction, Ctx, LauncherApp};
use crate::launcher::dialogs::{ConfirmAlert, ConfirmChoice, ListPicker, PickResult};
use crate::launcher::lineedit::LineEdit;
use crate::launcher::widgets::{self, contains, draw_button, BtnState, MenuItem, PointerEvent, ScrollBar};
use crate::system::config::Config;
use crate::system::Backend;
use crate::ui;

const ROWS: usize = 7;
const TOP: i32 = grid::BAR_H;
/// What is left of the panel's height (down to y 168, the last row the screens use) after the
/// transcript rows, the status row and the input row: 9 px. The transcript plate takes it, its
/// rows starting [`TEXT_Y`] (half of it lower), so the input row ends on y 168.
const PAD: i32 = grid::APP_FRAME_BOTTOM + 1 - TOP - (ROWS as i32 + 2) * grid::ROW_H;
const TEXT_Y: i32 = TOP + PAD / 2;
const AREA_H: i32 = ROWS as i32 * grid::ROW_H + PAD;
const STATUS_Y: i32 = TOP + AREA_H;
const INPUT_Y: i32 = STATUS_Y + grid::ROW_H;
/// The Agent is a panel on the right that slides over the current screen.
pub const PANEL_W: i32 = 212;
const PANEL_X: i32 = grid::SCREEN_W - PANEL_W;
const TEXT_X: i32 = PANEL_X + grid::MARGIN;
const TEXT_W: i32 = grid::SCROLL_X - grid::MARGIN - TEXT_X;
const SEND_W: i32 = 48;
const FIELD_W: i32 = grid::SCROLL_X + grid::SCROLL_W - grid::MARGIN - SEND_W - grid::GUTTER - TEXT_X;
const MAX_INPUT: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    User,
    Assistant,
    Tool,
    Notice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
    /// First wrapped line of its message (the marker is drawn there).
    pub first: bool,
}

/// Width taken by the marker column (`>` for you, a gutter bar for the agent).
const MARK_W: i32 = 8;
const WRAP_W: i32 = TEXT_W - MARK_W;

/// Greedy word wrap by measured width: at spaces when there are some, anywhere otherwise
/// (Japanese). Newlines start a new line. `measure` is the font's `measure_text`.
pub fn wrap_text(text: &str, width: i32, measure: &dyn Fn(&str) -> i32) -> Vec<String> {
    let mut out = vec![];
    for para in text.split('\n') {
        let mut line = String::new();
        for ch in para.chars() {
            let mut cand = line.clone();
            cand.push(ch);
            if measure(&cand) <= width || line.is_empty() {
                line = cand;
                continue;
            }
            // Break at the last space if the line has one and the new char is not a space.
            if ch != ' ' {
                if let Some(sp) = line.rfind(' ') {
                    let rest = line[sp + 1..].to_string();
                    line.truncate(sp);
                    out.push(std::mem::take(&mut line));
                    line = rest;
                    line.push(ch);
                    continue;
                }
            }
            out.push(std::mem::take(&mut line));
            if ch != ' ' {
                line.push(ch);
            }
        }
        out.push(line);
    }
    out
}

enum Phase {
    Idle,
    Thinking,
    /// A side-effect tool waits for the user's yes.
    Confirm(ToolCall, ConfirmAlert),
    /// The main loop is running a tool.
    HostWait(ToolCall),
}

/// A command Muse sent (`harbour.<tool>`): it runs next to the chat, not inside the model loop,
/// because Muse calls it while the provider call is still waiting for Muse's answer. Same
/// `tools::execute`, same confirmation card.
enum Remote {
    Confirm(RemoteCall, ConfirmAlert),
    /// The main loop is running the tool.
    Host(RemoteCall),
}

pub struct AgentApp {
    cfg: Arc<Mutex<Config>>,
    agent_cfg: AgentConfig,
    config_warning: Option<String>,
    config_exists: bool,
    /// HARBOUR_MOCK: the mock provider counts as set up (headless runs, tests).
    mock_forced: bool,
    /// The empty state's button was used: the host closes the panel and opens Settings > Agent.
    open_settings: bool,
    setup_hover: bool,
    provider_name: String,
    provider: Result<Arc<dyn Provider>, String>,
    model_override: Option<String>,
    messages: Vec<Message>,
    chat_id: String,
    input: LineEdit,
    scroll: usize,
    stick: bool,
    phase: Phase,
    rx: Option<Receiver<ProviderEvent>>,
    cur_text: String,
    cur_calls: Vec<ToolCall>,
    cur_error: Option<String>,
    queue: VecDeque<ToolCall>,
    rounds: usize,
    status: Option<String>,
    auto_run: bool,
    host_req: Option<HostReq>,
    remote_rx: Option<Receiver<RemoteCall>>,
    remote: Option<Remote>,
    remote_req: Option<HostReq>,
    reply_remote: bool,
    restyle: bool,
    picker: Option<ListPicker>,
    dir: PathBuf,
    home: PathBuf,
    sources: Vec<Box<dyn InputSource>>,
    lines: Vec<Line>,
    lines_rev: u64,
    rev: u64,
    lines_w: i32,
    sb_drag: bool,
    send_hover: bool,
    dots: usize,
    blink: usize,
    /// Push-to-talk (created on first use; a test can inject a fake backend first).
    voice: Option<VoiceController>,
    voice_backend: Option<Arc<dyn VoiceBackend>>,
    started: Instant,
}

pub fn make_with(cfg: Arc<Mutex<Config>>, mock: bool) -> AgentApp {
    AgentApp::new(cfg, mock, config::config_path(), config::data_dir())
}

impl AgentApp {
    pub fn new(cfg: Arc<Mutex<Config>>, mock: bool, config_file: PathBuf, dir: PathBuf) -> Self {
        let loaded = config::load(&config_file, mock);
        let st = history::load_state(&dir);
        let name = st
            .provider
            .clone()
            .filter(|p| loaded.config.providers.contains_key(p) && !mock)
            .or_else(|| loaded.config.default_name())
            .unwrap_or_else(|| "mock".into());
        let mut messages = vec![];
        let mut chat_id = history::new_id(chrono::Local::now());
        if let Some(mut c) = history::load_latest(&dir) {
            history::repair(&mut c.messages);
            messages = c.messages;
            chat_id = c.id;
        }
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
        let mut app = Self {
            cfg,
            agent_cfg: loaded.config,
            config_warning: loaded.warning,
            config_exists: loaded.exists,
            mock_forced: mock,
            open_settings: false,
            setup_hover: false,
            provider_name: name.clone(),
            provider: Err(String::new()),
            model_override: if mock { None } else { st.model.clone() },
            messages,
            chat_id,
            input: LineEdit::default(),
            scroll: 0,
            stick: true,
            phase: Phase::Idle,
            rx: None,
            cur_text: String::new(),
            cur_calls: vec![],
            cur_error: None,
            queue: VecDeque::new(),
            rounds: 0,
            status: None,
            auto_run: st.auto_run,
            host_req: None,
            remote_rx: None,
            remote: None,
            remote_req: None,
            reply_remote: false,
            restyle: false,
            picker: None,
            dir,
            home,
            sources: vec![],
            lines: vec![],
            lines_rev: u64::MAX,
            rev: 0,
            lines_w: 0,
            sb_drag: false,
            send_hover: false,
            dots: 0,
            blink: 0,
            voice: None,
            voice_backend: None,
            started: Instant::now(),
        };
        app.provider = provider::build(&app.agent_cfg, &name, app.model_override.as_deref());
        if !mock {
            match muse::start_for(&app.agent_cfg) {
                Some(Ok(rx)) => app.remote_rx = Some(rx),
                Some(Err(e)) => {
                    app.config_warning.get_or_insert(e);
                }
                None => {}
            }
        }
        app
    }

    // ------------------------------------------------------------ public API for the host

    /// A tool the main loop has to run; answer with [`host_reply`](Self::host_reply).
    pub fn take_host_req(&mut self) -> Option<HostReq> {
        if let Some(r) = self.host_req.take() {
            self.reply_remote = false;
            return Some(r);
        }
        let r = self.remote_req.take()?;
        self.reply_remote = true;
        Some(r)
    }

    pub fn host_reply(&mut self, r: Result<String, String>, sys: &dyn Backend) {
        if std::mem::take(&mut self.reply_remote) {
            self.finish_remote(r);
            return;
        }
        if let Phase::HostWait(call) = std::mem::replace(&mut self.phase, Phase::Idle) {
            self.finish_call(&call, r, sys);
        }
    }

    /// The one short word of the status row (never a provider or model name) and whether it is an
    /// alert (drawn on a plate).
    pub fn state_word(&self) -> (&'static str, bool) {
        match self.voice.as_ref().map(|v| v.state()) {
            Some(VoiceState::Recording) => return ("REC", true),
            Some(VoiceState::Sending) => return ("SENDING", false),
            Some(VoiceState::Playing) => return ("PLAYING", false),
            _ => {}
        }
        match &self.remote {
            Some(Remote::Confirm(..)) => return ("CONFIRM", false),
            Some(Remote::Host(..)) => return ("RUNNING", false),
            None => {}
        }
        match &self.phase {
            Phase::Thinking => ("THINKING", false),
            Phase::HostWait(_) => ("RUNNING", false),
            Phase::Confirm(..) => ("CONFIRM", false),
            Phase::Idle => {
                if self.status.as_deref().map_or(false, |s| s.starts_with('!')) || self.provider.is_err() {
                    ("ERROR", true)
                } else if self.config_warning.is_some() {
                    ("WARN", true)
                } else {
                    ("READY", false)
                }
            }
        }
    }

    // ------------------------------------------------------------ push-to-talk voice

    /// The Muse provider's voice settings, or why voice cannot be used (plain words).
    fn muse_voice_settings(&self) -> Result<muse::MuseSettings, String> {
        let (name, p) = self
            .agent_cfg
            .providers
            .get(&self.provider_name)
            .filter(|p| p.kind == "muse")
            .map(|p| (self.provider_name.clone(), p))
            .or_else(|| self.agent_cfg.providers.iter().find(|(_, p)| p.kind == "muse").map(|(n, p)| (n.clone(), p)))
            .ok_or_else(|| "Voice needs a Muse provider (docs/MUSE.md)".to_string())?;
        let s = muse::MuseSettings::from_cfg(p).map_err(|e| format!("{name}: {e}"))?;
        if !s.voice {
            return Err("Voice is off (muse.voice: false)".into());
        }
        if s.vm_id.is_none() {
            return Err("Voice needs muse.vm_id (docs/MUSE.md)".into());
        }
        Ok(s)
    }

    /// Whether holding Sym can work (used for the hint line).
    pub fn voice_available(&self) -> bool {
        self.voice_backend.is_some() || self.mock_forced || self.muse_voice_settings().is_ok()
    }

    /// Inject a backend (tests; never the real one).
    pub fn set_voice_backend(&mut self, b: Arc<dyn VoiceBackend>) {
        self.voice_backend = Some(b);
        self.voice = None;
    }

    fn voice_controller(&mut self) -> Result<&mut VoiceController, String> {
        if self.voice.is_none() {
            let backend: Arc<dyn VoiceBackend> = if let Some(b) = self.voice_backend.clone() {
                b
            } else if self.mock_forced {
                Arc::new(FakeVoice::demo())
            } else {
                let s = self.muse_voice_settings()?;
                Arc::new(SystemVoice { sdk_command: s.sdk_command, vm_id: s.vm_id.unwrap_or_default() })
            };
            self.voice = Some(VoiceController::new(backend, self.dir.join("voice")));
        }
        Ok(self.voice.as_mut().unwrap())
    }

    fn voice_events(&mut self, events: Vec<VoiceEvent>) {
        for e in events {
            match e {
                VoiceEvent::Info(t) => self.notice(&t),
                VoiceEvent::Error(t) => {
                    self.status = Some(format!("! {t}"));
                    self.notice(&format!("Voice: {t}"));
                }
            }
        }
    }

    /// Push-to-talk from the host: `true` = the key has been held long enough, `false` = released.
    pub fn voice_input(&mut self, down: bool) {
        if self.unconfigured() {
            return;
        }
        if down {
            if self.is_busy() || self.remote.is_some() {
                self.status = Some("! wait for the current answer first".into());
                return;
            }
            self.status = None;
            let r = self.voice_controller().and_then(|v| v.press());
            if let Err(e) = r {
                if e != "busy" {
                    self.voice_events(vec![VoiceEvent::Error(e)]);
                }
            }
        } else if let Some(v) = self.voice.as_mut() {
            let ev = v.release();
            self.voice_events(ev);
        }
    }

    /// The panel was closed: a recording in progress is thrown away.
    pub fn on_hidden(&mut self) {
        if let Some(v) = self.voice.as_mut() {
            v.cancel_recording();
        }
    }

    /// Poll the voice worker; true when something new was said to the chat.
    pub fn poll_voice(&mut self) -> bool {
        let Some(v) = self.voice.as_mut() else { return false };
        let ev = v.tick();
        let any = !ev.is_empty();
        self.voice_events(ev);
        any
    }

    pub fn voice_state(&self) -> VoiceState {
        self.voice.as_ref().map_or(VoiceState::Idle, |v| v.state())
    }

    /// A short informational note shown after the state word (e.g. "auto-run tools: on").
    pub fn state_note(&self) -> Option<String> {
        if !matches!(self.phase, Phase::Idle) {
            return None;
        }
        self.status.clone().filter(|s| !s.starts_with('!'))
    }

    /// No usable provider: the config is missing or broken, or only the built-in mock answers
    /// (unless HARBOUR_MOCK asked for it). The panel then shows a short empty state instead of the chat.
    pub fn unconfigured(&self) -> bool {
        self.provider.is_err() || (self.provider_name == "mock" && !self.mock_forced)
    }

    /// True once after the empty state's "Open Settings" button was used.
    pub fn take_open_settings(&mut self) -> bool {
        std::mem::take(&mut self.open_settings)
    }

    /// Keys in the empty state: Enter activates the button, Esc closes the panel.
    fn setup_key(&mut self, key: Keycode) -> AppAction {
        match key {
            Keycode::Escape => return AppAction::Quit,
            Keycode::Return | Keycode::KpEnter => self.open_settings = true,
            _ => {}
        }
        AppAction::None
    }

    fn setup_pointer(&mut self, ev: PointerEvent) {
        match ev {
            PointerEvent::Move { x, y } => self.setup_hover = contains(Self::setup_button(), x, y),
            PointerEvent::Down { x, y, button: 0, .. } if contains(Self::setup_button(), x, y) => self.open_settings = true,
            _ => {}
        }
    }

    fn setup_button() -> Rect {
        Rect::new(PANEL_X + (PANEL_W - 124) / 2, TOP + 64, 124, grid::BTN_H as u32)
    }

    /// A setting changed that needs the theme / fonts reloaded.
    pub fn take_restyle(&mut self) -> bool {
        std::mem::take(&mut self.restyle)
    }

    pub fn add_source(&mut self, s: Box<dyn InputSource>) {
        self.sources.push(s);
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn is_busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    pub fn auto_run(&self) -> bool {
        self.auto_run
    }

    pub fn status_text(&self) -> String {
        if let Some(r) = &self.remote {
            let c = match r {
                Remote::Confirm(c, _) | Remote::Host(c) => c,
            };
            return format!("Muse: {}", tools::describe_call(&c.call));
        }
        match &self.phase {
            Phase::Thinking => format!("thinking{}", ".".repeat(self.dots % 3 + 1)),
            Phase::HostWait(c) => format!("running: {}", tools::describe_call(c)),
            Phase::Confirm(c, _) => format!("run: {}?", tools::describe_call(c)),
            Phase::Idle => {
                if let Some(s) = &self.status {
                    s.clone()
                } else if let Some(w) = &self.config_warning {
                    format!("! {w}")
                } else {
                    match &self.provider {
                        Ok(p) => format!("provider: {}{}", p.label(), if self.auto_run { "  [auto-run]" } else { "" }),
                        Err(e) => format!("! {e}"),
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ conversation

    fn bump(&mut self) {
        self.rev += 1;
    }

    fn save(&self) {
        let chat = Chat { id: self.chat_id.clone(), provider: self.provider_name.clone(), messages: self.messages.clone() };
        if !self.messages.is_empty() {
            let _ = history::save(&self.dir, &chat);
        }
    }

    fn save_state(&self) {
        // Read first: Settings owns `enabled`, and the last status is kept.
        let mut st = history::load_state(&self.dir);
        st.auto_run = self.auto_run;
        st.provider = Some(self.provider_name.clone());
        st.model = self.model_override.clone();
        history::save_state(&self.dir, &st);
    }

    /// The one-line result of the last provider call, shown in Settings > Agent.
    fn record_status(&self, text: &str) {
        let mut st = history::load_state(&self.dir);
        st.last_status = Some(text.chars().take(60).collect());
        history::save_state(&self.dir, &st);
    }

    fn notice(&mut self, text: &str) {
        self.messages.push(Message::notice(text));
        self.bump();
        self.stick = true;
    }

    /// The user's line (typed, or from an [`InputSource`]).
    pub fn submit(&mut self, text: &str, _sys: &dyn Backend) {
        let text = text.trim();
        if text.is_empty() || self.is_busy() {
            return;
        }
        self.status = None;
        if let Err(e) = &self.provider {
            let msg = if e.is_empty() { "no provider is set up".to_string() } else { e.clone() };
            self.status = Some(format!("! {msg}"));
            self.notice(&format!("Cannot ask: {msg}. See docs/agent.example.json."));
            return;
        }
        self.messages.push(Message::user(text));
        self.bump();
        self.stick = true;
        self.rounds = 0;
        self.save();
        self.start_round();
    }

    fn start_round(&mut self) {
        if self.rounds >= MAX_ROUNDS {
            self.notice(&format!("Stopped after {MAX_ROUNDS} tool rounds."));
            self.phase = Phase::Idle;
            self.save();
            return;
        }
        let Ok(p) = &self.provider else {
            self.phase = Phase::Idle;
            return;
        };
        self.rounds += 1;
        let p = p.clone();
        let msgs = truncate_for_send(&self.messages, SEND_WINDOW);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let system = tools::system_prompt(grid::SCREEN_W, grid::SCREEN_H);
            let specs = tools::specs();
            let opts = ChatOpts::default();
            let req = ChatRequest { system: &system, messages: &msgs, tools: &specs, opts: &opts };
            p.chat(&req, &mut |e| {
                let _ = tx.send(e);
            });
        });
        self.rx = Some(rx);
        self.cur_text.clear();
        self.cur_calls.clear();
        self.cur_error = None;
        self.phase = Phase::Thinking;
    }

    fn poll_worker(&mut self, sys: &dyn Backend) -> bool {
        let Some(rx) = &self.rx else { return false };
        let mut done = false;
        let mut changed = false;
        loop {
            match rx.try_recv() {
                Ok(ProviderEvent::TextDelta(t)) => {
                    self.cur_text.push_str(&t);
                    changed = true;
                }
                Ok(ProviderEvent::ToolCall(c)) => self.cur_calls.push(c),
                Ok(ProviderEvent::Done) => {
                    done = true;
                    break;
                }
                Ok(ProviderEvent::Error(e)) => {
                    self.cur_error = Some(e);
                    done = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.cur_error.is_none() && self.cur_text.is_empty() && self.cur_calls.is_empty() {
                        self.cur_error = Some("the request ended without an answer".into());
                    }
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.rx = None;
            self.finish_round(sys);
            return true;
        }
        changed
    }

    fn finish_round(&mut self, sys: &dyn Backend) {
        if let Some(e) = self.cur_error.take() {
            self.status = Some(format!("! {e}"));
            self.record_status(&format!("Error: {}", e.lines().next().unwrap_or("")));
            self.notice(&format!("Error: {e}"));
            self.phase = Phase::Idle;
            self.save();
            return;
        }
        let text = std::mem::take(&mut self.cur_text);
        let calls = std::mem::take(&mut self.cur_calls);
        if text.trim().is_empty() && calls.is_empty() {
            self.notice("(the model sent an empty answer)");
            self.phase = Phase::Idle;
            return;
        }
        if self.provider_name != "mock" {
            self.record_status("OK");
        }
        self.messages.push(Message::assistant(text.trim(), calls.clone()));
        self.bump();
        self.stick = true;
        if calls.is_empty() {
            self.phase = Phase::Idle;
            self.save();
            return;
        }
        self.queue = calls.into();
        self.next_call(sys);
    }

    fn next_call(&mut self, sys: &dyn Backend) {
        let Some(call) = self.queue.pop_front() else {
            self.start_round();
            return;
        };
        if tools::needs_confirm(&call.name) && !self.auto_run {
            let lines = vec!["Run this tool?".to_string(), tools::describe_call(&call)];
            self.phase = Phase::Confirm(call, ConfirmAlert::new(lines, "Run", true));
            return;
        }
        self.run_call(call, sys);
    }

    fn run_call(&mut self, call: ToolCall, sys: &dyn Backend) {
        let env = ToolEnv { sys, cfg: &self.cfg, home: &self.home };
        let mut restyle = false;
        let out = tools::execute(&call, &env, &mut restyle);
        self.restyle |= restyle;
        match out {
            Exec::Done(r) => self.finish_call(&call, r, sys),
            Exec::Host(req) => {
                self.host_req = Some(req);
                self.phase = Phase::HostWait(call);
            }
        }
    }

    fn finish_call(&mut self, call: &ToolCall, r: Result<String, String>, sys: &dyn Backend) {
        let (text, err) = match r {
            Ok(t) => (tools::clip(t, tools::MAX_RESULT_CHARS), false),
            Err(e) => (e, true),
        };
        self.messages.push(Message::tool(&call.id, &text, err));
        self.bump();
        self.stick = true;
        self.next_call(sys);
    }

    fn decline(&mut self, call: &ToolCall, sys: &dyn Backend) {
        self.messages.push(Message::tool(&call.id, "The user declined to run this tool.", true));
        self.notice(&format!("declined: {}", tools::describe_call(call)));
        // The rest of this batch is answered the same way, then the model gets to react.
        self.next_call(sys);
    }

    // ------------------------------------------------------------ commands from Muse

    fn poll_remote(&mut self, sys: &dyn Backend) -> bool {
        if self.remote.is_some() || matches!(self.phase, Phase::Confirm(..) | Phase::HostWait(..)) {
            return false;
        }
        let Some(rx) = &self.remote_rx else { return false };
        while let Ok(rc) = rx.try_recv() {
            if Instant::now() >= rc.deadline {
                continue; // the socket side gave up: never run a stale command
            }
            if tools::needs_confirm(&rc.call.name) && !self.auto_run {
                let lines = vec!["Muse wants to:".to_string(), tools::describe_call(&rc.call)];
                self.remote = Some(Remote::Confirm(rc, ConfirmAlert::new(lines, "Run", true)));
            } else {
                self.run_remote(rc, sys);
            }
            return true;
        }
        false
    }

    fn run_remote(&mut self, rc: RemoteCall, sys: &dyn Backend) {
        let env = ToolEnv { sys, cfg: &self.cfg, home: &self.home };
        let mut restyle = false;
        let out = tools::execute(&rc.call, &env, &mut restyle);
        self.restyle |= restyle;
        self.notice(&format!("Muse: {}", tools::describe_call(&rc.call)));
        match out {
            Exec::Done(r) => {
                let _ = rc.reply.send(r.map(|t| tools::clip(t, tools::MAX_RESULT_CHARS)));
            }
            Exec::Host(req) => {
                self.remote_req = Some(req);
                self.remote = Some(Remote::Host(rc));
            }
        }
    }

    fn finish_remote(&mut self, r: Result<String, String>) {
        if let Some(Remote::Host(rc)) = self.remote.take() {
            let _ = rc.reply.send(r.map(|t| tools::clip(t, tools::MAX_RESULT_CHARS)));
        }
    }

    fn apply_remote_confirm(&mut self, choice: ConfirmChoice, sys: &dyn Backend) {
        if let Some(Remote::Confirm(rc, _)) = self.remote.take() {
            match choice {
                ConfirmChoice::Ok => self.run_remote(rc, sys),
                ConfirmChoice::Cancel => {
                    self.notice(&format!("declined (Muse): {}", tools::describe_call(&rc.call)));
                    let _ = rc.reply.send(Err("The user declined to run this tool.".into()));
                }
            }
        }
    }

    fn new_chat(&mut self) {
        self.rx = None;
        self.queue.clear();
        self.phase = Phase::Idle;
        self.messages.clear();
        self.chat_id = history::new_id(chrono::Local::now());
        self.status = None;
        self.scroll = 0;
        self.stick = true;
        self.bump();
    }

    fn switch_provider(&mut self, name: &str) {
        self.provider_name = name.to_string();
        self.provider = provider::build(&self.agent_cfg, name, self.model_override.as_deref());
        self.status = None;
        self.save_state();
    }

    // ------------------------------------------------------------ transcript lines

    fn hint_lines(&self) -> Vec<Line> {
        let mut v = vec![
            "Ask me to open an app, change a setting,".to_string(),
            "or tell you what is on the screen.".to_string(),
            "Esc closes. Alt opens the menu.".to_string(),
        ];
        if self.voice_available() {
            v.push("Hold Sym to talk.".into());
        }
        if !self.config_exists {
            v.push("No agent.yaml yet: answers are mocked.".into());
        }
        v.into_iter().map(|text| Line { kind: LineKind::Notice, text, first: true }).collect()
    }

    fn ensure_lines(&mut self, measure: &dyn Fn(&str) -> i32) {
        if self.lines_rev == self.rev && self.lines_w == WRAP_W {
            return;
        }
        let mut out: Vec<Line> = vec![];
        let push = |kind: LineKind, text: &str, out: &mut Vec<Line>| {
            for (i, l) in wrap_text(text, WRAP_W, measure).into_iter().enumerate() {
                out.push(Line { kind, text: l, first: i == 0 });
            }
        };
        for m in &self.messages {
            match m.role {
                Role::User => push(LineKind::User, &m.text, &mut out),
                Role::Assistant => {
                    if !m.text.is_empty() {
                        push(LineKind::Assistant, &m.text, &mut out);
                    }
                    for c in &m.tool_calls {
                        push(LineKind::Tool, &format!("ran: {}", tools::describe_call(c)), &mut out);
                    }
                }
                Role::Tool => {
                    if m.is_error {
                        let t: String = m.text.chars().take(100).collect();
                        push(LineKind::Tool, &format!("  ! {t}"), &mut out);
                    }
                }
                Role::Notice => push(LineKind::Notice, &m.text, &mut out),
            }
        }
        if self.messages.is_empty() {
            out.clear(); // the logo and the hints are drawn by `draw_empty`
        }
        self.lines = out;
        self.lines_rev = self.rev;
        self.lines_w = WRAP_W;
        self.clamp_scroll();
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(ROWS)
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        if self.stick || self.scroll > max {
            self.scroll = max;
        }
    }

    fn scroll_to(&mut self, v: isize) {
        let max = self.max_scroll() as isize;
        self.scroll = v.clamp(0, max) as usize;
        self.stick = self.scroll as isize == max;
    }

    fn scrollbar(&self) -> ScrollBar {
        ScrollBar::new(Rect::new(grid::SCROLL_X, TOP, grid::SCROLL_W as u32, AREA_H as u32), self.lines.len(), ROWS, self.scroll)
    }

    fn send_rect() -> Rect {
        Rect::new(grid::SCROLL_X + grid::SCROLL_W - grid::MARGIN - SEND_W, INPUT_Y, SEND_W as u32, grid::BTN_H_SM as u32)
    }

    fn try_send(&mut self, sys: &dyn Backend) {
        let t = self.input.text.clone();
        if t.trim().is_empty() || self.is_busy() {
            return;
        }
        self.input.clear();
        self.submit(&t, sys);
    }

    fn apply_confirm(&mut self, choice: ConfirmChoice, sys: &dyn Backend) {
        if let Phase::Confirm(call, _) = std::mem::replace(&mut self.phase, Phase::Idle) {
            match choice {
                ConfirmChoice::Ok => self.run_call(call, sys),
                ConfirmChoice::Cancel => self.decline(&call, sys),
            }
        }
    }

    fn apply_pick(&mut self, r: PickResult) {
        if let Some(p) = self.picker.take() {
            if let PickResult::Choose(i) = r {
                let names = self.agent_cfg.names();
                if let Some(n) = names.get(i) {
                    let n = n.clone();
                    self.switch_provider(&n);
                }
            }
            drop(p);
        }
    }
}

/// Five-by-five pixel letters of the logo (`#` = ink).
const LOGO: [[&str; 5]; 5] = [
    [".###.", "#...#", "#####", "#...#", "#...#"], // A
    [".####", "#....", "#..##", "#...#", ".####"], // G
    ["#####", "#....", "####.", "#....", "#####"], // E
    ["#...#", "##..#", "#.#.#", "#..##", "#...#"], // N
    ["#####", "..#..", "..#..", "..#..", "..#.."], // T
];

/// A glitchy copy of `word` in `buf` (stack only) and the index of the letter the scan is on
/// (`usize::MAX` = none this frame). Deterministic in `frame`.
fn shimmer<'a>(buf: &'a mut [u8; 12], word: &str, frame: usize) -> (&'a str, usize) {
    let b = word.as_bytes();
    let n = b.len().min(12);
    buf[..n].copy_from_slice(&b[..n]);
    if n == 0 {
        return ("", usize::MAX);
    }
    if frame % 9 < 2 {
        let step = frame / 9;
        buf[(step * 5 + 1) % n] = b"#%&*+=?0123"[(step * 7 + frame % 9) % 11];
    }
    let hl = frame % (n + 5);
    (std::str::from_utf8(&buf[..n]).unwrap_or(""), if hl < n { hl } else { usize::MAX })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Activity {
    Wave,
    Meter,
    Bars,
}

/// One period of the wave, amplitude 5.
const SINE: [i8; 32] = [0, 1, 2, 3, 4, 4, 5, 5, 5, 5, 4, 4, 3, 2, 1, 0, 0, -1, -2, -3, -4, -4, -5, -5, -5, -5, -4, -4, -3, -2, -1, 0];

/// A small animated strip in a `w` x `h` box: a scrolling wave (thinking / sending), a segmented level
/// meter (recording) or a bouncing equalizer (playing). 1 px rectangles only, no allocation.
fn draw_activity(c: &mut Canvas<Window>, kind: Activity, x: i32, y: i32, w: i32, h: i32, frame: usize, level: f32, ink: sdl2::pixels::Color, dim: sdl2::pixels::Color) {
    let mid = y + h / 2;
    match kind {
        Activity::Wave => {
            for col in 0..w {
                if col % 2 == 0 {
                    ui::fill_rect(c, Rect::new(x + col, mid, 1, 1), dim);
                }
                // The wave swells and fades as it scrolls: a radar-like pulse.
                let i = (col as usize * 2 + frame * 3) % 32;
                let env = 2 + ((col as usize + frame * 2) % 24) as i32 / 6; // 2..=5
                let dy = SINE[i] as i32 * env / 5;
                ui::fill_rect(c, Rect::new(x + col, mid + dy, 1, 1), ink);
            }
        }
        Activity::Meter => {
            let segs = (w / 4).max(1);
            let lit = ((level.clamp(0.0, 1.0) * segs as f32).round() as i32).min(segs);
            for i in 0..segs {
                let r = Rect::new(x + i * 4, y + 3, 3, (h - 6) as u32);
                if i < lit {
                    ui::fill_rect(c, r, ink);
                } else {
                    ui::fill_rect(c, Rect::new(r.x(), r.bottom() - 1, 3, 1), dim);
                }
            }
        }
        Activity::Bars => {
            let n = (w / 5).max(1);
            for i in 0..n {
                let hh = 2 + ((frame * 7 + i as usize * 13) ^ (i as usize * 5 + 3)) as i32 % (h - 3);
                ui::fill_rect(c, Rect::new(x + i * 5, y + h - 1 - hh, 3, hh as u32), ink);
            }
        }
    }
}

/// Four 4 px corner brackets just inside `r`.
fn brackets(c: &mut Canvas<Window>, r: Rect, col: sdl2::pixels::Color) {
    let (x0, y0) = (r.x(), r.y());
    let (x1, y1) = (r.x() + r.width() as i32 - 1, r.y() + r.height() as i32 - 1);
    for (x, y, dx, dy) in [(x0, y0, 1, 1), (x1, y0, -1, 1), (x0, y1, 1, -1), (x1, y1, -1, -1)] {
        ui::line(c, x, y, x + dx * 4, y, col);
        ui::line(c, x, y, x, y + dy * 4, col);
    }
}

/// A 1 px dashed rule (2 on, 2 off).
fn dashed(c: &mut Canvas<Window>, x: i32, y: i32, w: i32, col: sdl2::pixels::Color) {
    let mut i = 0;
    while i < w {
        ui::fill_rect(c, Rect::new(x + i, y, 2.min((w - i) as u32), 1), col);
        i += 4;
    }
}

impl AgentApp {
    /// The empty chat: the block-letter logo (with CRT scan lines through it), a dashed rule and the hints.
    fn draw_empty(&self, c: &mut Canvas<Window>, fs: &crate::font::FontSystem, tm: &crate::theme::ThemeManager, area: Rect) {
        let (ink, paper) = (tm.get_color("foreground"), tm.get_color("widget_face_active"));
        let (faint, line) = (widgets::secondary_ink(tm), tm.get_color("line"));
        let scale = 3;
        let w = (5 * 5 + 4) * scale;
        let (x, y) = (area.x() + (area.width() as i32 - w) / 2, area.y() + 8);
        for (li, letter) in LOGO.iter().enumerate() {
            let lx = x + li as i32 * 6 * scale;
            for (ry, row) in letter.iter().enumerate() {
                for (rx, b) in row.bytes().enumerate() {
                    if b == b'#' {
                        ui::fill_rect(c, Rect::new(lx + rx as i32 * scale, y + ry as i32 * scale, scale as u32, scale as u32), ink);
                    }
                }
            }
        }
        // Scan lines: every third pixel row of the logo is cut back to the paper colour.
        for k in (1..5 * scale).step_by(3) {
            ui::fill_rect(c, Rect::new(x, y + k, w as u32, 1), paper);
        }
        dashed(c, area.x() + 8, y + 5 * scale + 6, area.width() as i32 - 16, line);
        let hints = self.hint_lines();
        for (i, h) in hints.iter().enumerate() {
            let row = Rect::new(TEXT_X, y + 5 * scale + 10 + i as i32 * 14, TEXT_W as u32, 14);
            let t = crate::chrome::fit_text(fs, &h.text, TEXT_W);
            widgets::draw_row_text(c, fs, tm, row, TEXT_X, &t, false, faint);
        }
    }
}

impl LauncherApp for AgentApp {
    fn title(&self) -> String {
        "Agent".into()
    }

    fn describe(&self) -> String {
        format!("Agent chat ({} messages)", self.messages.len())
    }

    fn menu(&self) -> Vec<MenuItem> {
        vec![
            MenuItem::new("new", "New Chat"),
            MenuItem::new("provider", "Provider..."),
            MenuItem::new("tools", "Show Tools"),
            MenuItem::new("auto", if self.auto_run { "Auto-run Tools: On" } else { "Auto-run Tools: Off" }),
            MenuItem::new("clear", "Clear History"),
        ]
    }

    fn menu_action(&mut self, id: &str, _ctx: &Ctx) -> AppAction {
        match id {
            "new" => self.new_chat(),
            "provider" => {
                let rows: Vec<String> = self
                    .agent_cfg
                    .names()
                    .iter()
                    .map(|n| {
                        let p = &self.agent_cfg.providers[n];
                        let model = p.model.clone().unwrap_or_else(|| if p.kind == "muse" { "(Muse)".into() } else { String::new() });
                        format!("{}{} {}", if *n == self.provider_name { "* " } else { "" }, n, model)
                    })
                    .collect();
                let mut p = ListPicker::new("Provider:", rows);
                p.sel = self.agent_cfg.names().iter().position(|n| *n == self.provider_name).unwrap_or(0);
                self.picker = Some(p);
            }
            "tools" => {
                let names: Vec<&str> = tools::specs().iter().map(|t| t.name).collect();
                self.notice(&format!("Tools: {}", names.join(", ")));
            }
            "auto" => {
                self.auto_run = !self.auto_run;
                self.save_state();
                self.status = Some(format!("auto-run tools: {}", if self.auto_run { "on" } else { "off" }));
            }
            "clear" => {
                history::delete_all(&self.dir);
                self.new_chat();
                self.status = Some("history cleared".into());
            }
            _ => {}
        }
        AppAction::None
    }

    fn key(&mut self, key: Keycode, mods: Mod, _repeat: bool, ctx: &Ctx) -> AppAction {
        self.ensure_lines(&|s| ctx.fs.measure_text(s));
        if let Some(p) = &mut self.picker {
            if let Some(r) = p.key(key) {
                self.apply_pick(r);
            }
            return AppAction::None;
        }
        if let Some(Remote::Confirm(_, alert)) = &mut self.remote {
            if let Some(ch) = alert.key(key) {
                self.apply_remote_confirm(ch, ctx.sys);
            }
            return AppAction::None;
        }
        if let Phase::Confirm(_, alert) = &mut self.phase {
            if let Some(ch) = alert.key(key) {
                self.apply_confirm(ch, ctx.sys);
            }
            return AppAction::None;
        }
        if self.unconfigured() {
            return self.setup_key(key);
        }
        match key {
            Keycode::Escape => return AppAction::Quit,
            Keycode::Return | Keycode::KpEnter => self.try_send(ctx.sys),
            Keycode::Up if self.input.is_empty() => self.scroll_to(self.scroll as isize - 1),
            Keycode::Down if self.input.is_empty() => self.scroll_to(self.scroll as isize + 1),
            Keycode::PageUp => self.scroll_to(self.scroll as isize - ROWS as isize),
            Keycode::PageDown => self.scroll_to(self.scroll as isize + ROWS as isize),
            _ => {
                self.input.key(key, mods);
            }
        }
        AppAction::None
    }

    fn text(&mut self, text: &str, _ctx: &Ctx) -> AppAction {
        if !self.unconfigured() && self.picker.is_none() && !matches!(self.phase, Phase::Confirm(..)) && !matches!(self.remote, Some(Remote::Confirm(..))) && self.input.len() < MAX_INPUT {
            self.input.insert(text);
        }
        AppAction::None
    }

    fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> AppAction {
        self.ensure_lines(&|s| ctx.fs.measure_text(s));
        if self.unconfigured() && self.picker.is_none() {
            self.setup_pointer(ev);
            return AppAction::None;
        }
        if let Some(p) = &mut self.picker {
            if let PointerEvent::Down { x, y, button: 0, .. } = ev {
                if let Some(r) = p.click(x, y) {
                    self.apply_pick(r);
                }
            } else if let PointerEvent::Wheel { dy, .. } = ev {
                p.wheel(dy);
            }
            return AppAction::None;
        }
        if let Some(Remote::Confirm(_, alert)) = &mut self.remote {
            if let PointerEvent::Down { x, y, button: 0, .. } = ev {
                if let Some(ch) = alert.click(x, y) {
                    self.apply_remote_confirm(ch, ctx.sys);
                }
            }
            return AppAction::None;
        }
        if let Phase::Confirm(_, alert) = &mut self.phase {
            if let PointerEvent::Down { x, y, button: 0, .. } = ev {
                if let Some(ch) = alert.click(x, y) {
                    self.apply_confirm(ch, ctx.sys);
                }
            }
            return AppAction::None;
        }
        match ev {
            PointerEvent::Move { x, y } => self.send_hover = contains(Self::send_rect(), x, y),
            PointerEvent::Down { x, y, button: 0, .. } => {
                if contains(Self::send_rect(), x, y) {
                    self.try_send(ctx.sys);
                } else if let Some((off, drag)) = self.scrollbar().press(x, y) {
                    self.scroll_to(off as isize);
                    self.sb_drag = drag;
                }
            }
            PointerEvent::Drag { y, .. } if self.sb_drag => {
                let off = self.scrollbar().offset_for_y(y);
                self.scroll_to(off as isize);
            }
            PointerEvent::Up { .. } => self.sb_drag = false,
            PointerEvent::Wheel { dy, .. } => self.scroll_to(self.scroll as isize - dy.signum() as isize * 3),
            _ => {}
        }
        AppAction::None
    }

    fn tick(&mut self, ctx: &Ctx) -> bool {
        let mut dirty = false;
        // 120 ms frames for the spinner / scan line while busy, a 530 ms blink for the block cursor.
        let ms = self.started.elapsed().as_millis();
        let frame = (ms / 80) as usize; // ~12 fps while busy
        let frame_changed = frame != self.dots;
        let blink = (ms / 530 % 2) as usize;
        if self.is_busy() && frame != self.dots {
            dirty = true;
        }
        if !self.is_busy() && blink != self.blink {
            dirty = true;
        }
        self.dots = frame;
        self.blink = blink;
        if self.poll_voice() || (frame_changed && self.voice.as_ref().map_or(false, |v| v.active())) {
            dirty = true;
        }
        if self.poll_worker(ctx.sys) {
            dirty = true;
        }
        if self.poll_remote(ctx.sys) {
            dirty = true;
        }
        if !self.is_busy() {
            let mut got = None;
            for s in &mut self.sources {
                if let Some(t) = s.poll() {
                    got = Some(t);
                    break;
                }
            }
            if let Some(t) = got {
                self.submit(&t, ctx.sys);
                dirty = true;
            }
        }
        dirty
    }

    fn draw(&mut self, c: &mut Canvas<Window>, _assets: &mut AssetStore, ctx: &Ctx) {
        let (tm, fs) = (ctx.tm, ctx.fs);
        self.ensure_lines(&|s| fs.measure_text(s));
        let ink = tm.get_color("foreground");
        let paper = tm.get_color("widget_face_active");
        let faint = widgets::secondary_ink(tm);
        ui::fill_rect(c, Rect::new(PANEL_X, TOP, PANEL_W as u32, (grid::SCREEN_H as i32 - TOP) as u32), tm.get_color("home_bg"));
        ui::fill_rect(c, Rect::new(PANEL_X, TOP, 1, (grid::APP_FRAME_BOTTOM + 1 - TOP) as u32), ink);

        if self.unconfigured() && self.picker.is_none() {
            // Empty state: one line and one white button.
            let msg = "Agent is not set up.";
            let w = fs.measure_text(msg);
            let row = Rect::new(PANEL_X, TOP + 16, PANEL_W as u32, grid::ROW_H as u32);
            widgets::draw_row_text(c, fs, tm, row, PANEL_X + (PANEL_W - w) / 2, msg, false, ink);
            if let Err(e) = &self.provider {
                if !e.is_empty() {
                    let t = crate::chrome::fit_text(fs, e, PANEL_W - 2 * grid::MARGIN);
                    let row = Rect::new(PANEL_X, TOP + 36, PANEL_W as u32, grid::ROW_H as u32);
                    widgets::draw_row_text(c, fs, tm, row, PANEL_X + (PANEL_W - fs.measure_text(&t)) / 2, &t, false, faint);
                }
            }
            let st = BtnState { hover: self.setup_hover, pressed: false, default: true, disabled: false };
            draw_button(c, fs, tm, Self::setup_button(), "Open Settings", st);
            return;
        }

        let area = Rect::new(TEXT_X - 2, TOP, (TEXT_W + 4) as u32, AREA_H as u32);
        ui::fill_rect(c, area, paper);
        let line = tm.get_color("line");
        if self.messages.is_empty() {
            self.draw_empty(c, fs, tm, area);
        } else {
            for (i, l) in self.lines.iter().skip(self.scroll).take(ROWS).enumerate() {
                let y = TEXT_Y + i as i32 * grid::ROW_H;
                let row = Rect::new(TEXT_X + MARK_W, y, WRAP_W as u32, grid::ROW_H as u32);
                let col = match l.kind {
                    LineKind::User | LineKind::Assistant => ink,
                    LineKind::Tool | LineKind::Notice => faint,
                };
                match l.kind {
                    // You: a ">" prompt on the first line.
                    LineKind::User if l.first => {
                        let r = Rect::new(TEXT_X, y, MARK_W as u32, grid::ROW_H as u32);
                        widgets::draw_row_text(c, fs, tm, r, TEXT_X, ">", false, ink);
                    }
                    // The agent: a thin gutter bar down the whole message.
                    LineKind::Assistant => ui::fill_rect(c, Rect::new(TEXT_X + 1, y, 1, grid::ROW_H as u32), line),
                    _ => {}
                }
                widgets::draw_row_text(c, fs, tm, row, TEXT_X + MARK_W, &l.text, false, col);
            }
        }
        // HUD corner brackets on the transcript.
        brackets(c, area, ink);
        self.scrollbar().draw(c, tm, None);

        // Status row: a dashed rule (with a scan line sliding along it while busy), an LED, the state
        // word (shimmering and glitching while busy), a small activity strip (wave / level meter /
        // equalizer) and a spinner. Nothing here allocates: it all runs on stack buffers.
        let vstate = self.voice_state();
        let busy = self.is_busy() || vstate != VoiceState::Idle;
        let frame = self.dots;
        dashed(c, TEXT_X - 2, STATUS_Y, TEXT_W + 4, line);
        if busy {
            let span = TEXT_W + 4 + 24;
            let x0 = TEXT_X - 2 + (frame as i32 * 4) % span - 24;
            let (l, r) = (x0.max(TEXT_X - 2), (x0 + 24).min(TEXT_X + TEXT_W + 2));
            if r > l {
                ui::fill_rect(c, Rect::new(l, STATUS_Y, (r - l) as u32, 1), ink);
            }
        }
        let (word, alert) = self.state_word();
        let row = Rect::new(TEXT_X, STATUS_Y + 1, TEXT_W as u32, (grid::ROW_H - 1) as u32);
        let led = Rect::new(TEXT_X + 1, STATUS_Y + 6, 5, 5);
        if (busy && frame % 6 < 4) || alert {
            ui::fill_rect(c, led, ink); // the LED blinks while busy
        } else {
            ui::draw_rect(c, led, ink);
        }
        let wx = TEXT_X + 11;
        let ww = fs.measure_text(word);
        if alert {
            widgets::draw_row_plate(c, tm, Rect::new(wx - 3, STATUS_Y + 1, (ww + 6) as u32, grid::ROW_H as u32));
            widgets::draw_row_text(c, fs, tm, row, wx, word, true, ink);
        } else if busy {
            let mut buf = [0u8; 12];
            let (shown, hl) = shimmer(&mut buf, word, frame);
            widgets::draw_row_text(c, fs, tm, row, wx, shown, false, ink);
            if hl < shown.len() {
                // The "scanned" letter: a plate with the letter in the paper colour.
                let pre = fs.measure_text(&shown[..hl]);
                let lw = fs.measure_text(&shown[hl..hl + 1]);
                ui::fill_rect(c, Rect::new(wx + pre - 1, STATUS_Y + 3, (lw + 2) as u32, 11), ink);
                fs.draw_text(c, wx + pre, widgets::text_origin(STATUS_Y + 1, grid::ROW_H - 1), &shown[hl..hl + 1], paper);
            }
        } else {
            widgets::draw_row_text(c, fs, tm, row, wx, word, false, ink);
        }
        let mut right = TEXT_X + TEXT_W;
        if busy && vstate != VoiceState::Recording {
            let spin = ["|", "/", "-", "\\"][frame % 4];
            widgets::draw_row_text(c, fs, tm, row, right - 6, spin, false, ink);
            right -= 14;
        }
        if self.auto_run {
            let w = fs.measure_text("AUTO");
            widgets::draw_row_text(c, fs, tm, row, right - w, "AUTO", false, faint);
            right -= w + 8;
        }
        if busy {
            let ax = wx + ww + 10;
            let aw = (right - ax - 4).min(64);
            if aw >= 16 {
                let level = self.voice.as_ref().map_or(0.0, |v| v.level());
                let kind = match vstate {
                    VoiceState::Recording => Activity::Meter,
                    VoiceState::Playing => Activity::Bars,
                    _ => Activity::Wave,
                };
                draw_activity(c, kind, ax, STATUS_Y + 2, aw, 13, frame, level, ink, line);
            }
        } else if let Some(note) = self.state_note() {
            let nx = wx + ww + 10;
            let t = crate::chrome::fit_text(fs, &note, (right - nx).max(0));
            if !t.is_empty() {
                widgets::draw_row_text(c, fs, tm, row, nx, &t, false, faint);
            }
        }

        // Input line: the shared text field with a ">" prompt, then the Send button.
        let field = Rect::new(TEXT_X, INPUT_Y, FIELD_W as u32, widgets::FIELD_H as u32);
        let typing = (!busy).then_some(self.input.caret);
        widgets::draw_field(c, fs, tm, field, &widgets::Field { text: &self.input.text, caret: typing, placeholder: "type here", prefix: ">", ..Default::default() });
        let st = BtnState { hover: self.send_hover, pressed: false, default: false, disabled: self.input.text.trim().is_empty() || busy };
        draw_button(c, fs, tm, Self::send_rect(), "Send", st);

        if let Phase::Confirm(_, a) = &self.phase {
            a.draw(c, fs, tm);
        }
        if let Some(Remote::Confirm(_, a)) = &self.remote {
            a.draw(c, fs, tm);
        }
        if let Some(p) = &self.picker {
            p.draw(c, fs, tm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::mock::MockBackend;

    fn cell(s: &str) -> i32 {
        // Latin 6 px, others (Japanese) 12 px, like the body font.
        s.chars().map(|c| if c.is_ascii() { 6 } else { 12 }).sum()
    }

    fn app(tag: &str, mock: bool) -> (AgentApp, PathBuf) {
        let d = std::env::temp_dir().join(format!("agent-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let cfg = Arc::new(Mutex::new(Config::load(d.join("config.json"))));
        let a = AgentApp::new(cfg, mock, d.join("none.json"), d.join("data"));
        (a, d)
    }

    /// Run the loop until idle or a host request appears.
    fn pump(a: &mut AgentApp, sys: &dyn Backend) {
        for _ in 0..400 {
            a.poll_worker(sys);
            if !matches!(a.phase, Phase::Thinking) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("worker did not answer");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_inside_japanese() {
        let m = |s: &str| cell(s);
        assert_eq!(wrap_text("hello big world", 60, &m), ["hello big", "world"]);
        let jp = wrap_text("日本語のテキストを折り返す", 60, &m);
        assert!(jp.len() >= 3 && jp.iter().all(|l| cell(l) <= 60), "{jp:?}");
        assert_eq!(jp.concat(), "日本語のテキストを折り返す");
        assert_eq!(wrap_text("a\nb", 60, &m), ["a", "b"]);
        assert_eq!(wrap_text("abcdefghijklmnop", 30, &m).len(), 4, "a word longer than a line is cut");
        assert_eq!(wrap_text("", 60, &m), [""]);
    }

    #[test]
    fn a_question_runs_a_tool_loop_with_a_confirmation_card() {
        let (mut a, d) = app("loop", true);
        let sys = MockBackend::new();
        a.submit("please open files", &sys);
        pump(&mut a, &sys);
        // launch_app is a side effect: the card waits for the user.
        assert!(matches!(a.phase, Phase::Confirm(..)), "confirmation expected");
        assert!(a.status_text().contains("launch Files"));
        a.apply_confirm(ConfirmChoice::Ok, &sys);
        // The main loop would run it now.
        assert_eq!(a.take_host_req(), Some(HostReq::Launch("Files".into())));
        a.host_reply(Ok("Opened Files".into()), &sys);
        pump(&mut a, &sys);
        assert!(!a.is_busy());
        let last = a.messages().last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert!(last.text.contains("mock"));
        let roles: Vec<_> = a.messages().iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::User, Role::Assistant, Role::Tool, Role::Assistant]);
        assert!(!history::chat_files(&d.join("data")).is_empty(), "the chat is saved");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_muse_command_goes_through_the_confirmation_card_and_the_host() {
        let (mut a, d) = app("muse", true);
        let sys = MockBackend::new();
        let (tx, rx) = channel();
        a.remote_rx = Some(rx);
        let call = |name: &str, args: serde_json::Value| {
            let (rtx, rrx) = channel();
            let rc = RemoteCall { call: ToolCall { id: "m1".into(), name: name.into(), args }, reply: rtx, deadline: Instant::now() + std::time::Duration::from_secs(5) };
            (rc, rrx)
        };
        // A read tool runs at once and answers the socket side.
        let (rc, rrx) = call("system_info", serde_json::json!({}));
        tx.send(rc).unwrap();
        assert!(a.poll_remote(&sys));
        assert!(rrx.try_recv().unwrap().unwrap().contains("battery"));
        // A side-effect tool waits for the user, then goes to the host.
        let (rc, rrx) = call("launch_app", serde_json::json!({"name": "Files"}));
        tx.send(rc).unwrap();
        assert!(a.poll_remote(&sys));
        assert!(matches!(a.remote, Some(Remote::Confirm(..))));
        assert!(a.status_text().contains("launch Files"));
        assert!(rrx.try_recv().is_err(), "nothing runs before the user says yes");
        a.apply_remote_confirm(ConfirmChoice::Ok, &sys);
        assert_eq!(a.take_host_req(), Some(HostReq::Launch("Files".into())));
        a.host_reply(Ok("Launched Files.".into()), &sys);
        assert_eq!(rrx.try_recv().unwrap(), Ok("Launched Files.".to_string()));
        assert!(a.remote.is_none());
        // Declining answers Muse with an error, and nothing reaches the host.
        let (rc, rrx) = call("set_setting", serde_json::json!({"name": "volume", "value": "10"}));
        tx.send(rc).unwrap();
        a.poll_remote(&sys);
        a.apply_remote_confirm(ConfirmChoice::Cancel, &sys);
        assert!(rrx.try_recv().unwrap().unwrap_err().contains("declined"));
        assert!(a.take_host_req().is_none());
        // A call whose socket gave up is dropped, not run late.
        let (mut rc, rrx) = call("launch_app", serde_json::json!({"name": "Files"}));
        rc.deadline = Instant::now() - std::time::Duration::from_secs(1);
        tx.send(rc).unwrap();
        assert!(!a.poll_remote(&sys));
        assert!(a.remote.is_none() && rrx.try_recv().is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn declining_tells_the_model_and_continues() {
        let (mut a, d) = app("decline", true);
        let sys = MockBackend::new();
        a.submit("open files", &sys);
        pump(&mut a, &sys);
        a.apply_confirm(ConfirmChoice::Cancel, &sys);
        pump(&mut a, &sys);
        let tool = a.messages().iter().find(|m| m.role == Role::Tool).unwrap();
        assert!(tool.is_error && tool.text.contains("declined"));
        assert!(a.take_host_req().is_none(), "nothing ran");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn auto_run_skips_the_card_and_read_tools_never_ask() {
        let (mut a, d) = app("auto", true);
        let sys = MockBackend::new();
        a.submit("list the apps", &sys); // list_apps: no confirmation
        pump(&mut a, &sys);
        assert_eq!(a.take_host_req(), Some(HostReq::ListApps));
        a.host_reply(Ok("Files\nTimer".into()), &sys);
        pump(&mut a, &sys);
        assert!(!a.is_busy());
        a.auto_run = true;
        a.submit("open files", &sys);
        pump(&mut a, &sys);
        assert_eq!(a.take_host_req(), Some(HostReq::Launch("Files".into())), "ran without a card");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn errors_are_shown_in_plain_words_and_the_chat_stays_usable() {
        let (mut a, d) = app("err", true);
        let sys = MockBackend::new();
        a.submit("trigger an error", &sys);
        pump(&mut a, &sys);
        assert!(!a.is_busy());
        assert!(a.status_text().starts_with("! network error"), "{}", a.status_text());
        a.submit("hello", &sys);
        pump(&mut a, &sys);
        assert_eq!(a.messages().last().unwrap().role, Role::Assistant);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_tool_loop_stops_after_eight_rounds() {
        struct Loopy;
        impl Provider for Loopy {
            fn label(&self) -> String {
                "loopy".into()
            }
            fn chat(&self, _r: &ChatRequest, emit: &mut dyn FnMut(ProviderEvent)) {
                emit(ProviderEvent::ToolCall(ToolCall { id: "x".into(), name: "system_info".into(), args: serde_json::json!({}) }));
                emit(ProviderEvent::Done);
            }
        }
        let (mut a, d) = app("rounds", true);
        a.provider = Ok(Arc::new(Loopy));
        let sys = MockBackend::new();
        a.submit("go", &sys);
        for _ in 0..40 {
            pump(&mut a, &sys);
            if !a.is_busy() {
                break;
            }
        }
        assert!(!a.is_busy());
        let rounds = a.messages().iter().filter(|m| m.role == Role::Assistant).count();
        assert_eq!(rounds, MAX_ROUNDS);
        assert!(a.messages().last().unwrap().text.contains("Stopped after 8"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_provider_means_a_plain_hint_not_a_crash() {
        let (mut a, d) = app("noprov", false);
        // Without an agent.yaml the mock answers, which is what the empty hint says.
        assert!(!a.config_exists);
        a.provider = Err("groq: environment variable GROQ_API_KEY is not set".into());
        let sys = MockBackend::new();
        a.submit("hi", &sys);
        assert!(!a.is_busy());
        assert!(a.status_text().contains("GROQ_API_KEY"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn without_a_usable_provider_the_panel_offers_open_settings() {
        // No agent.yaml and no HARBOUR_MOCK: only the built-in mock exists, which is not "set up".
        let (mut a, d) = app("unconf", false);
        assert!(a.unconfigured());
        assert!(!a.take_open_settings());
        assert_eq!(a.setup_key(Keycode::Down), AppAction::None);
        assert!(!a.take_open_settings(), "other keys do nothing");
        assert_eq!(a.setup_key(Keycode::Escape), AppAction::Quit, "Esc still closes the panel");
        assert_eq!(a.setup_key(Keycode::Return), AppAction::None);
        assert!(a.take_open_settings(), "Enter activates the button");
        assert!(!a.take_open_settings(), "the request is taken once");
        // A pointer click on the button does the same; a click elsewhere does not.
        a.setup_pointer(PointerEvent::Down { x: 5, y: 5, button: 0, clicks: 1 });
        assert!(!a.take_open_settings());
        let b = AgentApp::setup_button();
        a.setup_pointer(PointerEvent::Down { x: b.x() + 5, y: b.y() + 5, button: 0, clicks: 1 });
        assert!(a.take_open_settings());
        // A broken provider (missing key) is the same empty state; HARBOUR_MOCK counts as set up.
        let (mut m, d2) = app("unconf2", true);
        assert!(!m.unconfigured());
        m.provider = Err("groq: environment variable GROQ_API_KEY is not set".into());
        assert!(m.unconfigured());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }

    #[test]
    fn the_status_row_shows_a_state_word_and_never_the_provider() {
        let (mut a, d) = app("word", true);
        let sys = MockBackend::new();
        assert_eq!(a.state_word(), ("READY", false));
        a.phase = Phase::Thinking; // (the mock's "slow" answer takes seconds; screenshots use it)
        assert_eq!(a.state_word(), ("THINKING", false));
        a.phase = Phase::Idle;
        assert_eq!(a.state_word(), ("READY", false));
        a.submit("trigger an error", &sys);
        pump(&mut a, &sys);
        assert_eq!(a.state_word(), ("ERROR", true), "errors are an alert word; the words are in the transcript");
        assert!(a.messages().iter().any(|m| m.role == Role::Notice && m.text.starts_with("Error:")));
        // Provider / model names never reach the words or the note, in any state.
        a.provider_name = "groq".into();
        a.model_override = Some("secret-model-9".into());
        for w in [a.state_word().0.to_string(), a.state_note().unwrap_or_default()] {
            assert!(!w.contains("groq") && !w.contains("secret-model") && !w.contains("mock"), "{w}");
        }
        a.status = Some("auto-run tools: on".into());
        assert_eq!(a.state_word(), ("READY", false));
        assert_eq!(a.state_note().as_deref(), Some("auto-run tools: on"));
        a.status = None;
        a.config_warning = Some("agent.yaml is readable by others: run chmod 600".into());
        assert_eq!(a.state_word(), ("WARN", true));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn messages_wrap_to_the_marker_column_and_mark_their_first_line() {
        let (mut a, d) = app("marks", true);
        a.messages.push(Message::user("one"));
        a.messages.push(Message::assistant(&"word ".repeat(40), vec![ToolCall { id: "1".into(), name: "list_apps".into(), args: serde_json::json!({}) }]));
        a.bump();
        a.ensure_lines(&cell);
        assert!(a.lines.iter().all(|l| cell(&l.text) <= WRAP_W), "every line fits beside the marker column");
        assert_eq!(a.lines[0], Line { kind: LineKind::User, text: "one".into(), first: true }, "no '> ' in the text: the marker is drawn");
        let assistant: Vec<_> = a.lines.iter().filter(|l| l.kind == LineKind::Assistant).collect();
        assert!(assistant.len() > 2 && assistant[0].first && assistant[1..].iter().all(|l| !l.first));
        assert_eq!(a.lines.last().unwrap().kind, LineKind::Tool);
        assert!(a.lines.last().unwrap().text.starts_with("ran: "));
        // An empty chat has no transcript lines: the logo and hints are drawn instead.
        a.new_chat();
        a.ensure_lines(&cell);
        assert!(a.lines.is_empty() && a.messages().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_logo_letters_are_five_by_five() {
        for l in LOGO.iter() {
            assert!(l.iter().all(|r| r.len() == 5 && r.bytes().all(|b| b == b'#' || b == b'.')));
        }
    }

    #[test]
    fn push_to_talk_needs_a_muse_provider_a_vm_id_and_voice_on() {
        let (mut a, d) = app("vcfg", false);
        a.provider = Ok(Arc::new(provider::MockProvider)); // configured enough to show the chat
        a.provider_name = "groq".into();
        assert!(!a.voice_available());
        assert!(a.muse_voice_settings().unwrap_err().contains("Muse provider"));
        let cfg = |muse: &str| AgentConfig::parse(&format!(r#"{{"providers":{{"muse":{{"kind":"muse","muse":{muse}}}}}}}"#)).unwrap();
        a.agent_cfg = cfg("{}");
        assert!(a.muse_voice_settings().unwrap_err().contains("vm_id"));
        a.agent_cfg = cfg(r#"{"vm_id":"vm-1","voice":false}"#);
        assert!(a.muse_voice_settings().unwrap_err().contains("off"));
        a.agent_cfg = cfg(r#"{"vm_id":"vm-1","sdk_command":"/opt/m/musegadget"}"#);
        let s = a.muse_voice_settings().unwrap();
        assert_eq!((s.vm_id.as_deref(), s.sdk_command.as_str()), (Some("vm-1"), "/opt/m/musegadget"));
        assert!(a.voice_available());
        // Without a usable setup, holding the key says why in the chat (no process is started).
        a.agent_cfg = cfg("{}");
        a.voice_input(true);
        assert!(a.status_text().starts_with("! Voice needs muse.vm_id"), "{}", a.status_text());
        assert_eq!(a.state_word(), ("ERROR", true));
        assert!(a.messages().iter().any(|m| m.role == Role::Notice && m.text.starts_with("Voice: ")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn push_to_talk_shows_rec_then_sending_then_playing_with_a_fake_backend() {
        use crate::launcher::agent::voice::FakeVoice;
        let (mut a, d) = app("vflow", true);
        let mut f = FakeVoice::new();
        f.call_ms = 60;
        f.play_ms = 60;
        let f = Arc::new(f);
        a.set_voice_backend(f.clone());
        a.voice_input(true);
        assert_eq!(a.voice_state(), VoiceState::Recording);
        assert_eq!(a.state_word(), ("REC", true));
        std::thread::sleep(std::time::Duration::from_millis(450));
        a.voice_input(false);
        assert_eq!(a.state_word(), ("SENDING", false));
        let mut seen_play = false;
        for _ in 0..400 {
            a.poll_voice();
            seen_play |= a.state_word().0 == "PLAYING";
            if a.voice_state() == VoiceState::Idle {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(seen_play || f.log.lock().unwrap().contains(&"play".to_string()));
        assert_eq!(a.voice_state(), VoiceState::Idle);
        let notes: Vec<&str> = a.messages().iter().filter(|m| m.role == Role::Notice).map(|m| m.text.as_str()).collect();
        assert!(notes.iter().any(|t| t.starts_with("voice: sent")) && notes.iter().any(|t| t.contains("Muse answered")), "{notes:?}");
        assert_eq!(a.state_word(), ("READY", false));
        // A question being answered blocks the key; the panel closing drops a recording.
        a.phase = Phase::Thinking;
        a.voice_input(true);
        assert_eq!(a.voice_state(), VoiceState::Idle);
        assert!(a.status_text().contains("wait for the current answer") || a.state_word().0 == "THINKING");
        a.phase = Phase::Idle;
        a.status = None;
        a.voice_input(true);
        assert_eq!(a.voice_state(), VoiceState::Recording);
        a.on_hidden();
        assert_eq!(a.voice_state(), VoiceState::Idle);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_busy_animation_helpers_are_stack_only_and_deterministic() {
        let mut buf = [0u8; 12];
        for frame in 0..200 {
            let (s, hl) = shimmer(&mut buf, "THINKING", frame);
            assert_eq!(s.len(), 8);
            assert!(hl == usize::MAX || hl < 8);
            // Most frames show the word untouched; a glitch replaces exactly one letter.
            let diff = s.bytes().zip("THINKING".bytes()).filter(|(a, b)| a != b).count();
            assert!(diff <= 1, "{s}");
            if frame % 9 >= 2 {
                assert_eq!(s, "THINKING");
            }
        }
        assert_eq!(shimmer(&mut buf, "", 3), ("", usize::MAX));
        let (s, _) = shimmer(&mut buf, "A_VERY_LONG_STATE_WORD", 1);
        assert_eq!(s.len(), 12, "clamped to the buffer");
    }

    #[test]
    fn scrolling_sticks_to_the_bottom_until_the_user_scrolls_up() {
        let (mut a, d) = app("scroll", true);
        for i in 0..20 {
            a.messages.push(Message::assistant(&format!("line {i}"), vec![]));
        }
        a.bump();
        a.ensure_lines(&cell);
        assert_eq!(a.scroll, a.max_scroll());
        a.scroll_to(a.scroll as isize - 3);
        let kept = a.scroll;
        assert!(!a.stick);
        a.messages.push(Message::assistant("new", vec![]));
        a.bump();
        a.ensure_lines(&cell);
        assert_eq!(a.scroll, kept, "stays where the user put it");
        a.scroll_to(1000);
        assert!(a.stick);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn menu_toggles_auto_run_and_persists_it() {
        let (mut a, d) = app("menu", true);
        assert!(!a.auto_run());
        assert_eq!(a.menu().iter().find(|m| m.id == "auto").unwrap().label, "Auto-run Tools: Off");
        a.auto_run = true;
        a.save_state();
        assert!(history::load_state(&d.join("data")).auto_run);
        let _ = std::fs::remove_dir_all(&d);
    }
}
