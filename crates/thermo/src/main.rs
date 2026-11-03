//! Live terminal dashboard for BTHome thermometers such as LYWSD03MMC on pvvx firmware.
//!
//! `thermo` starts the dashboard; `thermo calibrate [HOURS]` prints a calibration
//! proposal from recorded history without touching any device.

mod app;
mod ble;
mod calibrate;
mod devices;
mod forms;
mod jobs;
mod level;
mod localtime;
mod logfile;
mod scanner;
mod store;
mod ui;

use std::io::Write;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::app::App;
use crate::store::Store;
use crate::ui::Ui;

/// Redraw at least this often so "last seen" ages and the time axis keep moving.
const TICK: Duration = Duration::from_secs(1);
const DEFAULT_CALIBRATION_HOURS: u32 = 6;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => dashboard().await,
        ["calibrate"] => print_calibration(DEFAULT_CALIBRATION_HOURS),
        ["calibrate", hours] => print_calibration(
            hours
                .trim_end_matches('h')
                .parse()
                .context("HOURS must be a whole number")?,
        ),
        ["-h" | "--help"] => {
            println!("usage: thermo               live dashboard");
            println!(
                "       thermo calibrate [HOURS]  fit calibration on the last HOURS (default {DEFAULT_CALIBRATION_HOURS})"
            );
            Ok(())
        }
        _ => bail!("unknown arguments {args:?}; try --help"),
    }
}

fn open_store() -> Result<Store> {
    Store::default_dir()
        .context("neither XDG_DATA_HOME nor HOME is set")
        .and_then(Store::open)
}

fn print_calibration(hours: u32) -> Result<()> {
    let mut app = App::new(None, None);
    app.use_store(open_store()?);
    if let Some(warning) = &app.warning {
        eprintln!("warning: {warning}");
    }
    let now = SystemTime::now();
    let devices = app.compared_devices();
    let last_changes = open_store()?.last_calibrations()?;
    let requested = now - Duration::from_secs(u64::from(hours) * 3600);
    let since = calibrate::window_start(requested, &devices, &last_changes);
    let run = calibrate::run(&app.devices, &devices, since, now)?;
    print!("{}", calibrate::format(&run));
    Ok(())
}

async fn dashboard() -> Result<()> {
    let log_path = logfile::init(Store::default_dir().as_deref()).unwrap_or_else(|err| {
        eprintln!("warning: no log file: {err}");
        None
    });
    let mut terminal = ratatui::init();
    // Must run after entering the alternate screen and before reading input events.
    let picker = Picker::from_query_stdio().ok();
    let result = run(&mut terminal, picker).await;
    ratatui::restore();
    log::logger().flush();
    if let Some(path) = log_path {
        eprintln!("log: {}", path.display());
    }
    result
}

async fn run(terminal: &mut DefaultTerminal, picker: Option<Picker>) -> Result<()> {
    let (scan_tx, mut scan_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let ble = ble::Ble::default();
    tokio::spawn(scanner::run(ble.clone(), scan_tx, control_rx));
    let (job_events_tx, mut job_events) = mpsc::unbounded_channel();
    let jobs = jobs::spawn(ble, job_events_tx);

    let mut app = App::new(Some(jobs), Some(control_tx));
    match open_store() {
        Ok(store) => app.use_store(store),
        Err(err) => app.warning = Some(format!("history is not saved: {err:#}")),
    }
    let (mut ui, mut chart_updates) = Ui::new(picker);
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    while !app.should_quit() {
        terminal.draw(|frame| ui::draw(frame, &app, &mut ui))?;
        // Keys first so they never wait behind adverts; adverts that piled up during a
        // slow frame are applied together and cost a single redraw.
        tokio::select! {
            biased;
            Some(event) = input.next() => {
                match event? {
                    // Ctrl-L repaints everything, like in a shell.
                    Event::Key(key)
                        if key.code == KeyCode::Char('l')
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        terminal.clear()?;
                    }
                    Event::Key(key) => app.on_key(key),
                    // Some terminals (Terminal.app) leave stray cells behind when the
                    // window size changes; repaint from scratch.
                    Event::Resize(..) => terminal.clear()?,
                    _ => {}
                }
            }
            Some(update) = chart_updates.recv() => {
                if let Some(seq) = ui.accept_chart(update) {
                    write_raw(terminal, &seq)?;
                }
            }
            Some(event) = job_events.recv() => app.on_job_event(event),
            Some(event) = scan_rx.recv() => {
                app.on_scan_event(event);
                while let Ok(event) = scan_rx.try_recv() {
                    app.on_scan_event(event);
                }
            }
            _ = tick.tick() => app.on_tick(),
        }
    }
    if let Some(seq) = ui.farewell() {
        write_raw(terminal, &seq)?;
    }
    Ok(())
}

/// Sends escape sequences that bypass ratatui's cell buffer.
fn write_raw(terminal: &mut DefaultTerminal, seq: &str) -> Result<()> {
    let backend = terminal.backend_mut();
    backend.write_all(seq.as_bytes())?;
    backend.flush()?;
    Ok(())
}
