//! Live terminal dashboard for BTHome thermometers such as LYWSD03MMC on pvvx firmware.
//!
//! `thermo` starts the dashboard; `thermo calibrate [HOURS] [--match DEVICE]` prints a
//! calibration proposal from recorded history without touching any device.

mod app;
mod calibrate;
mod devices;
mod forms;
mod jobs;
mod level;
mod localtime;
mod store;
mod ui;

use std::io::Write;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, EventStream};
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
        ["calibrate"] => print_calibration(DEFAULT_CALIBRATION_HOURS, None),
        ["calibrate", "--match", device] => {
            print_calibration(DEFAULT_CALIBRATION_HOURS, Some(device))
        }
        ["calibrate", hours] => print_calibration(parse_hours(hours)?, None),
        ["calibrate", hours, "--match", device] => {
            print_calibration(parse_hours(hours)?, Some(device))
        }
        ["-h" | "--help"] => {
            println!("usage: thermo               live dashboard");
            println!(
                "       thermo calibrate [HOURS] [--match DEVICE]\n\
                 \x20                        fit calibration on the last HOURS (default {DEFAULT_CALIBRATION_HOURS}),\n\
                 \x20                        so the devices agree with their average or with DEVICE (name or MAC)"
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

fn parse_hours(hours: &str) -> Result<u32> {
    hours
        .trim_end_matches('h')
        .parse()
        .context("HOURS must be a whole number")
}

fn print_calibration(hours: u32, reference: Option<&str>) -> Result<()> {
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
    let reference = reference
        .map(|wanted| {
            app.devices
                .find(wanted)
                .with_context(|| format!("no device named {wanted:?} in the saved history"))
        })
        .transpose()?;
    let run = calibrate::run(&app.devices, &devices, since, now, reference)?;
    print!("{}", calibrate::format(&run));
    Ok(())
}

async fn dashboard() -> Result<()> {
    let mut terminal = ratatui::init();
    // Must run after entering the alternate screen and before reading input events.
    let picker = Picker::from_query_stdio().ok();
    let result = run(&mut terminal, picker).await;
    ratatui::restore();
    result
}

async fn run(terminal: &mut DefaultTerminal, picker: Option<Picker>) -> Result<()> {
    // A terminal without the kitty protocol (Terminal.app) may print part of the
    // picker's graphics query instead of swallowing it, and ratatui never redraws the
    // cells it believes blank: start from a really blank screen.
    terminal.clear()?;
    let bluetooth = ble::Bluetooth::default();
    let (scan_tx, mut scan_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(bluetooth.clone().scan(scan_tx, control_rx));
    let (job_events_tx, mut job_events) = mpsc::unbounded_channel();
    let jobs = jobs::spawn(bluetooth, job_events_tx);

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
                if let Event::Key(key) = event? {
                    app.on_key(key);
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
