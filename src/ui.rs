use crate::model::{App, View, number, text};
use anyhow::Result;
use ratatui::{
    Frame, Terminal,
    backend::TestBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, Gauge, Padding, Paragraph, Row, Sparkline, Table,
        TableState,
    },
};
use serde_json::json;

pub const BG: Color = Color::Rgb(15, 20, 30);
const CARD: Color = Color::Rgb(20, 28, 41);
const BORDER: Color = Color::Rgb(43, 57, 77);
const FG: Color = Color::Rgb(215, 225, 239);
const MUTED: Color = Color::Rgb(128, 147, 171);
const CYAN: Color = Color::Rgb(111, 215, 207);
const VIOLET: Color = Color::Rgb(176, 161, 241);
const GREEN: Color = Color::Rgb(159, 213, 170);
const AMBER: Color = Color::Rgb(237, 199, 139);
const RED: Color = Color::Rgb(238, 143, 153);
const TRACK: Color = Color::Rgb(35, 47, 65);

fn style(color: Color) -> Style {
    Style::default().fg(color)
}
fn dim(value: impl Into<String>) -> Span<'static> {
    Span::styled(value.into(), style(MUTED))
}
fn value(value: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(value.into(), style(color))
}
fn panel(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(style(BORDER))
        .padding(Padding::horizontal(1))
        .style(Style::default().bg(CARD).fg(FG))
        .title(Line::from(format!(" {} ", title.into())).style(style(MUTED)))
}
fn split(
    area: Rect,
    constraints: impl IntoIterator<Item = Constraint>,
    direction: Direction,
) -> Vec<Rect> {
    Layout::default()
        .direction(direction)
        .spacing(if direction == Direction::Horizontal {
            1
        } else {
            0
        })
        .constraints(constraints)
        .split(area)
        .to_vec()
}
fn line(f: &mut Frame, area: Rect, y: u16, text: impl Into<Line<'static>>) {
    if y < area.height {
        f.render_widget(
            Paragraph::new(text.into()),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
    }
}
fn inside(f: &mut Frame, area: Rect, title: impl Into<String>) -> Rect {
    let block = panel(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}
pub fn commas(n: u64) -> String {
    let chars = n.to_string();
    let mut out = String::new();
    for (i, c) in chars.chars().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
fn compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 10000 {
        format!("{:.1}k", n as f64 / 1000.)
    } else {
        commas(n)
    }
}
fn gib(n: u64) -> String {
    format!("{:.1}", n as f64 / 1073741824.)
}
fn opt_gib(n: Option<u64>) -> String {
    n.map(|n| format!("{} GiB", gib(n)))
        .unwrap_or_else(|| "—".into())
}
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.) as u64;
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, s % 3600 / 60)
    } else {
        format!("{}m {:02}s", s / 60, s % 60)
    }
}
fn gauge(f: &mut Frame, area: Rect, used: u64, total: u64, label: String, color: Color) {
    if area.height == 0 {
        return;
    }
    f.render_widget(
        Gauge::default()
            .gauge_style(style(color).bg(TRACK))
            .ratio(if total > 0 {
                (used as f64 / total as f64).clamp(0., 1.)
            } else {
                0.
            })
            .label(label)
            .use_unicode(true),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

pub fn process_height(height: u16) -> u16 {
    let cap = (height / 4).min(12);
    if cap >= 6 { cap } else { 0 }
}

pub fn visible_rows(app: &App, height: u16) -> usize {
    let process = process_height(height).min(app.hardware.processes.len().max(3) as u16 + 3);
    let body = height.saturating_sub(2 + 4 + 3 + resource_height(app, height) + process + 1);
    let reserved = if app.view == View::Requests {
        if body >= 12 { 8 } else { 3 }
    } else {
        2
    };
    body.saturating_sub(reserved).max(1) as usize
}

fn resource_height(app: &App, height: u16) -> u16 {
    if app.view != View::Overview {
        if height >= 36 { 5 } else { 0 }
    } else if height >= 32 {
        7
    } else {
        5
    }
}

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);
    if area.width < 50 || area.height < 14 {
        small(f, app, area);
        return;
    }
    let process = process_height(area.height).min(app.hardware.processes.len().max(3) as u16 + 3);
    let rows = split(
        area,
        [
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Length(resource_height(app, area.height)),
            Constraint::Min(1),
            Constraint::Length(process),
            Constraint::Length(1),
        ],
        Direction::Vertical,
    );
    header(f, app, rows[0]);
    stats(f, app, rows[1]);
    activity(f, app, rows[2]);
    resources(f, app, rows[3]);
    match app.view {
        View::Overview => overview(f, app, rows[4]),
        View::Requests => requests(f, app, rows[4], true),
        View::Logs => logs(f, app, rows[4]),
    }
    if process > 0 {
        processes(f, app, rows[5]);
    }
    if area.width < 90 {
        line(
            f,
            rows[6],
            0,
            Line::from(vec![
                dim(" 1/2/3 views  hjkl  p GPU  ?"),
                value(
                    if app.stopping {
                        "  Stopping…"
                    } else {
                        "  q quit"
                    },
                    if app.stopping { AMBER } else { CYAN },
                ),
            ]),
        );
    } else {
        line(
            f,
            rows[6],
            0,
            Line::from(vec![
                dim(" "),
                value("1", CYAN),
                dim(" Overview  "),
                value("2", CYAN),
                dim(" Requests  "),
                value("3", CYAN),
                dim(" Logs"),
                dim(if area.width >= 94 {
                    "   h/l views  j/k select  p GPU  ? help"
                } else {
                    "   hjkl navigate  ? help"
                }),
                value(
                    if app.stopping {
                        "   Stopping…"
                    } else {
                        "   q quit"
                    },
                    if app.stopping { AMBER } else { MUTED },
                ),
            ]),
        );
    }
    if app.help {
        help(f, area);
    }
}

