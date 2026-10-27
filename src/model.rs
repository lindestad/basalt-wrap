use crate::hardware::Hardware;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Instant};

fn epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn number(value: &Value, path: &str) -> f64 {
    value
        .pointer(path)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}
pub fn text<'a>(value: &'a Value, path: &str) -> &'a str {
    value.pointer(path).and_then(Value::as_str).unwrap_or("")
}
pub fn truth(value: &Value, path: &str) -> bool {
    value
        .pointer(path)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn prefill_rate(request: &Value) -> Option<f64> {
    let ms = number(request, "/prompt_ms");
    if ms <= 0.0 {
        return None;
    }
    // New engines report the tokens actually read, including partial reads and
    // prefix copies. Older engines only expose prompt and reused counts.
    let read = request
        .get("prompt_read")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .unwrap_or_else(|| number(request, "/prompt_tokens") - number(request, "/reused"));
    Some(read.max(0.0) * 1000.0 / ms)
}

/// Active decode seconds of a finished request. Basalt reports the decode rate and the wall
/// duration rather than a decode duration, so the rate gives the active part.
pub fn decode_seconds(request: &Value) -> f64 {
    let ms = number(request, "/decode_ms");
    if ms > 0.0 {
        return ms / 1000.;
    }
    let rate = number(request, "/decode_tok_s");
    if rate > 0.0 {
        number(request, "/output_tokens") / rate
    } else {
        0.0
    }
}

/// Basalt names finished requests `r_NNNN`, counting from 1 for each server start. The number
/// is what lets the wrapper tell a request it counted from one it never saw.
pub fn request_number(request: &Value) -> Option<u64> {
    request
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| id.rsplit_once('_'))
        .and_then(|(_, digits)| digits.parse().ok())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub data: Value,
    pub status: Value,
    /// Basalt's per-slot rows from `/slots`. The `/metrics` live block names the newest request
    /// but leaves out how far its prompt has been read and how many tokens it may produce, and
    /// those two are what the progress bars need.
    #[serde(default)]
    pub slots: Value,
    /// What the wrapper has counted of this server's session. Basalt's `/metrics` lists only
    /// its recent finished requests, so the session's sums ride along from poll to poll.
    #[serde(default)]
    pub session: Session,
}

impl Metrics {
    pub fn new(data: Value, status: Value, slots: Value) -> Self {
        Self {
            data,
            status,
            slots,
            session: Session::default(),
        }
    }

