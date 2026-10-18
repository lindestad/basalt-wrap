use crate::hardware::Hardware;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Instant};

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub data: Value,
    pub status: Value,
}

impl Metrics {
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
    pub fn totals(&self) -> Totals {
        let active = self.busy();
        let generated = if active {
            number(&self.data, "/live/generated")
        } else {
            0.0
        };
        let mean = number(&self.data, "/live/tok_s_mean");
        let output = number(&self.data, "/totals/output_tokens");
        let seconds = number(&self.data, "/totals/decode_ms") / 1000.0;
        let live_seconds = if generated > 0.0 && mean > 0.0 {
            generated / mean
        } else {
            0.0
        };
        let rate_tokens = output + if live_seconds > 0.0 { generated } else { 0.0 };
        Totals {
            input: number(&self.data, "/totals/prompt_tokens") as u64
                + if active {
                    number(&self.data, "/live/prompt_tokens") as u64
                } else {
                    0
                },
            output: (output + generated) as u64,
            reused: number(&self.data, "/totals/reused") as u64,
            requests: number(&self.data, "/totals/requests") as u64,
            decode_seconds: seconds + live_seconds,
            tps: if seconds + live_seconds > 0.0 {
                rate_tokens / (seconds + live_seconds)
            } else {
                0.0
            },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Totals {
    pub input: u64,
    pub output: u64,
    pub reused: u64,
    pub requests: u64,
    pub decode_seconds: f64,
    pub tps: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    Metrics(Metrics),
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
}

pub struct App {
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
}

impl App {
    pub fn new(model: String, port: u16) -> Self {
        Self {
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
        }
    }
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Metrics(metrics) => {
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
                self.tps.truncate_front(120);
                self.metrics = metrics;
                self.connected = true;
                self.updated = Some(Instant::now());
            }
            Event::Hardware(hw) => {
                self.gpu_load.push_back(hw.gpu_util.unwrap_or(0) as u64);
                self.gpu_load.truncate_front(60);
                self.ram.push_back(hw.ram_used / 1048576);
                self.ram.truncate_front(60);
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
                    self.loading_note = sanitize(message.trim_start_matches("[strata] "));
                }
                if self.scroll > 0 && self.view == View::Logs {
                    self.scroll += 1;
                }
                self.logs
                    .push_back(format!("{} {}", local_clock(), sanitize(&message)));
                self.logs.truncate_front(1000);
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let mut requests = vec![];
    for (i, speed) in [42.7, 38.2, 47.8, 41.3, 44.6, 39.1, 45.3, 43.5]
        .iter()
        .enumerate()
    {
        requests.push(json!({"time":now-32.0-i as f64*72.0,"prompt_tokens":8640+i*912,"reused":6144,
            "output_tokens":1128+i*134,"decode_tok_s":speed,"duration_s":32.4+i as f64*3.0,
            "prompt_ms":2430.0,"decode_ms":26300.0,"hit_rate":0.684,"pcie_share":0.2,"finish":"stop",
            "drafts_offered":1500,"drafts_accepted":1194}));
    }
    app.apply(Event::Metrics(Metrics { data: json!({"engine":{"model":app.model,"max_context":196608,
        "expert_cache_mib":6144,"expert_slots":3095,"kv":"k8v4","kv_resident":20480,
        "spec":4,"version":"0.1.44.1","conversation_cache_mib":12288,"conversation_cache_slots":4},
        "live":{"state":"generating","phase":"decoding","prompt_tokens":18432,"generated":684,
            "max_tokens":2048,"tok_s":44.8,"tok_s_mean":43.9,"elapsed_s":20.4,"queued":1,"prefill_tok_s_mean":3421.0},
        "requests":requests,"totals":{"requests":24,"prompt_tokens":284912,"reused":196608,
            "output_tokens":32768,"decode_ms":756911.0,"prompt_ms":48210.0},
        "conversation_cache":{"enabled":true,"budget_mib":12288,"slots":4,"parked":3,"reused_tokens":196608},
        "hardware":{"gpu_pcie_rx_mb":4782.0,"gpu_pcie_gen":5,"gpu_pcie_width":16,"disk_read_mb":148.2,"disk_write_mb":4.6}}),
        status: json!({"model":app.model,"loaded":true,"engine":"0.1.44.1","started":1780000000,
            "activity":{"requests":25,"in_flight":2,"last_request_at":1780000360},
            "vram":{"elastic":true,"expert_cache_mib":6144,"expert_cache_full_mib":7680,"expert_slots":3095}}) }));
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
    app
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weighted_decode_rate_ignores_idle_and_prefill() {
        let mut m = Metrics {
            data: json!({"totals":{"prompt_tokens":4000,"output_tokens":300,"decode_ms":10000,"prompt_ms":90000},
            "live":{"state":"idle"}}),
            status: Value::Null,
        };
        assert_eq!(m.totals().tps, 30.0);
        m.data["live"]["elapsed_s"] = json!(1000000);
        assert_eq!(m.totals().tps, 30.0);
        m.data["live"] =
            json!({"state":"generating","generated":100,"tok_s_mean":50,"prompt_tokens":1000});
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
}