fn header(f: &mut Frame, app: &App, area: Rect) {
    let badge = if app.stopping {
        "STOPPING"
    } else if app.stale() {
        "RECONNECTING"
    } else if !app.connected {
        "LOADING"
    } else {
        match text(&app.metrics.data, "/live/state") {
            "generating" => "GENERATING",
            "reading" => "READING",
            "unloaded" => "UNLOADED",
            _ => "READY",
        }
    };
    let color = if app.stopping || app.stale() {
        AMBER
    } else if app.metrics.busy() {
        CYAN
    } else {
        GREEN
    };
    let top = split(
        area,
        [
            Constraint::Min(10),
            Constraint::Length(badge.len() as u16 + 4),
        ],
        Direction::Horizontal,
    );
    line(
        f,
        top[0],
        0,
        Line::from(vec![
            value(" STRATA", CYAN).add_modifier(Modifier::BOLD),
            dim("  /  "),
            value(app.model.clone(), FG),
        ]),
    );
    line(
        f,
        top[1],
        0,
        Line::from(vec![value("● ", color), value(badge, color)]),
    );
    let engine = text(&app.metrics.status, "/engine");
    line(
        f,
        area,
        1,
        Line::from(vec![
            dim(format!(
                " {}{}",
                app.address,
                if app.demo { "   DEMO" } else { "" }
            )),
            dim(format!(
                "   {}",
                if engine.is_empty() {
                    "starting engine"
                } else {
                    engine
                }
            )),
            dim(format!(
                "   session {}",
                clock(app.started.elapsed().as_secs_f64())
            )),
        ]),
    );
}

fn stats(f: &mut Frame, app: &App, area: Rect) {
    let total = app.metrics.totals();
    let live = text(&app.metrics.data, "/live/state") == "generating";
    let rate = if live {
        number(&app.metrics.data, "/live/tok_s")
    } else {
        number(app.metrics.last(), "/decode_tok_s")
    };
    let areas = split(area, [Constraint::Percentage(25); 4], Direction::Horizontal);
    let cards = [
        (
            "Decode",
            format!("{rate:.1} tok/s"),
            if live { "live output" } else { "last request" }.to_string(),
            CYAN,
        ),
        (
            "Session avg",
            format!("{:.1} tok/s", total.tps),
            format!("{} decoding", clock(total.decode_seconds)),
            VIOLET,
        ),
        (
            "Input tokens",
            commas(total.input),
            format!("{} reused", compact(total.reused)),
            FG,
        ),
        (
            "Output tokens",
            commas(total.output),
            format!("{} requests", total.requests),
            GREEN,
        ),
    ];
    for (i, (title, big, sub, color)) in cards.into_iter().enumerate() {
        let inner = inside(f, areas[i], title);
        line(
            f,
            inner,
            0,
            Line::from(value(big, color).add_modifier(Modifier::BOLD)),
        );
        line(f, inner, 1, Line::from(dim(sub)));
    }
}

