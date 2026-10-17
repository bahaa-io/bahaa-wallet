#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;
use std::time::Duration;

use tokio::time::Instant;

const PROBLEM_WIDTH: usize = 110;

pub struct Outcome {
    summary: String,
    lines: Vec<String>,
}

impl Outcome {
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            lines: Vec::new(),
        }
    }

    pub fn line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    pub fn extras(&mut self, extras: Extras) {
        self.lines.push(extras.parts.join(", "));
        self.lines.extend(extras.problems);
    }
}

#[derive(Default)]
pub struct Extras {
    parts: Vec<String>,
    problems: Vec<String>,
}

impl Extras {
    pub fn add(&mut self, name: &str, result: anyhow::Result<String>) {
        match result {
            Ok(text) => self.parts.push(format!("{name} {text}")),
            Err(err) => {
                self.parts.push(format!("{name} no"));
                let reason: String = format!("{err:#}").chars().take(PROBLEM_WIDTH).collect();
                self.problems.push(format!("{name}: {reason}"));
            }
        }
    }
}

#[derive(Default)]
pub struct Report {
    failures: usize,
}

impl Report {
    pub async fn check(&mut self, name: &str, work: impl Future<Output = anyhow::Result<Outcome>>) {
        let (elapsed, result) = timed(work).await;
        self.record(name, elapsed, result);
    }

    pub fn record(&mut self, name: &str, elapsed: Duration, result: anyhow::Result<Outcome>) {
        let took = took(elapsed);
        match result {
            Ok(outcome) => {
                println!("  ok    {took:>8}  {name:<16}  {}", clean(&outcome.summary));
                for line in outcome.lines {
                    println!("{:36}{}", "", clean(&line));
                }
            }
            Err(err) => {
                self.failures += 1;
                println!(
                    "  FAIL  {took:>8}  {name:<16}  {}",
                    clean(&format!("{err:#}"))
                );
            }
        }
    }

    pub fn passed(&self) -> bool {
        self.failures == 0
    }
}

pub async fn timed<T>(work: impl Future<Output = T>) -> (Duration, T) {
    let started = Instant::now();
    let value = work.await;
    (started.elapsed(), value)
}

pub fn section(title: &str) {
    println!("\n{title}");
}

pub fn fatal(err: &anyhow::Error) -> ExitCode {
    eprintln!("bahaa-probe: {}", clean(&format!("{err:#}")));
    ExitCode::FAILURE
}

pub fn usage(target: &str, known: &[&str]) -> ExitCode {
    eprintln!(
        "bahaa-probe: unknown target {target:?}, expected all, {}",
        known.join(", ")
    );
    ExitCode::FAILURE
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if unsafe_to_print(c) { ' ' } else { c })
        .collect()
}

fn unsafe_to_print(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn took(elapsed: Duration) -> String {
    if elapsed < Duration::from_secs(10) {
        format!("{} ms", elapsed.as_millis())
    } else {
        format!("{} s", elapsed.as_secs())
    }
}
