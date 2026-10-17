mod bahaa;
mod clock;
mod evm;
mod net;
mod report;
mod ton;
mod tron;

use std::process::ExitCode;

use net::Net;
use report::Report;

#[derive(Clone, Copy)]
enum Target {
    Bahaa,
    Evm,
    Tron,
    Ton,
}

impl Target {
    const ALL: [Self; 4] = [Self::Bahaa, Self::Evm, Self::Tron, Self::Ton];

    fn name(self) -> &'static str {
        match self {
            Self::Bahaa => "bahaa",
            Self::Evm => "evm",
            Self::Tron => "tron",
            Self::Ton => "ton",
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
            Target::Tron => tron::run(&net, &mut report).await,
            Target::Ton => ton::run(&net, &mut report).await,
        }
    }
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
