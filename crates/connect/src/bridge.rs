use std::time::Duration;

use bahaa_types::hex;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Client;
use reqwest::header::ACCEPT;
use serde_json::Value;
use tokio::sync::mpsc;
use url::Url;

use crate::ConnectError;
use crate::session::SessionKey;

const TTL: &str = "300";
const MAX_EVENT: usize = 4 * 1024 * 1024;
const MAX_ID: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct Bridge {
    http: Client,
    url: Url,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub event: Option<String>,
    pub from: [u8; 32],
    pub sealed: Vec<u8>,
    pub trace: Option<String>,
}

impl Bridge {
    pub fn new(url: Url) -> Result<Self, ConnectError> {
        let http = Client::builder()
            .user_agent(concat!("bahaa-wallet/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|err| ConnectError::Network(err.to_string()))?;
        Ok(Self { http, url })
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub async fn send(
        &self,
        key: &SessionKey,
        to: &[u8; 32],
        message: &[u8],
        topic: Option<&str>,
        trace: Option<&str>,
    ) -> Result<(), ConnectError> {
        let sealed = key.seal(to, message)?;
        let mut url = self.endpoint("message")?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("client_id", &hex::encode(&key.id()))
                .append_pair("to", &hex::encode(to))
                .append_pair("ttl", TTL);
            if let Some(topic) = topic {
                query.append_pair("topic", topic);
            }
            if let Some(trace) = trace {
                query.append_pair("trace_id", trace);
            }
        }
        let response = self
            .http
            .post(url)
            .timeout(SEND_TIMEOUT)
            .body(STANDARD.encode(sealed))
            .send()
            .await
            .map_err(|err| ConnectError::Network(err.without_url().to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(ConnectError::Refused(response.status().as_u16()))
        }
    }

    pub async fn listen(
        &self,
        ids: &[[u8; 32]],
        last: Option<&str>,
        out: &mpsc::Sender<Incoming>,
    ) -> Result<(), ConnectError> {
        let network = |err: reqwest::Error| ConnectError::Network(err.without_url().to_string());
        let mut url = self.endpoint("events")?;
        {
            let ids: Vec<String> = ids.iter().map(|id| hex::encode(id)).collect();
            let mut query = url.query_pairs_mut();
            query.append_pair("client_id", &ids.join(","));
            if let Some(last) = last {
                query.append_pair("last_event_id", last);
            }
        }
        let mut response = self
            .http
            .get(url)
            .header(ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(network)?;
        if !response.status().is_success() {
            return Err(ConnectError::Refused(response.status().as_u16()));
        }
        let mut stream = Stream::default();
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            for event in stream.feed(&chunk)? {
                if let Some(incoming) = incoming(event)
                    && out.send(incoming).await.is_err()
                {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn endpoint(&self, path: &str) -> Result<Url, ConnectError> {
        let mut url = self.url.clone();
        url.path_segments_mut()
            .map_err(|()| ConnectError::Invalid("not a usable bridge address"))?
            .pop_if_empty()
            .push(path);
        Ok(url)
    }
}

#[derive(Default)]
struct Stream {
    line: Vec<u8>,
    kind: String,
    id: Option<String>,
    data: String,
}

struct Event {
    kind: String,
    id: Option<String>,
    data: String,
}

impl Stream {
    fn feed(&mut self, chunk: &[u8]) -> Result<Vec<Event>, ConnectError> {
        let mut events = Vec::new();
        for byte in chunk {
            if *byte != b'\n' {
                self.line.push(*byte);
                if self.line.len() + self.data.len() > MAX_EVENT {
                    return Err(ConnectError::Network(
                        "the bridge sent an oversized event".to_owned(),
                    ));
                }
                continue;
            }
            let line = std::mem::take(&mut self.line);
            let line =
                String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(&line)).into_owned();
            if line.is_empty() {
                if !self.data.is_empty() || !self.kind.is_empty() {
                    events.push(Event {
                        kind: std::mem::take(&mut self.kind),
                        id: self.id.clone(),
                        data: std::mem::take(&mut self.data),
                    });
                }
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line.as_str(), ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => value.clone_into(&mut self.kind),
                "id" if value.len() <= MAX_ID => self.id = Some(value.to_owned()),
                "data" => {
                    if !self.data.is_empty() {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                }
                _ => {}
            }
        }
        Ok(events)
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(data: &[u8]) {
    let mut stream = Stream::default();
    for chunk in data.chunks(7) {
        let Ok(events) = stream.feed(chunk) else {
            return;
        };
        for event in events {
            let _ = incoming(event);
        }
    }
}

fn incoming(event: Event) -> Option<Incoming> {
    if event.kind == "heartbeat" || event.data == "heartbeat" || event.data.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(&event.data).ok()?;
    let from = hex::decode_array::<32>(value.get("from")?.as_str()?)?;
    let sealed = STANDARD.decode(value.get("message")?.as_str()?).ok()?;
    let trace = value
        .get("trace_id")
        .and_then(Value::as_str)
        .filter(|trace| trace.len() <= MAX_ID)
        .map(str::to_owned);
    Some(Incoming {
        event: event.id,
        from,
        sealed,
        trace,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_read_across_chunks_and_heartbeats_skipped() {
        let mut stream = Stream::default();
        let first = stream
            .feed(b"retry: 100\r\n\r\nevent: heartbeat\n\nevent: message\nid: 3675\ndata: {\"from\":\"c26eecc868c3878d73ac2ef3cb4a22b497d9f621c1b8eea54d74a99cc8ca7f5c\",")
            .unwrap();
        assert_eq!(first.len(), 1);
        assert!(first.into_iter().next().and_then(incoming).is_none());
        let rest = stream.feed(b"\"message\":\"aGVsbG8=\",\"trace_id\":\"01a1dc26\"}\n\nevent: message\ndata: heartbeat\n\n").unwrap();
        let found: Vec<Incoming> = rest.into_iter().filter_map(incoming).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].event.as_deref(), Some("3675"));
        assert_eq!(found[0].sealed, b"hello");
        assert_eq!(found[0].trace.as_deref(), Some("01a1dc26"));
    }

    #[test]
    fn oversized_events_stop_the_stream() {
        let mut stream = Stream::default();
        let big = vec![b'a'; MAX_EVENT + 1];
        assert!(stream.feed(&big).is_err());
    }

    #[test]
    fn endpoints_keep_the_bridge_path() {
        for (base, expected) in [
            (
                "https://connect.ton.org/bridge",
                "https://connect.ton.org/bridge/events",
            ),
            (
                "https://tonconnectbridge.mytonwallet.org/bridge/",
                "https://tonconnectbridge.mytonwallet.org/bridge/events",
            ),
            (
                "https://tonbridge.telegram.org",
                "https://tonbridge.telegram.org/events",
            ),
        ] {
            let bridge = Bridge::new(Url::parse(base).unwrap()).unwrap();
            assert_eq!(bridge.endpoint("events").unwrap().as_str(), expected);
        }
    }
}