fn activity(f: &mut Frame, app: &App, area: Rect) {
    let inner = inside(f, area, "Now");
    let data = &app.metrics.data;
    let state = text(data, "/live/state");
    let label = if app.stopping {
        "Stopping the server and its engine…".into()
    } else if app.stale() {
        "Monitoring connection interrupted · management waits for fresh data".into()
    } else if !app.connected {
        app.loading_note.clone()
    } else if state == "reading" {
        format!(
            "Reading prompt  {} / {} tokens  ·  {:.1}s",
            commas(number(data, "/live/prompt_read") as u64),
            commas(number(data, "/live/prompt_total") as u64),
            number(data, "/live/elapsed_s")
        )
    } else if state == "generating" {
        format!(
            "Generating  {} tokens  ·  {:.1} tok/s  ·  {:.1}s  ·  {} queued",
            commas(number(data, "/live/generated") as u64),
            number(data, "/live/tok_s"),
            number(data, "/live/elapsed_s"),
            number(data, "/live/queued")
        )
    } else if state == "unloaded" {
        "Model unloaded · waiting for a request".into()
    } else {
        "Ready for the next request".into()
    };
    let (used, total) = if state == "reading" {
        (
            number(data, "/live/prompt_read") as u64,
            number(data, "/live/prompt_total") as u64,
        )
    } else if state == "generating" {
        (
            number(data, "/live/generated") as u64,
            number(data, "/live/max_tokens") as u64,
        )
    } else {
        (0, 1)
    };
    gauge(
        f,
        inner,
        used,
        total,
        label,
        if state == "reading" { VIOLET } else { CYAN },
    );
}

