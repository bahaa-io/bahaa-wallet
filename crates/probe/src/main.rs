mod bahaa;
mod net;
mod report;

use std::process::ExitCode;

use net::Net;
use report::Report;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let target = std::env::args().nth(1).unwrap_or_default();
    let net = match Net::new() {
        Ok(net) => net,
        Err(err) => return report::fatal(&err),
    };
    let mut report = Report::default();
    match target.as_str() {
        "" | "all" | "bahaa" => bahaa::run(&net, &mut report).await,
        other => return report::usage(other),
    }
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
