use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bahaa_engine::{Engine, EngineConfig, EngineError};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, watch};

use crate::action::Action;
use crate::call;
use crate::core::{Core, Effect, Host, Input};
use crate::view::{Request, View};

const TICK: Duration = Duration::from_secs(1);
const REQUESTS: usize = 64;

pub struct AppConfig {
    pub engine: EngineConfig,
    pub updates: bool,
}

impl AppConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            engine: EngineConfig::new(data_dir),
            updates: false,
        }
    }
}

#[derive(Clone)]
pub struct App {
    actions: mpsc::UnboundedSender<(u64, Action)>,
    sent: Arc<AtomicU64>,
    view: watch::Receiver<Arc<View>>,
    engine: Engine,
}

impl App {
    pub fn start(config: AppConfig) -> Result<(Self, mpsc::Receiver<Request>), EngineError> {
        let host = Host {
            folder: config.engine.data_dir.display().to_string(),
            updates: config.updates,
        };
        let engine = Engine::start(config.engine)?;
        let core = Core::new(engine.registry(), bahaa_engine::now(), host);
        let (actions, inbox) = mpsc::unbounded_channel();
        let (requests_tx, requests) = mpsc::channel(REQUESTS);
        let (publish, view) = watch::channel(Arc::new(core.view()));
        let (results_tx, results) = mpsc::unbounded_channel();
        let runtime = Runtime {
            engine: engine.clone(),
            core,
            inbox,
            requests: requests_tx,
            publish,
            results_tx,
            results,
        };
        tokio::spawn(runtime.run());
        Ok((
            Self {
                actions,
                sent: Arc::new(AtomicU64::new(0)),
                view,
                engine,
            },
            requests,
        ))
    }

    pub fn act(&self, action: Action) -> u64 {
        let number = self.sent.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.actions.send((number, action));
        number
    }

    pub fn view(&self) -> watch::Receiver<Arc<View>> {
        self.view.clone()
    }

    pub async fn shutdown(&self) -> Result<(), EngineError> {
        self.engine.shutdown().await
    }
}

struct Runtime {
    engine: Engine,
    core: Core,
    inbox: mpsc::UnboundedReceiver<(u64, Action)>,
    requests: mpsc::Sender<Request>,
    publish: watch::Sender<Arc<View>>,
    results_tx: mpsc::UnboundedSender<Input>,
    results: mpsc::UnboundedReceiver<Input>,
}

impl Runtime {
    async fn run(mut self) {
        let mut state = self.engine.watch_state();
        let mut portfolio = self.engine.portfolio();
        let mut events = self.engine.events();
        let mut tick = tokio::time::interval(TICK);
        self.step(Input::State(*state.borrow_and_update()));
        loop {
            let input = tokio::select! {
                action = self.inbox.recv() => match action {
                    Some((number, action)) => {
                        self.engine.touch();
                        Input::Action(number, action)
                    }
                    None => break,
                },
                Ok(()) = state.changed() => Input::State(*state.borrow_and_update()),
                Ok(()) = portfolio.changed() => {
                    Input::Portfolio(Box::new(portfolio.borrow_and_update().clone()))
                }
                event = events.recv() => match event {
                    Ok(event) => Input::Event(Box::new(event)),
                    Err(RecvError::Lagged(_)) => Input::Lagged,
                    Err(RecvError::Closed) => break,
                },
                Some(input) = self.results.recv() => input,
                _ = tick.tick() => Input::Tick(bahaa_engine::now()),
            };
            self.step(input);
        }
    }

    fn step(&mut self, input: Input) {
        for effect in self.core.update(input) {
            self.perform(effect);
        }
        let view = self.core.view();
        self.publish.send_if_modified(|current| {
            let changed = **current != view;
            if changed {
                *current = Arc::new(view);
            }
            changed
        });
    }

    fn perform(&self, effect: Effect) {
        let engine = self.engine.clone();
        let results = self.results_tx.clone();
        match effect {
            Effect::Call(ticket, call) => {
                tokio::spawn(async move {
                    let outcome = call::run(&engine, call).await;
                    let _ = results.send(Input::Done(ticket, Box::new(outcome)));
                });
            }
            Effect::Load(load) => {
                tokio::spawn(async move {
                    if let Some(loaded) = call::load(&engine, load).await {
                        let _ = results.send(Input::Loaded(loaded));
                    }
                });
            }
            Effect::Request(request) => {
                let _ = self.requests.try_send(request);
            }
            Effect::After(delay, timer) => {
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let _ = results.send(Input::Timer(timer));
                });
            }
        }
    }
}