    /// The slots holding a request that is reading or decoding.
    pub fn active_slots(&self) -> Vec<&Value> {
        self.slots
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter(|row| text(row, "/state") != "idle")
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Prompt tokens read and prompt length, summed over the reading slots. `None` while the
    /// engine has not sent a progress line yet.
    pub fn prefill_progress(&self) -> Option<(f64, f64)> {
        let rows: Vec<&Value> = self
            .active_slots()
            .into_iter()
            .filter(|row| text(row, "/state") == "prefill")
            .filter(|row| row.get("prefill_done").and_then(Value::as_f64).is_some())
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(rows.into_iter().fold((0., 0.), |(done, total), row| {
            (
                done + number(row, "/prefill_done"),
                total + number(row, "/prompt_tokens"),
            )
        }))
    }

    /// Tokens generated and their cap, summed over the decoding slots.
    pub fn decode_progress(&self) -> Option<(f64, f64)> {
        let rows: Vec<&Value> = self
            .active_slots()
            .into_iter()
            .filter(|row| text(row, "/state") == "decoding")
            .filter(|row| number(row, "/max_tokens") > 0.)
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(rows.into_iter().fold((0., 0.), |(generated, max), row| {
            (
                generated + number(row, "/generated"),
                max + number(row, "/max_tokens"),
            )
        }))
    }

    /// Carry the counts from the previous poll and add the finished requests of this one.
    pub fn observe(&mut self, previous: &Session) {
        self.session = previous.clone();
        self.session.observe(&self.data, &self.status);
    }

    pub fn busy(&self) -> bool {
        number(&self.status, "/activity/in_flight") > 0.0
            || matches!(text(&self.data, "/live/state"), "reading" | "generating")
            || number(&self.data, "/live/queued") > 0.0
    }
    pub fn activity(&self) -> String {
        format!(
            "{}:{}:{}",
            number(&self.status, "/started"),
            number(&self.status, "/activity/requests"),
            number(&self.status, "/activity/last_request_at")
        )
    }
    pub fn cache(&self) -> u64 {
        if !truth(&self.status, "/loaded") {
            return 0;
        }
        self.status
            .pointer("/vram/expert_cache_mib")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| number(&self.data, "/engine/expert_cache_mib") as u64)
    }
    pub fn full_cache(&self) -> u64 {
        if !truth(&self.status, "/loaded") {
            return 0;
        }
        self.status
            .pointer("/vram/expert_cache_full_mib")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| number(&self.data, "/engine/expert_cache_mib") as u64)
    }
    pub fn last(&self) -> &Value {
        self.data.pointer("/requests/0").unwrap_or(&Value::Null)
    }
    /// Session totals. The finished part comes from the requests the wrapper has counted; the
    /// live part adds the request in flight, whose active seconds the server's rate estimate
    /// gives. Basalt's `/metrics` keeps only its recent requests, so a session longer than that
    /// window is a floor: `partial` says so.
    pub fn totals(&self) -> Totals {
        let session = self.session_counts();
        let active = self.busy();
        let live = self.active_slots();
        let slotted = !live.is_empty();
        let (generated, live_input, live_seconds) = if slotted {
            live.into_iter()
                .fold((0., 0., 0.), |(out, input, seconds), row| {
                    let generated = number(row, "/generated");
                    let rate = number(row, "/tok_s");
                    (
                        out + generated,
                        input + number(row, "/prompt_tokens"),
                        seconds
                            + if generated > 0. && rate > 0. {
                                generated / rate
                            } else {
                                0.
                            },
                    )
                })
        } else if active {
            let generated = number(&self.data, "/live/generated");
            let mean = number(&self.data, "/live/tok_s_mean");
            let rate = if mean > 0. {
                mean
            } else {
                number(&self.data, "/live/tok_s")
            };
            (
                generated,
                number(&self.data, "/live/prompt_tokens"),
                if generated > 0. && rate > 0. {
                    generated / rate
                } else {
                    0.
                },
            )
        } else {
            (0., 0., 0.)
        };
        // `/v1/status` counts the request in flight, and a queued one with it.
        let finished = (number(&self.status, "/activity/requests")
            - number(&self.status, "/activity/in_flight"))
        .max(0.) as u64;
        let gaps = if session.requests == 0 {
            0
        } else {
            session.last - session.first + 1 - session.requests
        };
        // Without the slot table the live numbers only cover the newest request, so a session
        // running several at once is a floor while they do.
        let unslotted = !slotted && number(&self.data, "/live/slots/active") > 1.;
        let seconds = session.decode_seconds + live_seconds;
        let rate_tokens = session.output as f64 + if live_seconds > 0.0 { generated } else { 0.0 };
        Totals {
            partial: gaps > 0 || session.first > 1 || finished > session.last || unslotted,
            input: session.input + live_input as u64,
            output: session.output + generated as u64,
            reused: session.reused,
            requests: session.requests.max(finished),
            decode_seconds: seconds,
            tps: if seconds > 0.0 {
                rate_tokens / seconds
            } else {
                0.0
            },
        }
    }

    /// The session's sums including this poll. Counting is idempotent, so a poll already
    /// counted adds nothing; a poll that never went through `observe` still accounts for
    /// whatever the server's own list covers.
    fn session_counts(&self) -> Session {
        let mut session = self.session.clone();
        session.observe(&self.data, &self.status);
        session
    }
}

/// One server's session as the wrapper has seen it. Basalt numbers finished requests from 1 at
/// each start, so a gap in those numbers, or a server that reported more requests than the
/// wrapper counted, means the sums are a floor.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    /// `/started` of the server these counts belong to; a new server starts the counts over.
    server: f64,
    first: u64,
    last: u64,
    requests: u64,
    input: u64,
    output: u64,
    reused: u64,
    decode_seconds: f64,
}

