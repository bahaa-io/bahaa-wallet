mod draw;
mod editor;
mod hints;
mod palette;
mod platform;
mod terminal;
mod theme;
mod ui;
mod update;
mod wrap;

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use bahaa_app_core::{App, AppConfig, Request};
use clap::Parser;
use crossterm::event::{Event, EventStream};
use directories::ProjectDirs;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::platform::Platform;
use crate::terminal::Session;
use crate::ui::Ui;

const SPIN: Duration = Duration::from_millis(100);

#[derive(Parser)]
#[command(name = env!("CARGO_BIN_NAME"), version, about = hints::ABOUT)]
struct Args {
    #[arg(long, value_name = "DIR", help = hints::DATA_HELP)]
    data: Option<PathBuf>,
}

#[tokio::main]
#[allow(clippy::print_stderr)]
async fn main() -> ExitCode {
    let args = Args::parse();
    let Some(data) = args.data.or_else(|| {
        ProjectDirs::from("io", "bahaa", "Bahaa Wallet").map(|dirs| dirs.data_dir().to_owned())
    }) else {
        eprintln!("{}", hints::NO_DATA_DIR);
        return ExitCode::FAILURE;
    };
    let config = AppConfig {
        updates: true,
        ..AppConfig::new(data)
    };
    let (app, requests) = match App::start(config) {
        Ok(started) => started,
        Err(err) => {
            eprintln!("{}", hints::cannot_start(err));
            return ExitCode::FAILURE;
        }
    };
    let result = run(&app, requests).await;
    let _ = app.shutdown().await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}", hints::cannot_start(err));
            ExitCode::FAILURE
        }
    }
}

async fn run(app: &App, mut requests: mpsc::Receiver<Request>) -> io::Result<()> {
    let mut session = Session::start()?;
    let mut platform = Platform::new();
    let mut views = app.view();
    let mut view = Arc::clone(&views.borrow_and_update());
    let mut ui = Ui::default();
    ui.sync(&view);
    let mut events = EventStream::new();
    let mut spin = tokio::time::interval(SPIN);
    let (finished, mut results) = mpsc::unbounded_channel();
    loop {
        if let Some(release) = ui.install.take() {
            let finished = finished.clone();
            tokio::spawn(async move {
                let result = update::install(release).await;
                let _ = finished.send(update::Done::Installed(result));
            });
        }
        session.draw(|frame| draw::frame(frame, &view, &mut ui))?;
        tokio::select! {
            event = events.next() => {
                let Some(first) = event else {
                    return Ok(());
                };
                let mut next = Some(first);
                while let Some(event) = next {
                    for action in ui.event(&event?, &view) {
                        ui.sent(app.act(action));
                    }
                    next = queued(&mut events).await;
                }
            }
            changed = views.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                view = Arc::clone(&views.borrow_and_update());
                ui.sync(&view);
            }
            Some(request) = requests.recv() => match request {
                Request::Copy(text) => ui.notice(platform.copy(text)),
                Request::ClearClipboard => platform.clear(),
                Request::Open(url) => ui.notice(Platform::open(&url)),
                Request::CheckUpdates { beta, asked } => {
                    let finished = finished.clone();
                    tokio::spawn(async move {
                        let result = update::check(beta).await;
                        let _ = finished.send(update::Done::Checked { asked, result });
                    });
                }
                Request::Quit => return Ok(()),
            },
            Some(done) = results.recv() => ui.updates(done),
            _ = spin.tick(), if view.busy || ui.installing() => ui.spin(),
        }
    }
}

async fn queued(events: &mut EventStream) -> Option<io::Result<Event>> {
    std::future::poll_fn(|cx| match events.poll_next_unpin(cx) {
        Poll::Ready(event) => Poll::Ready(event),
        Poll::Pending => Poll::Ready(None),
    })
    .await
}
