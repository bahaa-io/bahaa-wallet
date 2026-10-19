use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bahaa_types::PriceId;
use futures_util::{SinkExt, StreamExt};
use rustls::ClientConfig;
use rustls_platform_verifier::ConfigVerifierExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, connect_async_tls_with_config};
use url::Url;

use crate::protocol::{self, Frame, Quotes};
use crate::{Fetch, MAX_REPLY, MarketError, RestClient};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketConfig {
    pub api: Url,
    pub stream: Url,
    pub poll_every: Duration,
    pub backoff_start: Duration,
    pub backoff_max: Duration,
    pub silence_limit: Duration,
    pub connect_timeout: Duration,
}

impl MarketConfig {
    pub fn bahaa() -> Result<Self, MarketError> {
        let parse = |text: &str| {
            Url::parse(text).map_err(|_| MarketError::Invalid("bad price service address"))
        };
        Ok(Self {
            api: parse("https://api.bahaa.io/")?,
            stream: parse("wss://api.bahaa.io/v1/stream")?,
            poll_every: Duration::from_secs(30),
            backoff_start: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            silence_limit: Duration::from_secs(45),
            connect_timeout: Duration::from_secs(10),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub wanted: BTreeSet<PriceId>,
    pub base: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Live,
    Polling,
    Offline,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub base: String,
    pub quotes: Quotes,
    pub status: Status,
}

pub struct Market {
    want: watch::Sender<Request>,
    snapshot: watch::Receiver<Snapshot>,
    task: JoinHandle<()>,
}

impl Market {
    pub fn spawn(config: MarketConfig) -> Result<Self, MarketError> {
        let rest = RestClient::new(&config.api)?;
        let tls = Arc::new(
            ClientConfig::with_platform_verifier()
                .map_err(|err| MarketError::Network(err.to_string()))?,
        );
        let (want, wanted) = watch::channel(Request::default());
        let (publish, snapshot) = watch::channel(Snapshot::default());
        let worker = Worker {
            config,
            rest,
            tls,
            wanted,
            publish,
            last_poll: None,
            etag: None,
        };
        let task = tokio::spawn(worker.run());
        Ok(Self {
            want,
            snapshot,
            task,
        })
    }

    pub fn want(&self, request: Request) {
        self.want.send_replace(request);
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.clone()
    }
}

impl Drop for Market {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reply {
    Nothing,
    Pong,
    Live,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Session {
    Healthy,
    Failed,
    Idle,
    Closed,
}

struct Worker {
    config: MarketConfig,
    rest: RestClient,
    tls: Arc<ClientConfig>,
    wanted: watch::Receiver<Request>,
    publish: watch::Sender<Snapshot>,
    last_poll: Option<Instant>,
    etag: Option<(Request, String)>,
}

impl Worker {
    async fn run(mut self) {
        let mut backoff = self.config.backoff_start;
        loop {
            let request = self.wanted.borrow_and_update().clone();
            self.align(&request);
            if request.wanted.is_empty() {
                self.status(Status::Idle);
                if self.wanted.changed().await.is_err() {
                    return;
                }
                continue;
            }
            match Box::pin(self.stream(request)).await {
                Session::Closed => return,
                Session::Idle => continue,
                Session::Healthy => {
                    backoff = self.config.backoff_start;
                    self.last_poll = None;
                }
                Session::Failed => {}
            }
            if self.publish.borrow().status == Status::Live {
                self.status(Status::Offline);
            }
            if !self.poll_until(Instant::now() + backoff).await {
                return;
            }
            backoff = (backoff * 2).min(self.config.backoff_max);
        }
    }

    async fn stream(&mut self, mut request: Request) -> Session {
        let settings = WebSocketConfig::default()
            .max_message_size(Some(MAX_REPLY))
            .max_frame_size(Some(MAX_REPLY));
        let connector = Connector::Rustls(Arc::clone(&self.tls));
        let connect = connect_async_tls_with_config(
            self.config.stream.as_str(),
            Some(settings),
            false,
            Some(connector),
        );
        let Ok(Ok((mut socket, _))) = timeout(self.config.connect_timeout, connect).await else {
            return Session::Failed;
        };
        if socket
            .send(Message::text(protocol::subscribe(
                &request.wanted,
                &request.base,
            )))
            .await
            .is_err()
        {
            return Session::Failed;
        }
        let mut healthy = false;
        let outcome = |healthy: bool| {
            if healthy {
                Session::Healthy
            } else {
                Session::Failed
            }
        };
        loop {
            tokio::select! {
                message = timeout(self.config.silence_limit, socket.next()) => {
                    let Ok(Some(Ok(message))) = message else {
                        return outcome(healthy);
                    };
                    let Message::Text(text) = message else {
                        if matches!(message, Message::Close(_)) {
                            return outcome(healthy);
                        }
                        continue;
                    };
                    let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                        return outcome(healthy);
                    };
                    match self.on_frame(frame, &request) {
                        Reply::Pong => {
                            if socket.send(Message::text(protocol::pong())).await.is_err() {
                                return outcome(healthy);
                            }
                        }
                        Reply::Live => healthy = true,
                        Reply::Stop => return outcome(healthy),
                        Reply::Nothing => {}
                    }
                }
                changed = self.wanted.changed() => {
                    if changed.is_err() {
                        let _ = socket.close(None).await;
                        return Session::Closed;
                    }
                    let next = self.wanted.borrow_and_update().clone();
                    if next == request {
                        continue;
                    }
                    self.align(&next);
                    if next.wanted.is_empty() {
                        let _ = socket.close(None).await;
                        return Session::Idle;
                    }
                    let resubscribed = socket.send(Message::text(protocol::unsubscribe())).await.is_ok()
                        && socket
                            .send(Message::text(protocol::subscribe(&next.wanted, &next.base)))
                            .await
                            .is_ok();
                    if !resubscribed {
                        return outcome(healthy);
                    }
                    request = next;
                }
            }
        }
    }

    fn on_frame(&self, frame: Frame, request: &Request) -> Reply {
        match frame.kind.as_str() {
            "ping" => Reply::Pong,
            "error" => Reply::Stop,
            "snapshot" if frame.ours() => {
                let quotes = protocol::quotes(frame.result, &request.base, &request.wanted);
                self.publish.send_modify(|snapshot| {
                    snapshot.quotes = quotes;
                    snapshot.status = Status::Live;
                });
                Reply::Live
            }
            "rates" if frame.ours() => {
                let quotes = protocol::quotes(frame.result, &request.base, &request.wanted);
                self.publish
                    .send_modify(|snapshot| snapshot.quotes.extend(quotes));
                Reply::Nothing
            }
            "removed" if frame.ours() => {
                let gone: Vec<PriceId> =
                    frame.result.iter().filter_map(protocol::Item::id).collect();
                self.publish.send_modify(|snapshot| {
                    for id in &gone {
                        snapshot.quotes.remove(id);
                    }
                });
                Reply::Nothing
            }
            _ => Reply::Nothing,
        }
    }

    async fn poll_until(&mut self, reconnect_at: Instant) -> bool {
        loop {
            let request = self.wanted.borrow_and_update().clone();
            if request.wanted.is_empty() {
                return true;
            }
            self.align(&request);
            let due = self
                .last_poll
                .map_or_else(Instant::now, |last| last + self.config.poll_every);
            if due <= Instant::now() {
                self.poll_once(&request).await;
            }
            let wake = due.max(Instant::now()).min(reconnect_at);
            if Instant::now() >= reconnect_at {
                return true;
            }
            tokio::select! {
                () = sleep_until(wake) => {}
                changed = self.wanted.changed() => {
                    if changed.is_err() {
                        return false;
                    }
                    self.last_poll = None;
                }
            }
            if Instant::now() >= reconnect_at {
                return true;
            }
        }
    }

    async fn poll_once(&mut self, request: &Request) {
        self.last_poll = Some(Instant::now());
        let etag = self
            .etag
            .as_ref()
            .filter(|(seen, _)| seen == request)
            .map(|(_, etag)| etag.clone());
        match self
            .rest
            .fetch(&request.wanted, &request.base, etag.as_deref())
            .await
        {
            Ok(Fetch::Fresh { quotes, etag }) => {
                self.etag = etag.map(|etag| (request.clone(), etag));
                self.publish.send_modify(|snapshot| {
                    snapshot.quotes = quotes;
                    snapshot.status = Status::Polling;
                });
            }
            Ok(Fetch::NotModified) => self.status(Status::Polling),
            Err(MarketError::RateLimited(wait)) => {
                if let Some(wait) = wait {
                    self.last_poll =
                        Some(Instant::now() + wait.saturating_sub(self.config.poll_every));
                }
            }
            Err(_) => self.status(Status::Offline),
        }
    }

    fn align(&self, request: &Request) {
        self.publish.send_if_modified(|snapshot| {
            let before = snapshot.quotes.len();
            let mut changed = false;
            if snapshot.base != request.base {
                snapshot.base.clone_from(&request.base);
                snapshot.quotes.clear();
                changed = true;
            }
            snapshot.quotes.retain(|id, _| request.wanted.contains(id));
            changed || snapshot.quotes.len() != before
        });
    }

    fn status(&self, status: Status) {
        self.publish.send_if_modified(|snapshot| {
            let changed = snapshot.status != status;
            snapshot.status = status;
            changed
        });
    }
}
