use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bahaa_config::{Endpoint, Feature};
use thiserror::Error;
use tokio::time::{Instant, sleep_until};

pub const COOLDOWN_BASE: Duration = Duration::from_secs(5);
pub const COOLDOWN_MAX: Duration = Duration::from_secs(300);
const MAX_DOUBLINGS: u32 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure<E> {
    Retry(E),
    RateLimited(E, Option<Duration>),
    Unsupported(E),
    Fatal(E),
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PoolError<E> {
    #[error("{0}")]
    Fatal(E),
    #[error("every endpoint failed, the last said: {0}")]
    Exhausted(E),
    #[error("no endpoint is available right now, try again in {} s", .0.as_secs().max(1))]
    Unavailable(Duration),
    #[error("no endpoint offers what this request needs")]
    NoEndpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointStatus {
    pub url: String,
    pub available: bool,
    pub failures: u32,
    pub latency: Option<Duration>,
}

#[derive(Debug)]
pub struct Pool {
    slots: Vec<Slot>,
}

#[derive(Debug)]
struct Slot {
    endpoint: Endpoint,
    health: Mutex<Health>,
}

#[derive(Debug, Default)]
struct Health {
    failures: u32,
    cooldown_until: Option<Instant>,
    next_start: Option<Instant>,
    latency: Option<Duration>,
}

impl Pool {
    pub fn new(endpoints: Vec<Endpoint>) -> Self {
        Self {
            slots: endpoints
                .into_iter()
                .map(|endpoint| Slot {
                    endpoint,
                    health: Mutex::new(Health::default()),
                })
                .collect(),
        }
    }

    pub async fn call<T, E, F, Fut>(&self, need: Option<Feature>, op: F) -> Result<T, PoolError<E>>
    where
        F: Fn(Endpoint) -> Fut,
        Fut: Future<Output = Result<T, Failure<E>>>,
    {
        let candidates: Vec<&Slot> = self
            .slots
            .iter()
            .filter(|slot| need.is_none_or(|feature| slot.endpoint.has(feature)))
            .collect();
        if candidates.is_empty() {
            return Err(PoolError::NoEndpoint);
        }
        let now = Instant::now();
        let ready: Vec<&Slot> = candidates
            .iter()
            .copied()
            .filter(|slot| slot.available(now))
            .collect();
        let mut last = None;
        for slot in ready {
            sleep_until(slot.reserve()).await;
            let started = Instant::now();
            match op(slot.endpoint.clone()).await {
                Ok(value) => {
                    slot.succeeded(started.elapsed());
                    return Ok(value);
                }
                Err(Failure::Fatal(err)) => {
                    slot.succeeded(started.elapsed());
                    return Err(PoolError::Fatal(err));
                }
                Err(Failure::Retry(err)) => {
                    slot.failed(None);
                    last = Some(err);
                }
                Err(Failure::RateLimited(err, wait)) => {
                    slot.failed(Some(wait.unwrap_or(COOLDOWN_BASE)));
                    last = Some(err);
                }
                Err(Failure::Unsupported(err)) => {
                    slot.answered();
                    last = Some(err);
                }
            }
        }
        if let Some(err) = last {
            return Err(PoolError::Exhausted(err));
        }
        let now = Instant::now();
        let soonest = candidates
            .iter()
            .filter_map(|slot| slot.health().cooldown_until)
            .min()
            .unwrap_or(now);
        Err(PoolError::Unavailable(
            soonest.saturating_duration_since(now),
        ))
    }

    pub fn status(&self) -> Vec<EndpointStatus> {
        let now = Instant::now();
        self.slots
            .iter()
            .map(|slot| {
                let health = slot.health();
                EndpointStatus {
                    url: slot.endpoint.url.to_string(),
                    available: health.cooldown_until.is_none_or(|until| until <= now),
                    failures: health.failures,
                    latency: health.latency,
                }
            })
            .collect()
    }
}

impl Slot {
    fn health(&self) -> MutexGuard<'_, Health> {
        self.health.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn gap(&self) -> Duration {
        Duration::from_millis(u64::from(self.endpoint.min_gap_ms))
    }

    fn available(&self, now: Instant) -> bool {
        self.health()
            .cooldown_until
            .is_none_or(|until| until <= now)
    }

    fn reserve(&self) -> Instant {
        let gap = self.gap();
        let mut health = self.health();
        let start = health
            .next_start
            .map_or_else(Instant::now, |next| next.max(Instant::now()));
        health.next_start = Some(start + gap);
        start
    }

    fn finished(&self, health: &mut Health) {
        let after = Instant::now() + self.gap();
        health.next_start = Some(health.next_start.map_or(after, |next| next.max(after)));
    }

    fn succeeded(&self, took: Duration) {
        let mut health = self.health();
        health.failures = 0;
        health.cooldown_until = None;
        health.latency = Some(health.latency.map_or(took, |old| old * 3 / 4 + took / 4));
        self.finished(&mut health);
    }

    fn answered(&self) {
        let mut health = self.health();
        self.finished(&mut health);
    }

    fn failed(&self, wait: Option<Duration>) {
        let mut health = self.health();
        health.failures = health.failures.saturating_add(1);
        let backoff = COOLDOWN_BASE
            .saturating_mul(1 << (health.failures - 1).min(MAX_DOUBLINGS))
            .min(COOLDOWN_MAX);
        health.cooldown_until = Some(Instant::now() + wait.unwrap_or(backoff).min(COOLDOWN_MAX));
        self.finished(&mut health);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bahaa_config::Protocol;
    use url::Url;

    use super::*;

    type Log = Arc<Mutex<Vec<String>>>;

    fn endpoint(host: &str, features: Vec<Feature>, min_gap_ms: u32) -> Endpoint {
        Endpoint {
            protocol: Protocol::JsonRpc,
            url: Url::parse(&format!("https://{host}/")).unwrap(),
            features,
            min_gap_ms,
        }
    }

    fn pool(hosts: &[&str]) -> Pool {
        Pool::new(hosts.iter().map(|host| endpoint(host, vec![], 0)).collect())
    }

    async fn run(
        pool: &Pool,
        log: &Log,
        outcome: impl Fn(&str) -> Result<u32, Failure<&'static str>>,
    ) -> Result<u32, PoolError<&'static str>> {
        pool.call(None, |endpoint| {
            let host = endpoint.url.host_str().unwrap().to_owned();
            log.lock().unwrap().push(host.clone());
            let result = outcome(&host);
            async move { result }
        })
        .await
    }

    fn taken(log: &Log) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    #[tokio::test(start_paused = true)]
    async fn prefers_the_first_endpoint() {
        let pool = pool(&["a", "b"]);
        let log = Log::default();
        assert_eq!(run(&pool, &log, |_| Ok(7)).await, Ok(7));
        assert_eq!(taken(&log), ["a"]);
    }

    #[tokio::test(start_paused = true)]
    async fn fails_over_then_returns_after_the_cooldown() {
        let pool = pool(&["a", "b"]);
        let log = Log::default();
        let a_down = |host: &str| {
            if host == "a" {
                Err(Failure::Retry("down"))
            } else {
                Ok(1)
            }
        };
        assert_eq!(run(&pool, &log, a_down).await, Ok(1));
        assert_eq!(taken(&log), ["a", "b"]);
        assert_eq!(run(&pool, &log, |_| Ok(2)).await, Ok(2));
        assert_eq!(taken(&log), ["b"]);
        tokio::time::advance(COOLDOWN_BASE).await;
        assert_eq!(run(&pool, &log, |_| Ok(3)).await, Ok(3));
        assert_eq!(taken(&log), ["a"]);
    }

    #[tokio::test(start_paused = true)]
    async fn cooldowns_double_up_to_a_cap_and_reset_on_success() {
        let pool = pool(&["a"]);
        let log = Log::default();
        let mut expected = COOLDOWN_BASE;
        for _ in 0..8 {
            assert_eq!(
                run(&pool, &log, |_| Err(Failure::Retry("down"))).await,
                Err(PoolError::Exhausted("down"))
            );
            assert_eq!(
                run(&pool, &log, |_| Ok(0)).await,
                Err(PoolError::Unavailable(expected))
            );
            tokio::time::advance(expected).await;
            expected = (expected * 2).min(COOLDOWN_MAX);
        }
        assert_eq!(expected, COOLDOWN_MAX);
        assert_eq!(run(&pool, &log, |_| Ok(5)).await, Ok(5));
        assert_eq!(pool.status()[0].failures, 0);
        assert_eq!(
            run(&pool, &log, |_| Err(Failure::Retry("down"))).await,
            Err(PoolError::Exhausted("down"))
        );
        assert_eq!(
            run(&pool, &log, |_| Ok(0)).await,
            Err(PoolError::Unavailable(COOLDOWN_BASE))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limits_wait_as_long_as_the_server_asks() {
        let pool = pool(&["a"]);
        let log = Log::default();
        let limited = |_: &str| {
            Err(Failure::RateLimited(
                "slow down",
                Some(Duration::from_secs(30)),
            ))
        };
        assert_eq!(
            run(&pool, &log, limited).await,
            Err(PoolError::Exhausted("slow down"))
        );
        assert_eq!(
            run(&pool, &log, |_| Ok(0)).await,
            Err(PoolError::Unavailable(Duration::from_secs(30)))
        );
        let forever = |_: &str| {
            Err(Failure::RateLimited(
                "slow down",
                Some(Duration::from_hours(24)),
            ))
        };
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(
            run(&pool, &log, forever).await,
            Err(PoolError::Exhausted("slow down"))
        );
        assert_eq!(
            run(&pool, &log, |_| Ok(0)).await,
            Err(PoolError::Unavailable(COOLDOWN_MAX))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn request_errors_are_not_retried_elsewhere() {
        let pool = pool(&["a", "b"]);
        let log = Log::default();
        assert_eq!(
            run(&pool, &log, |_| Err(Failure::Fatal("insufficient funds"))).await,
            Err(PoolError::Fatal("insufficient funds"))
        );
        assert_eq!(taken(&log), ["a"]);
        assert_eq!(run(&pool, &log, |_| Ok(1)).await, Ok(1));
        assert_eq!(taken(&log), ["a"]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_endpoint_that_cannot_serve_a_request_is_skipped_without_a_cooldown() {
        let pool = pool(&["a", "b"]);
        let log = Log::default();
        let a_cannot = |host: &str| {
            if host == "a" {
                Err(Failure::Unsupported("archive only"))
            } else {
                Ok(1)
            }
        };
        assert_eq!(run(&pool, &log, a_cannot).await, Ok(1));
        assert_eq!(taken(&log), ["a", "b"]);
        assert_eq!(run(&pool, &log, |_| Ok(2)).await, Ok(2));
        assert_eq!(taken(&log), ["a"]);
        assert_eq!(
            (pool.status()[0].available, pool.status()[0].failures),
            (true, 0)
        );
        assert_eq!(
            run(&pool, &log, |_| Err(Failure::Unsupported("archive only"))).await,
            Err(PoolError::Exhausted("archive only"))
        );
        assert_eq!(run(&pool, &log, |_| Ok(3)).await, Ok(3));
    }

    #[tokio::test(start_paused = true)]
    async fn only_endpoints_with_the_needed_feature_are_used() {
        let pool = Pool::new(vec![
            endpoint("a", vec![], 0),
            endpoint("b", vec![Feature::Logs], 0),
        ]);
        let used = pool
            .call(Some(Feature::Logs), |endpoint| async move {
                Ok::<_, Failure<()>>(endpoint.url.host_str().map(str::to_owned))
            })
            .await;
        assert_eq!(used, Ok(Some("b".to_owned())));
        let none = pool
            .call(Some(Feature::History), |_| async {
                Ok::<_, Failure<()>>(())
            })
            .await;
        assert_eq!(none, Err(PoolError::NoEndpoint));
    }

    #[tokio::test(start_paused = true)]
    async fn paced_endpoints_wait_between_requests() {
        let pool = Pool::new(vec![endpoint("a", vec![], 1100)]);
        let started = Instant::now();
        let mut times = Vec::new();
        for _ in 0..3 {
            let at = pool
                .call(None, |_| async { Ok::<_, Failure<()>>(started.elapsed()) })
                .await
                .unwrap();
            times.push(at.as_millis());
        }
        assert_eq!(times, [0, 1100, 2200]);
    }

    #[tokio::test(start_paused = true)]
    async fn reports_endpoint_health() {
        let pool = pool(&["a", "b"]);
        let log = Log::default();
        let a_down = |host: &str| {
            if host == "a" {
                Err(Failure::Retry("down"))
            } else {
                Ok(1)
            }
        };
        run(&pool, &log, a_down).await.unwrap();
        let status = pool.status();
        assert_eq!((status[0].available, status[0].failures), (false, 1));
        assert_eq!((status[1].available, status[1].failures), (true, 0));
        assert_eq!(status[1].url, "https://b/");
    }
}
