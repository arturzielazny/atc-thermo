//! Live terminal dashboard for BTHome thermometers such as LYWSD03MMC on pvvx firmware.

mod app;
mod devices;
mod scanner;
mod store;
mod ui;

use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use tokio::sync::mpsc;

use crate::app::App;
use crate::store::Store;
use crate::ui::Ui;

/// Redraw at least this often so "last seen" ages and the time axis keep moving.
const TICK: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    // Must run after entering the alternate screen and before reading input events.
    let picker = Picker::from_query_stdio().ok();
    let result = run(&mut terminal, picker).await;
    ratatui::restore();
    result
}

async fn run(terminal: &mut DefaultTerminal, picker: Option<Picker>) -> Result<()> {
    let (scan_tx, mut scan_rx) = mpsc::unbounded_channel();
    tokio::spawn(scanner::run(scan_tx));

    let mut app = App::new();
    let store = Store::default_dir()
        .context("neither XDG_DATA_HOME nor HOME is set")
        .and_then(Store::open);
    match store {
        Ok(store) => app.use_store(store),
        Err(err) => app.warning = Some(format!("history is not saved: {err:#}")),
    }
    let mut ui = Ui::new(picker);
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(TICK);

    while !app.should_quit() {
        terminal.draw(|frame| ui::draw(frame, &app, &mut ui))?;
        tokio::select! {
            Some(event) = scan_rx.recv() => app.on_scan_event(event),
            Some(event) = input.next() => {
                if let Event::Key(key) = event? {
                    app.on_key(key);
                }
            }
            _ = tick.tick() => {}
        }
    }
    Ok(())
}