impl Session {
    pub fn observe(&mut self, data: &Value, status: &Value) {
        let started = number(status, "/started");
        if started > 0. && started != self.server {
            *self = Self {
                server: started,
                ..Default::default()
            };
        }
        let Some(rows) = data["requests"].as_array() else {
            return;
        };
        let mut fresh: Vec<(u64, &Value)> = rows
            .iter()
            .filter_map(|row| Some((request_number(row)?, row)))
            .filter(|(id, _)| *id > self.last)
            .collect();
        fresh.sort_by_key(|(id, _)| *id);
        for (id, row) in fresh {
            self.first = if self.first == 0 {
                id
            } else {
                self.first.min(id)
            };
            self.last = self.last.max(id);
            self.requests += 1;
            self.input += number(row, "/prompt_tokens") as u64;
            self.output += number(row, "/output_tokens") as u64;
            self.reused += number(row, "/reused") as u64;
            self.decode_seconds += decode_seconds(row);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Totals {
    pub partial: bool,
    pub input: u64,
    pub output: u64,
    pub reused: u64,
    pub requests: u64,
    pub decode_seconds: f64,
    pub tps: f64,
}

/// The model config's `pricing` block: USD per token. A session's cached prompt tokens
/// are what the conversation cache served back, so they ride the cache-read rate; the
/// act of caching is not charged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    #[serde(default)]
    pub prompt: f64,
    #[serde(default)]
    pub completion: f64,
    #[serde(default, rename = "cache_read")]
    pub cache_read: f64,
}

impl Pricing {
    pub fn cost(&self, totals: &Totals) -> f64 {
        let cached = (totals.reused as f64).min(totals.input as f64);
        (totals.input as f64 - cached) * self.prompt
            + cached * self.cache_read
            + totals.output as f64 * self.completion
    }
}

/// The rates from a model config, when it states them.
pub fn pricing(config: &Value) -> Option<Pricing> {
    config
        .get("pricing")
        .and_then(Value::as_object)
        .and_then(|block| serde_json::from_value(Value::Object(block.clone())).ok())
}

/// The same from a model config file, for an attachment that joined after the worker read it.
pub fn pricing_at(path: &std::path::Path) -> Option<Pricing> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|config| pricing(&config))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    Supervisor(Box<crate::control::Snapshot>),
    ControlReply {
        preview: bool,
        result: Result<Value, String>,
    },
    Metrics(Box<Metrics>),
    Hardware(Box<Hardware>),
    Log {
        message: String,
        raw: bool,
    },
    Policy {
        state: String,
        quiet_left: Option<f64>,
        pending: bool,
    },
}

#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub enum View {
    #[default]
    Overview,
    Requests,
    Logs,
    Policy,
}

pub struct App {
    pub supervisor: Option<Box<crate::control::Snapshot>>,
    pub reload_preview: Option<serde_json::Value>,
    pub control_notice: Option<String>,
    pub control_pending: bool,
    pub age_seconds: bool,
    pub age_now: f64,
    age_updated: Instant,
    pub attached: bool,
    pub step_mib: u64,
    pub theme: crate::theme::Theme,
    pub theme_picker: Option<crate::theme::Theme>,
    pub theme_path: Option<std::path::PathBuf>,
    pub theme_profile: Option<String>,
    pub theme_notice: Option<String>,
    pub model: String,
    pub address: String,
    pub metrics: Metrics,
    pub hardware: Hardware,
    pub logs: VecDeque<String>,
    pub policy: String,
    pub quiet_left: Option<f64>,
    pub resize_pending: bool,
    pub view: View,
    pub scroll: usize,
    pub help: bool,
    pub stopping: bool,
    pub connected: bool,
    pub updated: Option<Instant>,
    pub started: Instant,
    pub tps: VecDeque<u64>,
    pub gpu_load: VecDeque<u64>,
    pub ram: VecDeque<u64>,
    pub min_free_mib: u64,
    pub grow_below_mib: u64,
    pub demo: bool,
    pub pending_g: Option<Instant>,
    pub page_rows: usize,
    pub process_focus: bool,
    pub process_offset: usize,
    pub process_rows: usize,
    pub loading_note: String,
    /// Rates for the session cost in the header, from the model config.
    pub pricing: Option<Pricing>,
}

