use crate::{
    Args,
    model::{self, App, Event, View, number, text},
    runtime, ui,
};
use anyhow::Result;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::{
    io::{BufRead, BufReader, IsTerminal},
    process::{Child, Command, Stdio},
    sync::{atomic::Ordering, mpsc},
    thread,
    time::{Duration, Instant},
};

struct TerminalReset(bool);
impl Drop for TerminalReset {
    fn drop(&mut self) {
        if self.0 {
            ratatui::restore();
        }
    }
}

struct Runner<'a> {
    child: Child,
    args: &'a Args,
}
impl Drop for Runner<'_> {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            runtime::stop_unit(self.args, false);
            let until = Instant::now() + Duration::from_secs(22);
            while Instant::now() < until {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn launch(args: &Args) -> Result<i32> {
    let config = runtime::read_config(args)?;
    let port = args
        .port
        .unwrap_or_else(|| config["port"].as_u64().unwrap_or(8080) as u16);
    let mut app = App::new(
        config["model_name"].as_str().unwrap_or("Strata").into(),
        port,
    );
    app.min_free_mib = args.min_free_mib;
    app.grow_below_mib = args.grow_below_mib;
    let signal = runtime::signals()?;
    let mut runner = Runner {
        child: runtime::launch_command(args)?.spawn()?,
        args,
    };
    let (tx, rx) = mpsc::channel();
    if let Some(pipe) = runner.child.stdout.take() {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let event = serde_json::from_str::<Event>(&line).unwrap_or(Event::Log {
                    message: line,
                    raw: false,
                });
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(pipe) = runner.child.stderr.take() {
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                if tx
                    .send(Event::Log {
                        message: line,
                        raw: false,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
    }
    let interactive =
        !args.plain && std::io::stdout().is_terminal() && std::io::stdin().is_terminal();
    let mut terminal = if interactive {
        Some(ratatui::try_init()?)
    } else {
        None
    };
    let reset = TerminalReset(interactive);
    let mut last_stamp = 0.;
    let mut last_stop = Instant::now() - Duration::from_secs(1);
    let mut force = false;
    let exit = loop {
        while let Ok(event) = rx.try_recv() {
            if !interactive {
                plain(&event, args.raw, &mut last_stamp);
            }
            app.apply(event);
        }
        if signal.swap(false, Ordering::Relaxed) {
            force = app.stopping;
            app.stopping = true;
        }
        if let Some(terminal) = &mut terminal {
            terminal.draw(|f| {
                viewport(&mut app, f.area().height);
                ui::render(f, &app)
            })?;
            if event::poll(Duration::from_millis(80))? {
                match event::read()? {
                    TermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                        let quit = key.code == KeyCode::Char('q')
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL));
                        if quit {
                            force = app.stopping;
                            app.stopping = true;
                        } else {
                            key_event(&mut app, key);
                        }
                    }
                    TermEvent::Resize(_, _) => {}
                    _ => {}
                }
            }
        } else {
            thread::sleep(Duration::from_millis(80));
        }
        if app.stopping && last_stop.elapsed() >= Duration::from_millis(500) {
            runtime::stop_unit(args, force);
            last_stop = Instant::now();
        }
        if let Some(status) = runner.child.try_wait()? {
            // Drain final shutdown events before leaving the alternate screen.
            for event in rx.try_iter() {
                if !interactive {
                    plain(&event, args.raw, &mut last_stamp);
                }
                app.apply(event);
            }
            break status.code().unwrap_or(1);
        }
    };
    drop(reset);
    let total = app.metrics.totals();
    if exit != 0 && !app.stopping {
        for line in app.logs.iter().rev().take(4).rev() {
            eprintln!("{line}");
        }
    }
    println!(
        "Strata stopped · {} requests · {} input · {} output · {:.1} tok/s active average",
        total.requests,
        ui::commas(total.input),
        ui::commas(total.output),
        total.tps
    );
    Ok(if app.stopping { 0 } else { exit })
}

fn plain(event: &Event, raw: bool, stamp: &mut f64) {
    match event {
        Event::Log {
            message,
            raw: is_raw,
        } => {
            if !is_raw
                || raw
                || ["error", "warning", "failed"]
                    .iter()
                    .any(|s| message.to_lowercase().contains(s))
            {
                println!(":: {}", model::sanitize(message));
            }
        }
        Event::Metrics(metrics) => {
            if let Some(rows) = metrics.data["requests"].as_array() {
                for row in rows.iter().rev() {
                    let at = number(row, "/time");
                    if at > *stamp {
                        *stamp = at;
                        println!(
                            ":: {} input · {} cached · {} output · {:.1} tok/s · {:.1}s · {}",
                            ui::commas(number(row, "/prompt_tokens") as u64),
                            ui::commas(number(row, "/reused") as u64),
                            ui::commas(number(row, "/output_tokens") as u64),
                            number(row, "/decode_tok_s"),
                            number(row, "/duration_s"),
                            text(row, "/finish")
                        );
                    }
                }
            }
        }
        _ => {}
    }
}

pub fn key_event(app: &mut App, key: KeyEvent) {
    if !app.help && key.modifiers.contains(KeyModifiers::CONTROL) {
        let page = if app.process_focus {
            app.process_rows.max(1)
        } else {
            app.page_rows
        };
        let (direction, count) = match key.code {
            KeyCode::Char('d') => (KeyCode::Down, page.div_ceil(2)),
            KeyCode::Char('u') => (KeyCode::Up, page.div_ceil(2)),
            KeyCode::Char('f') => (KeyCode::Down, page),
            KeyCode::Char('b') => (KeyCode::Up, page),
            _ => {
                key_action(app, key.code);
                return;
            }
        };
        app.pending_g = None;
        for _ in 0..count {
            key_action(app, direction);
        }
        return;
    }
    if !app.help && key.code == KeyCode::Char('g') {
        if app
            .pending_g
            .take()
            .is_some_and(|at| at.elapsed() < Duration::from_millis(700))
        {
            key_action(app, KeyCode::Home);
        } else {
            app.pending_g = Some(Instant::now());
        }
        return;
    }
    app.pending_g = None;
    key_action(app, key.code);
}

pub fn key_action(app: &mut App, key: KeyCode) {
    if app.help {
        if matches!(key, KeyCode::Esc | KeyCode::Char('?')) {
            app.help = false;
        }
        return;
    }
    if key == KeyCode::Char('p') && app.process_rows > 0 {
        app.process_focus = !app.process_focus;
        return;
    }
    if app.process_focus {
        let end = app
            .hardware
            .processes
            .len()
            .saturating_sub(app.process_rows);
        match key {
            KeyCode::Down | KeyCode::Char('j') => {
                app.process_offset = (app.process_offset + 1).min(end);
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.process_offset = app.process_offset.saturating_sub(1);
                return;
            }
            KeyCode::Home => {
                app.process_offset = 0;
                return;
            }
            KeyCode::End | KeyCode::Char('G') => {
                app.process_offset = end;
                return;
            }
            KeyCode::Esc => {
                app.process_focus = false;
                return;
            }
            KeyCode::PageDown => {
                app.process_offset = (app.process_offset + app.process_rows).min(end);
                return;
            }
            KeyCode::PageUp => {
                app.process_offset = app.process_offset.saturating_sub(app.process_rows);
                return;
            }
            KeyCode::Char('?') => {
                app.help = true;
                return;
            }
            _ => app.process_focus = false,
        }
    }
    match key {
        KeyCode::Left | KeyCode::Char('h') => {
            app.view = match app.view {
                View::Logs => View::Requests,
                _ => View::Overview,
            };
            app.scroll = 0;
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.view = match app.view {
                View::Overview => View::Requests,
                _ => View::Logs,
            };
            app.scroll = 0;
        }
        KeyCode::Enter if app.view == View::Overview => {
            app.view = View::Requests;
        }
        KeyCode::Char('1') => {
            app.view = View::Overview;
            app.scroll = 0;
        }
        KeyCode::Char('2') => {
            app.view = View::Requests;
            app.scroll = 0;
        }
        KeyCode::Char('3') => {
            app.view = View::Logs;
            app.scroll = 0;
        }
        KeyCode::Tab => {
            app.view = match app.view {
                View::Overview => View::Requests,
                View::Requests => View::Logs,
                View::Logs => View::Overview,
            };
            app.scroll = 0;
        }
        KeyCode::Char('?') => app.help = true,
        KeyCode::Up | KeyCode::Char('k') => {
            if app.view == View::Logs {
                app.scroll = app.scroll.saturating_add(1).min(app.logs.len());
            } else {
                app.scroll = app.scroll.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.view == View::Logs {
                app.scroll = app.scroll.saturating_sub(1);
            } else {
                app.scroll = (app.scroll + 1).min(
                    app.metrics.data["requests"]
                        .as_array()
                        .map_or(0, |a| a.len().saturating_sub(1)),
                );
            }
        }
        KeyCode::PageUp => {
            for _ in 0..app.page_rows {
                key_action(app, KeyCode::Up);
            }
        }
        KeyCode::PageDown => {
            for _ in 0..app.page_rows {
                key_action(app, KeyCode::Down);
            }
        }
        KeyCode::Home => {
            app.scroll = if app.view == View::Logs {
                app.logs.len().saturating_sub(1)
            } else {
                0
            };
        }
        KeyCode::End | KeyCode::Char('G') => {
            app.scroll = if app.view == View::Logs {
                0
            } else {
                app.metrics.data["requests"]
                    .as_array()
                    .map_or(0, |a| a.len().saturating_sub(1))
            };
        }
        KeyCode::Char('b') => {
            let url = format!("http://{}", app.address);
            thread::spawn(move || {
                let _ = Command::new("xdg-open")
                    .arg(url)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            });
        }
        _ => {}
    }
}

pub fn demo(args: &Args) -> Result<i32> {
    let mut app = model::demo();
    if let Some(size) = &args.snapshot {
        let (w, h) = size
            .split_once('x')
            .ok_or_else(|| anyhow::anyhow!("snapshot size must be WIDTHxHEIGHT"))?;
        let (w, h): (u16, u16) = (w.parse()?, h.parse()?);
        anyhow::ensure!(w <= 500 && h <= 200, "snapshot is limited to 500×200");
        print!("{}", ui::snapshot(&app, w, h, args.cells)?);
        return Ok(0);
    }
    let signal = runtime::signals()?;
    let mut terminal = ratatui::try_init()?;
    let _reset = TerminalReset(true);
    while !signal.load(Ordering::Relaxed) {
        terminal.draw(|f| {
            viewport(&mut app, f.area().height);
            ui::render(f, &app)
        })?;
        if event::poll(Duration::from_millis(100))?
            && let TermEvent::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Char('q')
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                break;
            }
            key_event(&mut app, key);
        }
    }
    Ok(0)
}

