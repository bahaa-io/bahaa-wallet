#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;
use std::time::Duration;

use tokio::time::Instant;

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
}

#[derive(Default)]
pub struct Report {
    failures: usize,
}

impl Report {
    pub async fn check(&mut self, name: &str, work: impl Future<Output = anyhow::Result<Outcome>>) {
        let started = Instant::now();
        let result = work.await;
        let took = took(started.elapsed());
        match result {
            Ok(outcome) => {
                println!("  ok    {took:>8}  {name:<12}  {}", outcome.summary);
                for line in outcome.lines {
                    println!("{:32}{line}", "");
                }
            }
            Err(err) => {
                self.failures += 1;
                println!("  FAIL  {took:>8}  {name:<12}  {err:#}");
            }
        }
    }

    pub fn passed(&self) -> bool {
        self.failures == 0
    }
}

pub fn section(title: &str) {
    println!("\n{title}");
}

pub fn fatal(err: &anyhow::Error) -> ExitCode {
    eprintln!("bahaa-probe: {err:#}");
    ExitCode::FAILURE
}

pub fn usage(target: &str) -> ExitCode {
    eprintln!("bahaa-probe: unknown target {target:?}, expected all or bahaa");
    ExitCode::FAILURE
}

fn took(elapsed: Duration) -> String {
    if elapsed < Duration::from_secs(10) {
        format!("{} ms", elapsed.as_millis())
    } else {
        format!("{} s", elapsed.as_secs())
    }
}