impl App {
    pub fn new(model: String, port: u16) -> Self {
        Self {
            supervisor: None,
            reload_preview: None,
            control_notice: None,
            control_pending: false,
            age_seconds: false,
            age_now: epoch(),
            age_updated: Instant::now(),
            attached: false,
            step_mib: 512,
            theme: crate::theme::Theme::default(),
            theme_picker: None,
            theme_path: None,
            theme_profile: None,
            theme_notice: None,
            model,
            address: format!("127.0.0.1:{port}"),
            metrics: Metrics::default(),
            hardware: Hardware::default(),
            logs: VecDeque::new(),
            policy: "Waiting for the model".into(),
            quiet_left: None,
            resize_pending: false,
            view: View::default(),
            scroll: 0,
            help: false,
            stopping: false,
            connected: false,
            updated: None,
            started: Instant::now(),
            tps: VecDeque::new(),
            gpu_load: VecDeque::new(),
            ram: VecDeque::new(),
            min_free_mib: 1024,
            grow_below_mib: 14336,
            demo: false,
            pending_g: None,
            page_rows: 12,
            process_focus: false,
            process_offset: 0,
            process_rows: 3,
            loading_note: "Loading model weights and preparing the expert cache…".into(),
            pricing: None,
        }
    }
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::ControlReply { preview, result } => {
                self.control_pending = false;
                match result {
                    Ok(value) => {
                        if preview {
                            self.reload_preview = Some(value);
                            self.control_notice = None;
                        } else {
                            self.reload_preview = None;
                            self.control_notice = Some(
                                "Policy updated · growth timer restarted; queued resize unchanged"
                                    .into(),
                            );
                            if let Some(paused) = value["growth_paused"].as_bool()
                                && let Some(state) = &mut self.supervisor
                            {
                                state.growth_paused = paused;
                            }
                        }
                    }
                    Err(error) => self.control_notice = Some(error),
                }
            }
            Event::Supervisor(snapshot) => {
                if self.view == View::Policy
                    && self.scroll > 0
                    && let Some(old) = &self.supervisor
                {
                    let old_selected = old
                        .resize_history
                        .iter()
                        .rev()
                        .nth(self.scroll)
                        .map(|r| r.at);
                    if let Some(at) = old_selected
                        && let Some(index) = snapshot
                            .resize_history
                            .iter()
                            .rev()
                            .position(|r| r.at == at)
                    {
                        self.scroll = index;
                    }
                }
                self.step_mib = snapshot.effective.step_mib;
                self.min_free_mib = snapshot.effective.min_free_mib;
                self.grow_below_mib = snapshot.effective.grow_below_mib;
                self.policy = snapshot.policy.clone();
                self.resize_pending = snapshot.pending;
                self.supervisor = Some(snapshot);
            }
            Event::Metrics(mut metrics) => {
                metrics.observe(&self.metrics.session);
                // The session began when the server did, not when this wrapper connected to it.
                let started = number(&metrics.status, "/started");
                if started > 0.0 {
                    self.started = Instant::now()
                        - std::time::Duration::from_secs_f64(
                            (epoch() - started).clamp(0., 2_592_000.),
                        );
                }
                let new_entry = self.metrics.last()["time"] != metrics.last()["time"];
                if new_entry
                    || self.age_updated.elapsed().as_secs() >= if self.age_seconds { 1 } else { 30 }
                {
                    self.age_now = epoch();
                    self.age_updated = Instant::now();
                }
                if self.scroll > 0 && self.view != View::Logs {
                    let selected = self
                        .metrics
                        .data
                        .pointer(&format!("/requests/{}/time", self.scroll))
                        .and_then(Value::as_f64);
                    if let Some(at) = selected
                        && let Some(rows) = metrics.data["requests"].as_array()
                        && let Some(index) =
                            rows.iter().position(|r| r["time"].as_f64() == Some(at))
                    {
                        self.scroll = index;
                    }
                }
                let name = text(&metrics.status, "/model");
                if !name.is_empty() {
                    self.model = name.to_owned();
                }
                let speed = if text(&metrics.data, "/live/state") == "generating" {
                    number(&metrics.data, "/live/tok_s")
                } else {
                    0.0
                };
                self.tps.push_back((speed * 10.0) as u64);
                RingLimit::truncate_front(&mut self.tps, 120);
                self.metrics = *metrics;
                self.connected = true;
                self.updated = Some(Instant::now());
            }
            Event::Hardware(hw) => {
                self.gpu_load.push_back(hw.gpu_util.unwrap_or(0) as u64);
                RingLimit::truncate_front(&mut self.gpu_load, 60);
                self.ram.push_back(hw.ram_used / 1048576);
                RingLimit::truncate_front(&mut self.ram, 60);
                self.hardware = *hw;
            }
            Event::Log { message, .. } => {
                if [
                    "starting the engine",
                    "loading the",
                    "experts loaded",
                    "filling the GPU",
                    "almost ready",
                    "still starting",
                ]
                .iter()
                .any(|s| message.contains(s))
                {
                    self.loading_note = sanitize(message.trim_start_matches("[basalt] "));
                }
                if self.scroll > 0 && self.view == View::Logs {
                    self.scroll += 1;
                }
                self.logs
                    .push_back(format!("{} {}", local_clock(), sanitize(&message)));
                RingLimit::truncate_front(&mut self.logs, 1000);
                if self.view == View::Logs {
                    self.scroll = self.scroll.min(self.logs.len().saturating_sub(1));
                }
            }
            Event::Policy {
                state,
                quiet_left,
                pending,
            } => {
                self.policy = state;
                self.quiet_left = quiet_left;
                self.resize_pending = pending;
            }
        }
    }
    pub fn list_len(&self) -> usize {
        match self.view {
            View::Logs => self.logs.len(),
            View::Policy => self
                .supervisor
                .as_ref()
                .map_or(0, |s| s.resize_history.len()),
            _ => self.metrics.data["requests"].as_array().map_or(0, Vec::len),
        }
    }
    pub fn refresh_ages(&mut self) {
        self.age_now = epoch();
        self.age_updated = Instant::now();
    }
    pub fn stale(&self) -> bool {
        !self.demo && self.updated.is_some_and(|t| t.elapsed().as_secs() >= 4)
    }
}