fn resources(f: &mut Frame, app: &App, area: Rect) {
    let cols = split(area, [Constraint::Percentage(50); 2], Direction::Horizontal);
    let hw = &app.hardware;
    let left = inside(
        f,
        cols[0],
        if hw.gpu_name.is_empty() {
            "GPU".into()
        } else {
            hw.gpu_name.replace("NVIDIA GeForce ", "")
        },
    );
    let danger = hw.gpu_free < 1024 * 1048576 && hw.gpu_total > 0;
    gauge(
        f,
        left,
        hw.gpu_used,
        hw.gpu_total,
        if left.width < 44 {
            format!(
                "VRAM {}/{}G · {}G free",
                gib(hw.gpu_used),
                gib(hw.gpu_total),
                gib(hw.gpu_free)
            )
        } else {
            format!(
                "VRAM {} / {} GiB   {} free",
                gib(hw.gpu_used),
                gib(hw.gpu_total),
                gib(hw.gpu_free)
            )
        },
        if danger { RED } else { CYAN },
    );
    line(
        f,
        left,
        1,
        Line::from(vec![
            dim("Load "),
            value(
                hw.gpu_util
                    .map(|v| format!("{v}%"))
                    .unwrap_or_else(|| "—".into()),
                FG,
            ),
            dim("   Temp "),
            value(
                hw.gpu_temp
                    .map(|v| format!("{v}°C"))
                    .unwrap_or_else(|| "—".into()),
                if hw.gpu_temp.unwrap_or(0) >= 80 {
                    AMBER
                } else {
                    FG
                },
            ),
            dim("   Power "),
            value(
                hw.gpu_power
                    .map(|v| format!("{v:.0}W"))
                    .unwrap_or_else(|| "—".into()),
                FG,
            ),
        ]),
    );
    line(
        f,
        left,
        2,
        Line::from(vec![
            dim(format!(
                "PCIe {}×{}",
                hw.pcie_gen.unwrap_or(0),
                hw.pcie_width.unwrap_or(0)
            )),
            value(format!("  {:.0} MiB/s → GPU", hw.pcie_rx.unwrap_or(0.)), FG),
        ]),
    );
    if left.height >= 5 {
        let points = app.gpu_load.iter().copied().collect::<Vec<_>>();
        f.render_widget(
            Sparkline::default()
                .data(&points)
                .max(100)
                .style(style(CYAN)),
            Rect::new(left.x, left.y + 3, left.width, 1),
        );
        let owned = hw
            .processes
            .iter()
            .filter(|p| p.owned)
            .filter_map(|p| p.vram)
            .sum::<u64>();
        line(
            f,
            left,
            4,
            Line::from(dim(if let Some(error) = &hw.gpu_error {
                error.clone()
            } else {
                format!(
                    "Strata {} GiB   ·   other / driver {} GiB",
                    gib(owned),
                    gib(hw.gpu_used.saturating_sub(owned))
                )
            })),
        );
    }
    let right = inside(f, cols[1], "System memory & CPU");
    gauge(
        f,
        right,
        hw.ram_used,
        hw.ram_total,
        if right.width < 44 {
            format!(
                "RAM {}/{}G · {}G free",
                gib(hw.ram_used),
                gib(hw.ram_total),
                gib(hw.ram_available)
            )
        } else {
            format!(
                "RAM {} / {} GiB   {} available",
                gib(hw.ram_used),
                gib(hw.ram_total),
                gib(hw.ram_available)
            )
        },
        if hw.ram_available < hw.ram_total / 12 {
            AMBER
        } else {
            VIOLET
        },
    );
    line(
        f,
        right,
        1,
        Line::from(vec![
            dim("Model PSS "),
            value(opt_gib(hw.model_pss), FG),
            dim("   locked "),
            value(opt_gib(hw.model_locked), FG),
        ]),
    );
    line(
        f,
        right,
        2,
        Line::from(vec![
            dim(format!("CPU {:.0}%", hw.cpu)),
            value(
                format!(
                    "  read {:.0} · write {:.1} MiB/s",
                    hw.disk_read, hw.disk_write
                ),
                FG,
            ),
        ]),
    );
    if right.height >= 5 {
        line(
            f,
            right,
            3,
            Line::from(dim(format!(
                "Anon {}G · file {}G · shared {}G",
                hw.model_anon.map(gib).unwrap_or_else(|| "—".into()),
                hw.model_file.map(gib).unwrap_or_else(|| "—".into()),
                hw.model_shared.map(gib).unwrap_or_else(|| "—".into())
            ))),
        );
        gauge(
            f,
            Rect::new(right.x, right.y + 4, right.width, 1),
            hw.swap_used,
            hw.swap_total,
            format!("Swap {} / {} GiB", gib(hw.swap_used), gib(hw.swap_total)),
            VIOLET,
        );
    }
}

fn overview(f: &mut Frame, app: &App, area: Rect) {
    if area.height < 5 {
        let inner = inside(f, area, "Context & expert cache");
        line(
            f,
            inner,
            0,
            Line::from(dim(format!(
                "Context {}/{} · cache {:.1}/{:.1} GiB · {}",
                compact(number(&app.metrics.data, "/live/prompt_tokens") as u64),
                compact(number(&app.metrics.data, "/engine/max_context") as u64),
                app.metrics.cache() as f64 / 1024.,
                app.metrics.full_cache() as f64 / 1024.,
                app.policy
            ))),
        );
        return;
    }
    if area.width < 70 {
        runtime(f, app, area);
        return;
    }
    let cols = split(
        area,
        [Constraint::Percentage(64), Constraint::Percentage(36)],
        Direction::Horizontal,
    );
    if area.height >= 12 {
        let rows = split(
            cols[0],
            [Constraint::Length(5), Constraint::Min(3)],
            Direction::Vertical,
        );
        let inner = inside(f, rows[0], "Decode rhythm · last minute");
        let points = app
            .tps
            .iter()
            .rev()
            .take(inner.width as usize)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        f.render_widget(
            Sparkline::default()
                .data(&points)
                .max(points.iter().copied().max().unwrap_or(500).max(100))
                .style(style(CYAN)),
            inner,
        );
        requests(f, app, rows[1], false);
    } else {
        requests(f, app, cols[0], false);
    }
    runtime(f, app, cols[1]);
}

