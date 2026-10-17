mod bahaa;
mod clock;
mod evm;
mod net;
mod report;

use std::process::ExitCode;

use net::Net;
use report::Report;

#[derive(Clone, Copy)]
enum Target {
    Bahaa,
    Evm,
}

impl Target {
    const ALL: [Self; 2] = [Self::Bahaa, Self::Evm];

    fn name(self) -> &'static str {
        match self {
            Self::Bahaa => "bahaa",
            Self::Evm => "evm",
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let wanted = std::env::args().nth(1).unwrap_or_default();
    let targets = if wanted.is_empty() || wanted == "all" {
        Target::ALL.to_vec()
    } else if let Some(target) = Target::ALL.into_iter().find(|t| t.name() == wanted) {
        vec![target]
    } else {
        return report::usage(&wanted, &Target::ALL.map(Target::name));
    };
    let net = match Net::new() {
        Ok(net) => net,
        Err(err) => return report::fatal(&err),
    };
    let mut report = Report::default();
    for target in targets {
        match target {
            Target::Bahaa => bahaa::run(&net, &mut report).await,
            Target::Evm => evm::run(&net, &mut report).await,
        }
    }
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