trait RingLimit {
    fn truncate_front(&mut self, max: usize);
}
impl<T> RingLimit for VecDeque<T> {
    fn truncate_front(&mut self, max: usize) {
        while self.len() > max {
            self.pop_front();
        }
    }
}

pub fn sanitize(text: &str) -> String {
    // Server logs are data, never terminal commands. Remove complete ANSI CSI
    // sequences as well as control characters before giving text to Ratatui.
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
        } else if !ch.is_control() {
            result.push(ch);
        }
    }
    result
}

fn local_clock() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut out = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r writes one tm into the supplied allocation and is
    // thread-safe. The result is read only after the non-null success check.
    unsafe {
        if libc::localtime_r(&seconds, out.as_mut_ptr()).is_null() {
            return "--:--:--".into();
        }
        let t = out.assume_init();
        format!("{:02}:{:02}:{:02}", t.tm_hour, t.tm_min, t.tm_sec)
    }
}

pub fn demo() -> App {
    let mut app = App::new("qwen3.8-flash-next-ud-iq3_s".into(), 8080);
    app.demo = true;
    app.pricing = Some(Pricing {
        prompt: 1.5e-7,
        completion: 4.7e-7,
        cache_read: 1.6e-8,
    });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let mut requests = vec![];
    for (i, speed) in [42.7, 38.2, 47.8, 41.3, 44.6, 39.1, 45.3, 43.5]
        .iter()
        .enumerate()
    {
        requests.push(json!({"id":format!("r_{:04}", 8 - i),"time":now-32.0-i as f64*72.0,
            "prompt_tokens":8640+i*912,"reused":6144,"output_tokens":1128+i*134,"decode_tok_s":speed,
            "duration_s":32.4+i as f64*3.0,"prompt_ms":2430.0,"hit_rate":0.684,"pcie_share":0.2,
            "finish":"stop","drafts_offered":1500,"drafts_accepted":1194}));
    }
    app.apply(Event::Metrics(Box::new(Metrics::new(json!({"engine":{"model":app.model,"max_context":196608,
        "expert_cache_mib":6144,"expert_slots":3095,"kv":"k8v4","kv_resident":20480,
        "spec":4,"version":"0.1.44.1","conversation_cache_mib":12288,"conversation_cache_slots":4},
        "live":{"state":"generating","prompt_tokens":18432,"generated":684,
            "tok_s":44.8,"elapsed_s":20.4,"queued":1,"prefill_tok_s_mean":3421.0,
            "slots":{"total":2,"active":1,"queue_depth":1}},
            "requests":requests,
            "conversation_cache":{"parked":3,"slots":6,"bytes":5_200_000_000u64,
                "budget_mib":16384,"restores":7,"evictions":1},
        "hardware":{"gpu_pcie_rx_mb":4782.0,"gpu_pcie_gen":5,"gpu_pcie_width":16,"disk_read_mb":148.2,"disk_write_mb":4.6}}),
        json!({"model":app.model,"loaded":true,"engine":"0.1.44.1","started":epoch() - 780.0,
            "activity":{"requests":9,"in_flight":1,"last_request_at":1780000360},
            "vram":{"elastic":true,"expert_cache_mib":6144,"expert_cache_full_mib":7680,"expert_slots":3095}}),
        json!([{"id":0,"state":"decoding","generated":684,"max_tokens":2048,"prompt_tokens":18432,
            "prefill_done":18432,"tok_s":44.8,"ctx_used":19116},
            {"id":1,"state":"idle","generated":0,"max_tokens":null,"prompt_tokens":0,
            "prefill_done":null,"tok_s":0.0,"ctx_used":0}])))));
    app.hardware = Hardware::demo();
    app.policy = "Holding · inference active".into();
    for i in 0..90 {
        app.tps
            .push_back((360.0 + (i as f64 * 0.4).sin() * 65.0 + (i % 9) as f64 * 4.0) as u64);
        app.gpu_load
            .push_back((68.0 + (i as f64 * 0.25).sin() * 19.0) as u64);
        app.ram.push_back(68400 + i * 12);
    }
    for message in [
        "Model loaded · elastic segments 512 MiB",
        "Request completed · 1,128 output tokens · 42.7 tok/s",
        "VRAM shrink confirmed · cache 7,168 → 6,656 MiB",
        "VRAM shrink confirmed · cache 6,656 → 6,144 MiB",
        "Request started · 18,432 input tokens · 12,288 cached",
    ] {
        app.logs.push_back(message.into());
    }
    use clap::Parser;
    let args = crate::Args::parse_from(["basalt-wrap", "iq3_s"]);
    let mut state = crate::control::Snapshot::new(&args, 8080);
    state.policy = app.policy.clone();
    state.metrics = Some(app.metrics.clone());
    state.hardware = Some(app.hardware.clone());
    for i in 0..120 {
        state
            .memory_history
            .push_back((1800 + ((i as f64 * 0.1).sin().abs() * 200.) as u64, 6144));
    }
    for (i, (before, after)) in [(7168, 6656), (6656, 6144)].into_iter().enumerate() {
        state
            .resize_history
            .push_back(crate::control::ResizeRecord {
                at: now - 180. + i as f64 * 60.,
                direction: "shrink".into(),
                target_free_mib: 1600,
                before_mib: before,
                after_mib: Some(after),
                free_mib: Some(1600),
                wait_seconds: 1.4,
                result: "applied".into(),
            });
    }
    app.supervisor = Some(Box::new(state));
    app
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefill_uses_actual_read_tokens_with_older_engine_fallback() {
        let mut request =
            json!({"prompt_tokens":10000,"reused":1000,"prompt_read":2000,"prompt_ms":500.0});
        assert_eq!(prefill_rate(&request), Some(4000.0));
        request["prompt_read"] = Value::Null;
        assert_eq!(prefill_rate(&request), Some(18000.0));
        request["prompt_ms"] = json!(0);
        assert_eq!(prefill_rate(&request), None);
        request["prompt_ms"] = json!(500);
        request["reused"] = json!(20000);
        assert_eq!(prefill_rate(&request), Some(0.0));
    }
    #[test]
    fn weighted_decode_rate_ignores_idle_and_prefill() {
        let mut m = Metrics::new(
            json!({"requests":[{"id":"r_0001","prompt_tokens":4000,"output_tokens":300,
                "decode_tok_s":30.0,"prompt_ms":90000}],"live":{"state":"idle"}}),
            json!({"started":1780000000,"activity":{"requests":1,"in_flight":0}}),
            Value::Null,
        );
        assert_eq!(m.totals().tps, 30.0);
        m.data["live"]["elapsed_s"] = json!(1000000);
        assert_eq!(m.totals().tps, 30.0);
        m.data["live"] =
            json!({"state":"generating","generated":100,"tok_s":50,"prompt_tokens":1000});
        assert!((m.totals().tps - 400.0 / 12.0).abs() < 0.001);
        assert_eq!(m.totals().input, 5000);
        assert_eq!(m.totals().output, 400);
    }
    #[test]
    fn removes_terminal_sequences_from_logs() {
        assert_eq!(sanitize("a\x1b[31mred\x1b[0m\r\x07"), "ared");
    }

    #[test]
    fn scrolled_logs_keep_their_anchor_when_output_arrives() {
        let mut app = demo();
        app.view = View::Logs;
        app.scroll = 2;
        let before = app.logs[app.logs.len() - app.scroll - 1].clone();
        app.apply(Event::Log {
            message: "a new event".into(),
            raw: false,
        });
        assert_eq!(app.logs[app.logs.len() - app.scroll - 1], before);
    }

    #[test]
    fn unloaded_model_does_not_display_old_expert_allocations() {
        let mut app = demo();
        assert!(app.metrics.cache() > 0);
        app.metrics.status["loaded"] = json!(false);
        assert_eq!(app.metrics.cache(), 0);
        assert_eq!(app.metrics.full_cache(), 0);
    }
    #[test]
    fn session_totals_span_polls_and_mark_what_was_never_seen() {
        let mut m = Metrics::new(
            json!({"requests":[
                {"id":"r_0002","prompt_tokens":2000,"output_tokens":200,"decode_tok_s":40.0},
                {"id":"r_0001","prompt_tokens":1000,"output_tokens":100,"decode_tok_s":20.0}],
                "live":{"state":"idle"}}),
            json!({"started":1780000000,"activity":{"requests":2,"in_flight":0}}),
            Value::Null,
        );
        assert_eq!(m.totals().requests, 2);
        assert_eq!(m.totals().input, 3000);
        assert_eq!(m.totals().output, 300);
        // 100 tokens at 20 tok/s and 200 at 40 tok/s: ten active decode seconds.
        assert!((m.totals().decode_seconds - 10.0).abs() < 0.001);
        assert!((m.totals().tps - 30.0).abs() < 0.001);
        assert!(!m.totals().partial);
        let previous = m.session.clone();
        m.observe(&previous);
        assert_eq!(m.totals().requests, 2);
        // A server that counted more than the wrapper's window shows as a floor.
        let late = Metrics::new(
            json!({"requests":[{"id":"r_0006","prompt_tokens":100,"output_tokens":50,
                "decode_tok_s":25.0}],"live":{"state":"idle"}}),
            json!({"started":1780000000,"activity":{"requests":6,"in_flight":0}}),
            Value::Null,
        );
        assert_eq!(late.totals().requests, 6);
        assert!(late.totals().partial);
        // The request numbers start over when the server does.
        m.status = json!({"started":1780000999,"activity":{"requests":1,"in_flight":0}});
        m.data["requests"] = json!([{"id":"r_0001","prompt_tokens":10,"output_tokens":10,
            "decode_tok_s":10.0}]);
        let previous = m.session.clone();
        m.observe(&previous);
        assert_eq!(m.totals().requests, 1);
        assert_eq!(m.totals().output, 10);
        assert!(!m.totals().partial);
    }
    #[test]
    fn basalt_rows_without_engine_timings_still_count_toward_the_session() {
        // The shape Basalt answers with: no totals block, a slots summary rather than rows,
        // and null timings when the engine has not reported them.
        let rows: Vec<Value> = (1..=3)
            .map(|i| {
                json!({"id":format!("r_{i:04}"),"path":"/v1/chat/completions","slot":0,
                    "time":1793098731.9,"duration_s":0.0,"finish":"length","prompt_tokens":246,
                    "reused":null,"output_tokens":8,"prompt_ms":null,"decode_tok_s":null,
                    "hit_rate":null,"pcie_share":null,"drafts_offered":null,
                    "drafts_accepted":null,"ttft_s":0.0,"draft_len":null})
            })
            .rev()
            .collect();
        let m = Metrics::new(
            json!({"requests":rows,"live":{"state":"idle","queued":0,
                "slots":{"total":1,"active":0,"queue_depth":0},"queue":[],"prompt_tokens":null,
                "generated":null,"elapsed_s":null,"tok_s":null,"prefill_tok_s_mean":null}}),
            json!({"service":"basalt","loaded":true,"started":1793098687,
                "activity":{"requests":3,"in_flight":0,"last_request_at":1793098731}}),
            json!([]),
        );
        let totals = m.totals();
        assert_eq!(totals.requests, 3);
        assert_eq!(totals.input, 738);
        assert_eq!(totals.output, 24);
        // No decode rate here: the average stays empty instead of inventing one.
        assert_eq!(totals.decode_seconds, 0.0);
        assert_eq!(totals.tps, 0.0);
        assert!(!totals.partial);
    }

    #[test]
    fn slot_rows_fill_the_progress_bars() {
        // Basalt keeps the prompt position and the token cap on `/slots`, not in `/metrics`.
        let mut m = Metrics::new(
            json!({"live":{"state":"reading","prompt_tokens":5264,"generated":null,"tok_s":null,
                "elapsed_s":2.4,"queued":0,"slots":{"total":1,"active":1,"queue_depth":0}}}),
            json!({"started":1793098687,"activity":{"requests":0,"in_flight":1}}),
            json!([{"id":0,"state":"prefill","prefill_done":2519,"prompt_tokens":5264,
                "generated":0,"max_tokens":4,"tok_s":0.0}]),
        );
        assert_eq!(m.prefill_progress(), Some((2519.0, 5264.0)));
        assert_eq!(m.decode_progress(), None);
        // A prompt being read is not decode time, but its tokens count.
        assert_eq!(m.totals().decode_seconds, 0.0);
        assert_eq!(m.totals().input, 5264);
        m.slots = json!([{"id":0,"state":"decoding","prefill_done":null,"prompt_tokens":5264,
            "generated":120,"max_tokens":2048,"tok_s":44.8}]);
        assert_eq!(m.prefill_progress(), None);
        assert_eq!(m.decode_progress(), Some((120.0, 2048.0)));
        assert!((m.totals().decode_seconds - 120.0 / 44.8).abs() < 0.001);
    }

    #[test]
    fn several_live_slots_count_once_toward_the_session() {
        let m = Metrics::new(
            json!({"live":{"state":"generating","generated":100,"tok_s":50,"prompt_tokens":1000,
                "queued":0,"slots":{"total":2,"active":2,"queue_depth":0}}}),
            json!({"started":1793098687,"activity":{"requests":0,"in_flight":2}}),
            json!([{"id":0,"state":"decoding","generated":100,"max_tokens":512,
                    "prompt_tokens":1000,"tok_s":50.0},
                {"id":1,"state":"decoding","generated":200,"max_tokens":512,
                    "prompt_tokens":2000,"tok_s":25.0}]),
        );
        assert_eq!(m.totals().output, 300);
        assert_eq!(m.totals().input, 3000);
        assert!((m.totals().decode_seconds - 10.0).abs() < 0.001);
        // The table covers every running request, so nothing is a floor here.
        assert!(!m.totals().partial);
    }

    #[test]
    fn session_cost_bills_cached_prompt_tokens_at_their_own_rate() {
        let pricing = Pricing {
            prompt: 2.0,
            completion: 4.0,
            cache_read: 1.0,
        };
        let totals = Totals {
            partial: false,
            input: 100,
            output: 50,
            reused: 60,
            requests: 1,
            decode_seconds: 0.0,
            tps: 0.0,
        };
        assert_eq!(pricing.cost(&totals), 40.0 * 2.0 + 60.0 * 1.0 + 50.0 * 4.0);
        // reused can never exceed what was read
        let totals = Totals {
            reused: 500,
            ..totals
        };
        assert_eq!(pricing.cost(&totals), 100.0 * 1.0 + 50.0 * 4.0);
    }

    #[test]
    fn pricing_comes_only_from_a_pricing_block() {
        assert!(pricing(&json!({"server": {}})).is_none());
        assert!(pricing(&json!({"pricing": null})).is_none());
        let p = pricing(&json!({"pricing": {"completion": 4.7e-7, "cache_write": 2e-7}})).unwrap();
        assert_eq!((p.prompt, p.cache_read), (0.0, 0.0));
        assert_eq!(p.completion, 4.7e-7);
    }

    #[test]
    fn the_session_clock_follows_the_server_not_the_wrapper() {
        // an attachment starts minutes after the server it joins
        let mut app = App::new("m".into(), 8080);
        app.apply(Event::Metrics(Box::new(Metrics::new(
            json!({"live": {"state": "idle"}, "requests": []}),
            json!({"started": epoch() - 120.0, "activity": {"requests": 0, "in_flight": 0}}),
            Value::Null,
        ))));
        let elapsed = app.started.elapsed().as_secs_f64();
        assert!((elapsed - 120.0).abs() < 5.0, "session showed {elapsed}s");
    }

    #[test]
    fn request_ages_only_refresh_on_entry_or_interval() {
        let mut app = demo();
        let metrics = app.metrics.clone();
        app.age_now = 100.;
        app.age_updated = Instant::now();
        app.apply(Event::Metrics(Box::new(metrics.clone())));
        assert_eq!(app.age_now, 100.);
        app.age_updated = Instant::now() - std::time::Duration::from_secs(31);
        app.apply(Event::Metrics(Box::new(metrics.clone())));
        assert!(app.age_now > 100.);
        app.age_now = 100.;
        let mut next = metrics;
        next.data["requests"][0]["time"] = json!(1.);
        app.apply(Event::Metrics(Box::new(next)));
        assert!(app.age_now > 100.);
    }
}