fn runtime(f: &mut Frame, app: &App, area: Rect) {
    let inner = inside(f, area, "Context & elastic cache");
    let m = &app.metrics;
    let data = &m.data;
    let context = number(data, "/engine/max_context") as u64;
    let active = m.busy();
    let used = if active {
        number(data, "/live/prompt_tokens") + number(data, "/live/generated")
    } else {
        number(m.last(), "/prompt_tokens") + number(m.last(), "/output_tokens")
    };
    gauge(
        f,
        inner,
        used as u64,
        context,
        format!("Context {} / {}", compact(used as u64), compact(context)),
        CYAN,
    );
    line(
        f,
        inner,
        1,
        Line::from(dim(format!(
            "KV {} · {} resident",
            text(data, "/engine/kv"),
            compact(number(data, "/engine/kv_resident") as u64)
        ))),
    );
    if inner.height > 2 {
        gauge(
            f,
            Rect::new(inner.x, inner.y + 2, inner.width, 1),
            m.cache(),
            m.full_cache(),
            format!(
                "Experts {:.1} / {:.1} GiB",
                m.cache() as f64 / 1024.,
                m.full_cache() as f64 / 1024.
            ),
            VIOLET,
        );
    }
    line(
        f,
        inner,
        3,
        Line::from(dim(format!(
            "{} expert slots · 512 MiB steps",
            commas(number(data, "/engine/expert_slots") as u64)
        ))),
    );
    line(
        f,
        inner,
        4,
        Line::from(value(
            app.policy.clone(),
            if app.resize_pending { AMBER } else { CYAN },
        )),
    );
    line(
        f,
        inner,
        5,
        Line::from(dim(format!(
            "Reserve ≥{:.1} GiB · grow below {:.0} GiB",
            app.min_free_mib as f64 / 1024.0,
            app.grow_below_mib as f64 / 1024.0
        ))),
    );
    let last = m.last();
    let offered = number(last, "/drafts_offered");
    line(
        f,
        inner,
        7,
        Line::from(vec![
            dim("MTP acceptance "),
            value(
                if offered > 0. {
                    format!("{:.1}%", number(last, "/drafts_accepted") / offered * 100.)
                } else {
                    "—".into()
                },
                GREEN,
            ),
        ]),
    );
    line(
        f,
        inner,
        8,
        Line::from(vec![
            dim("Prefill "),
            value(
                format!(
                    "{:.0} tok/s",
                    if active {
                        number(data, "/live/prefill_tok_s_mean")
                    } else {
                        let ms = number(last, "/prompt_ms");
                        if ms > 0. {
                            (number(last, "/prompt_tokens") - number(last, "/reused")).max(0.)
                                * 1000.
                                / ms
                        } else {
                            0.
                        }
                    }
                ),
                FG,
            ),
        ]),
    );
    let cache = &data["conversation_cache"];
    line(
        f,
        inner,
        10,
        Line::from(dim(format!(
            "Conversation cache · {} / {} parked",
            number(cache, "/parked"),
            number(cache, "/slots")
        ))),
    );
    line(
        f,
        inner,
        11,
        Line::from(vec![
            value(
                format!(
                    "{} / {:.1} GiB",
                    gib(number(cache, "/bytes") as u64),
                    number(cache, "/budget_mib") / 1024.
                ),
                FG,
            ),
            dim(format!(" · {} restores", number(cache, "/restores"))),
        ]),
    );
    line(
        f,
        inner,
        12,
        Line::from(dim(format!(
            "{} reused · {} evictions",
            compact(m.totals().reused),
            number(cache, "/evictions")
        ))),
    );
    line(
        f,
        inner,
        14,
        Line::from(dim(format!(
            "Model group {} charged",
            opt_gib(app.hardware.model_cgroup)
        ))),
    );
    line(
        f,
        inner,
        15,
        Line::from(dim(format!("{} CPU threads", app.hardware.threads))),
    );
}