fn viewport(app: &mut App, height: u16) {
    app.page_rows = ui::visible_rows(app, height);
    app.process_rows = ui::process_height(height)
        .min(app.hardware.processes.len().max(3) as u16 + 3)
        .saturating_sub(3) as usize;
    if app.process_rows == 0 {
        app.process_focus = false;
    }
    app.process_offset = app.process_offset.min(
        app.hardware
            .processes
            .len()
            .saturating_sub(app.process_rows),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(app: &mut App, code: KeyCode) {
        key_event(app, KeyEvent::new(code, KeyModifiers::NONE));
    }
    #[test]
    fn vim_views_selection_and_gg_g() {
        let mut app = model::demo();
        key(&mut app, KeyCode::Char('l'));
        assert_eq!(app.view, View::Requests);
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.scroll, 1);
        key(&mut app, KeyCode::Char('k'));
        assert_eq!(app.scroll, 0);
        key(&mut app, KeyCode::Char('G'));
        assert_eq!(app.scroll, 7);
        key(&mut app, KeyCode::Char('g'));
        assert_eq!(app.scroll, 7);
        key(&mut app, KeyCode::Char('g'));
        assert_eq!(app.scroll, 0);
        key(&mut app, KeyCode::Char('l'));
        assert_eq!(app.view, View::Logs);
        key(&mut app, KeyCode::Char('k'));
        assert_eq!(app.scroll, 1);
        key(&mut app, KeyCode::Char('G'));
        assert_eq!(app.scroll, 0);
        key(&mut app, KeyCode::Char('h'));
        assert_eq!(app.view, View::Requests);
    }
    #[test]
    fn control_page_navigation_is_bounded() {
        let mut app = model::demo();
        app.view = View::Requests;
        app.page_rows = 6;
        key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.scroll, 3);
        key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.scroll, 0);
        key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.scroll, 6);
        key_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn gpu_process_focus_scrolls_with_vim_and_clears_when_hidden() {
        let mut app = model::demo();
        app.process_rows = 3;
        key(&mut app, KeyCode::Char('p'));
        assert!(app.process_focus);
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.process_offset, 1);
        key(&mut app, KeyCode::Char('G'));
        assert_eq!(app.process_offset, 2);
        key(&mut app, KeyCode::Char('g'));
        key(&mut app, KeyCode::Char('g'));
        assert_eq!(app.process_offset, 0);
        viewport(&mut app, 23);
        assert!(!app.process_focus);
        assert_eq!(app.process_rows, 0);
    }
}