fn requests(f: &mut Frame, app: &App, area: Rect, detail: bool) {
    let rows = if detail && area.height >= 12 {
        split(
            area,
            [Constraint::Min(6), Constraint::Length(5)],
            Direction::Vertical,
        )
    } else {
        vec![area]
    };
    let requests = app.metrics.data["requests"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let wide = area.width >= 86;
    let header = if wide {
        vec![
            "Age",
            "Input",
            "Reused",
            "Output",
            "Prefill/s",
            "Decode/s",
            "Hit",
            "Wall",
            "Result",
        ]
    } else {
        vec!["Age", "Input", "Output", "tok/s", "Result"]
    };
    let latest = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let records = requests
        .iter()
        .map(|r| {
            let status = text(r, "/finish");
            let color = if matches!(status, "error" | "cancel" | "disconnect") {
                AMBER
            } else {
                GREEN
            };
            let age = clock((latest - number(r, "/time")).max(0.));
            let mut cells = vec![
                Cell::from(age).style(style(MUTED)),
                Cell::from(compact(number(r, "/prompt_tokens") as u64)),
            ];
            if wide {
                cells.push(Cell::from(compact(number(r, "/reused") as u64)));
            }
            cells.push(Cell::from(compact(number(r, "/output_tokens") as u64)));
            if wide {
                let ms = number(r, "/prompt_ms");
                cells.push(Cell::from(if ms > 0. {
                    format!(
                        "{:.0}",
                        (number(r, "/prompt_tokens") - number(r, "/reused")).max(0.) * 1000. / ms
                    )
                } else {
                    "—".into()
                }));
            }
            cells.push(Cell::from(format!("{:.1}", number(r, "/decode_tok_s"))).style(style(CYAN)));
            if wide {
                cells.push(Cell::from(format!("{:.0}%", number(r, "/hit_rate") * 100.)));
                cells.push(Cell::from(format!("{:.1}s", number(r, "/duration_s"))));
            }
            cells.push(Cell::from(status.to_string()).style(style(color)));
            Row::new(cells)
        })
        .collect::<Vec<_>>();
    let widths = if wide {
        vec![
            Constraint::Length(8),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(10),
        ]
    } else {
        vec![
            Constraint::Length(8),
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(10),
        ]
    };
    let table = Table::new(records, widths)
        .header(Row::new(header).style(style(MUTED)).height(1))
        .block(panel(if detail {
            "Request history · j/k select"
        } else {
            "Recent requests · Enter details"
        }))
        .column_spacing(1)
        .row_highlight_style(Style::default().bg(TRACK))
        .highlight_symbol(if detail { "› " } else { "" });
    let mut state = TableState::default();
    if !requests.is_empty() {
        state.select(Some(app.scroll.min(requests.len() - 1)));
    }
    f.render_stateful_widget(table, rows[0], &mut state);
    if requests.is_empty() && rows[0].height >= 4 {
        line(
            f,
            rows[0].inner(ratatui::layout::Margin::new(2, 2)),
            0,
            Line::from(dim("No requests yet")),
        );
    }
    if rows.len() > 1 {
        let inner = inside(f, rows[1], "Selected request");
        if let Some(r) = requests.get(app.scroll.min(requests.len().saturating_sub(1))) {
            line(
                f,
                inner,
                0,
                Line::from(vec![
                    dim("Input "),
                    value(commas(number(r, "/prompt_tokens") as u64), FG),
                    dim("   cached "),
                    value(commas(number(r, "/reused") as u64), VIOLET),
                    dim("   output "),
                    value(commas(number(r, "/output_tokens") as u64), GREEN),
                ]),
            );
            line(
                f,
                inner,
                1,
                Line::from(dim(format!(
                    "Prefill {:.2}s · decode {:.2}s · total {:.1}s · {:.1} tok/s",
                    number(r, "/prompt_ms") / 1000.,
                    number(r, "/decode_ms") / 1000.,
                    number(r, "/duration_s"),
                    number(r, "/decode_tok_s")
                ))),
            );
            line(
                f,
                inner,
                2,
                Line::from(dim(format!(
                    "MTP {:.0}/{:.0} accepted · expert hit {:.1}% · PCIe share {:.1}%",
                    number(r, "/drafts_accepted"),
                    number(r, "/drafts_offered"),
                    number(r, "/hit_rate") * 100.,
                    number(r, "/pcie_share") * 100.
                ))),
            );
        }
    }
}

fn processes(f: &mut Frame, app: &App, area: Rect) {
    let rows = &app.hardware.processes;
    let capacity = area.height.saturating_sub(3) as usize;
    let title = format!(
        "GPU processes · {}{}",
        rows.len(),
        if rows.len() > capacity {
            format!(" · showing {capacity}")
        } else {
            String::new()
        }
    );
    let data = rows
        .iter()
        .skip(app.process_offset)
        .take(capacity)
        .map(|p| {
            let color = if p.owned { CYAN } else { FG };
            let memory = p
                .vram
                .map(|v| format!("{} MiB", commas(v >> 20)))
                .unwrap_or_else(|| "unavailable".into());
            Row::new(vec![
                Cell::from(p.pid.to_string()).style(style(MUTED)),
                Cell::from(format!("{}{}", if p.owned { "● " } else { "  " }, p.name))
                    .style(style(color)),
                Cell::from(p.kind.clone()).style(style(MUTED)),
                Cell::from(memory).style(style(color)),
                Cell::from(
                    p.vram
                        .map(|v| {
                            format!(
                                "{:.1}%",
                                v as f64 / app.hardware.gpu_total.max(1) as f64 * 100.
                            )
                        })
                        .unwrap_or_else(|| "—".into()),
                )
                .style(style(MUTED)),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Table::new(
            data,
            [
                Constraint::Length(8),
                Constraint::Fill(1),
                Constraint::Length(5),
                Constraint::Length(13),
                Constraint::Length(7),
            ],
        )
        .header(Row::new(["PID", "Process", "Type", "VRAM", "Share"]).style(style(MUTED)))
        .column_spacing(2)
        .block(
            panel(format!(
                "{title}{}",
                if app.process_focus {
                    " · j/k scroll · Esc back"
                } else {
                    " · p focus"
                }
            ))
            .border_style(style(if app.process_focus { CYAN } else { BORDER })),
        ),
        area,
    );
    if let Some(error) = &app.hardware.process_error {
        line(
            f,
            area.inner(ratatui::layout::Margin::new(2, 2)),
            0,
            Line::from(value(error.clone(), AMBER)),
        );
    }
}

fn logs(f: &mut Frame, app: &App, area: Rect) {
    let inner = inside(
        f,
        area,
        if app.scroll == 0 {
            "Event log · following"
        } else {
            "Event log · scrolled · End to follow"
        },
    );
    let take = inner.height as usize;
    let end = app
        .logs
        .len()
        .saturating_sub(app.scroll.min(app.logs.len()));
    let start = end.saturating_sub(take);
    let lines = app
        .logs
        .range(start..end)
        .map(|s| {
            Line::from(Span::styled(
                s.clone(),
                style(
                    if s.to_lowercase().contains("error") || s.to_lowercase().contains("failed") {
                        RED
                    } else if s.contains("VRAM") {
                        CYAN
                    } else {
                        MUTED
                    },
                ),
            ))
        })
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(lines), inner);
}

fn small(f: &mut Frame, app: &App, area: Rect) {
    let t = app.metrics.totals();
    let lines = vec![
        Line::from(value(" STRATA", CYAN)),
        Line::from(app.model.clone()),
        Line::from(format!(
            "Session {:.1} tok/s · {} output",
            t.tps,
            compact(t.output)
        )),
        Line::from(format!(
            "{} input · RAM {} GiB",
            compact(t.input),
            gib(app.hardware.ram_used)
        )),
        Line::from(dim("Resize to 50×14 for the dashboard")),
        Line::from(dim("q quit · ? help")),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn help(f: &mut Frame, area: Rect) {
    let width = area.width.saturating_sub(4).min(80);
    let height = area.height.saturating_sub(2).min(22);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);
    let inner = inside(f, rect, "Strata · keyboard & metrics");
    let lines = [
        "1 / 2 / 3 / Tab    Overview / request history / logs",
        "h / l / ← →        Previous / next view",
        "j / k / ↓ ↑        Select requests or scroll logs",
        "Ctrl+D / Ctrl+U    Half-page down / up",
        "Ctrl+F / Ctrl+B    Page down / up (also PageDown / PageUp)",
        "gg / G             First / last request; oldest / newest log",
        "Enter              Open the selected request's details",
        "p / Esc            Focus GPU processes / return; j/k scroll",
        "b                   Open Strata's web dashboard",
        "? / Esc             Close this help",
        "q / Ctrl+C          Stop Strata and exit (again to force)",
        "",
        "Session avg = output tokens / active decode seconds.",
        "Idle and prompt processing do not dilute it. Live output is included.",
        "Input counts include cached tokens; reused tokens are shown separately.",
        "Model PSS shares shared pages proportionally across processes.",
        "Locked memory is part of model memory, not an extra allocation.",
        "GPU processes include graphics and compute, deduplicated by PID.",
        "VRAM resizing waits behind inference; the reserve is best effort.",
    ];
    f.render_widget(
        Paragraph::new(Text::from(
            lines
                .into_iter()
                .map(|l| Line::from(l.to_string()))
                .collect::<Vec<_>>(),
        ))
        .style(style(FG)),
        inner,
    );
}

pub fn snapshot(app: &App, width: u16, height: u16, cells: bool) -> Result<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|f| render(f, app))?;
    let buffer = terminal.backend().buffer();
    if cells {
        let mut output = vec![];
        for y in 0..height {
            for x in 0..width {
                let c = &buffer[(x, y)];
                output.push(json!({"x":x,"y":y,"text":c.symbol(),"fg":rgb(c.fg,FG),"bg":rgb(c.bg,BG),"bold":c.modifier.contains(Modifier::BOLD)}));
            }
        }
        return Ok(serde_json::to_string(
            &json!({"width":width,"height":height,"cells":output}),
        )?);
    }
    let mut output = String::new();
    for y in 0..height {
        let mut row = String::new();
        for x in 0..width {
            row.push_str(buffer[(x, y)].symbol());
        }
        output.push_str(row.trim_end());
        output.push('\n');
    }
    Ok(output)
}
fn rgb(color: Color, fallback: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => match fallback {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "#ffffff".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_panel_never_exceeds_quarter_and_requires_three_rows() {
        for h in 1..250 {
            let p = process_height(h);
            assert!(p <= h / 4);
            assert!(p == 0 || p >= 6);
        }
    }
    #[test]
    fn every_terminal_size_renders_without_panicking() {
        let mut app = crate::model::demo();
        for view in [View::Overview, View::Requests, View::Logs] {
            app.view = view;
            for w in [0, 20, 49, 50, 60, 80, 100, 140] {
                for h in [0, 5, 13, 14, 20, 23, 24, 32, 45, 60] {
                    snapshot(&app, w, h, false).unwrap();
                }
            }
        }
    }
    #[test]
    fn compact_layout_hides_processes() {
        let app = crate::model::demo();
        assert!(
            !snapshot(&app, 100, 23, false)
                .unwrap()
                .contains("GPU processes")
        );
        assert!(
            snapshot(&app, 100, 32, false)
                .unwrap()
                .contains("GPU processes")
        );
    }

    #[test]
    fn small_request_and_log_views_keep_room_for_content() {
        let mut app = crate::model::demo();
        app.view = View::Requests;
        assert!(snapshot(&app, 80, 24, false).unwrap().contains("1,128"));
        app.view = View::Logs;
        assert!(
            snapshot(&app, 80, 24, false)
                .unwrap()
                .contains("Request started")
        );
    }
    #[test]
    fn dashboard_includes_requested_session_and_memory_values() {
        let text = snapshot(&crate::model::demo(), 140, 48, false).unwrap();
        for label in [
            "Session avg",
            "Input tokens",
            "Output tokens",
            "Model PSS",
            "locked",
            "GPU processes",
            "RuneLite",
            "Context",
        ] {
            assert!(text.contains(label), "missing {label}");
        }
    }
}
