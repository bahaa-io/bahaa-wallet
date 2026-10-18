use bahaa_chain::{ChainError, Transfer};
use bahaa_types::Address;
use ruint::aliases::U256;

use super::{TonChain, amount};
use crate::cells::Account;
use crate::emulate::{self, Emulated, Emulator, Hop};
use crate::fees::{Config, Settings};
use crate::wallet::{self, Order, Token};

const PROBE: u64 = 100_000_000;
const MAX_LIBRARIES: usize = 4;

#[derive(Debug, PartialEq, Eq)]
struct Flow {
    refund: u64,
    adjustment: i128,
    refund_fees: u64,
}

pub(super) struct Contracts<'a> {
    pub config: &'a Config,
    pub settings: Settings,
    pub now: u32,
}

impl TonChain {
    pub(super) async fn token_order(
        &self,
        transfer: &Transfer,
        master: &Address,
        order: &mut Order,
        wallet: &Account,
        contracts: Contracts<'_>,
    ) -> Result<u64, ChainError> {
        let decimals = self.token_decimals(master).await?;
        let units = transfer.amount.units();
        let held = self.jetton_balance(&transfer.from, master).await?;
        if held < units {
            return Err(ChainError::Insufficient {
                symbol: self.symbol(master),
                needed: transfer.amount,
                available: amount(held, decimals)?,
            });
        }
        let ours = self.jetton_wallet(&transfer.from, master).await?;
        let theirs = self.jetton_wallet(&transfer.to, master).await?;
        if ours == theirs {
            return Err(ChainError::Invalid("send tokens to another account"));
        }
        let ours_state = self.http.account(&ours).await?;
        let theirs_state = self.http.account(&theirs).await?;
        order.bounce = true;
        order.token = Some(Token {
            wallet: ours,
            units,
            query_id: (u64::from(order.valid_until) << 32) | u64::from(order.seqno),
        });
        let params = contracts.config.encoded().ok_or_else(|| {
            ChainError::Network("the network config cannot be encoded".to_owned())
        })?;
        let run = Run {
            params: &params,
            settings: contracts.settings,
            now: contracts.now,
        };
        let parties = Parties {
            ours: (&ours, &ours_state.boc),
            theirs: (&theirs, &theirs_state.boc),
            wallet: &wallet.boc,
        };
        let mut libraries = Vec::new();
        order.nano = U256::from(PROBE);
        let probe = self.flow(order, &parties, &run, &mut libraries).await?;
        let attachment = PROBE
            .checked_sub(probe.refund)
            .and_then(|need| need.checked_mul(2))
            .ok_or_else(overflow)?;
        order.nano = U256::from(attachment);
        let flow = self.flow(order, &parties, &run, &mut libraries).await?;
        let spent = i128::from(attachment) - i128::from(flow.refund) - flow.adjustment
            + i128::from(flow.refund_fees);
        u64::try_from(spent).map_err(|_| overflow())
    }

    async fn flow(
        &self,
        order: &Order,
        parties: &Parties<'_>,
        run: &Run<'_>,
        libraries: &mut Vec<Vec<u8>>,
    ) -> Result<Flow, ChainError> {
        for _ in 0..=MAX_LIBRARIES {
            let followed = {
                let emulator = Emulator::new(run.params, libraries, run.settings, run.now)
                    .ok_or_else(|| {
                        ChainError::Network("the network config cannot be emulated".to_owned())
                    })?;
                follow(&emulator, order, parties)?
            };
            match followed {
                Followed::Done(flow) => return Ok(flow),
                Followed::Missing(hash) => libraries.push(self.http.library(&hash).await?),
            }
        }
        Err(ChainError::Network(
            "a contract needs too many libraries".to_owned(),
        ))
    }
}

pub(crate) struct Parties<'a> {
    pub ours: (&'a Address, &'a [u8]),
    pub theirs: (&'a Address, &'a [u8]),
    pub wallet: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
enum Followed {
    Done(Flow),
    Missing([u8; 32]),
}

fn follow(
    emulator: &Emulator,
    order: &Order,
    parties: &Parties<'_>,
) -> Result<Followed, ChainError> {
    let (ours, ours_state) = parties.ours;
    let (theirs, theirs_state) = parties.theirs;
    let first = match emulator.run(ours, ours_state, &wallet::delivered(order)?)? {
        Emulated::Done(hop) => hop,
        Emulated::Missing(hash) => return Ok(Followed::Missing(hash)),
    };
    if !first.success {
        return Err(refused("your token wallet", &first));
    }
    let forwarded = first
        .out
        .iter()
        .find(|message| emulate::destination(message).is_some_and(|(to, _)| to == *theirs))
        .ok_or_else(|| {
            ChainError::Rejected("your token wallet would not forward the tokens".to_owned())
        })?;
    let second = match emulator.run(theirs, theirs_state, forwarded)? {
        Emulated::Done(hop) => hop,
        Emulated::Missing(hash) => return Ok(Followed::Missing(hash)),
    };
    if !second.success {
        return Err(refused("the recipient's token wallet", &second));
    }
    let mut flow = Flow {
        refund: 0,
        adjustment: [&first, &second]
            .iter()
            .map(|hop| i128::from(hop.gas_fees) - i128::from(hop.charged_gas_fees))
            .sum(),
        refund_fees: 0,
    };
    for message in &second.out {
        if let Some((to, value)) = emulate::destination(message)
            && to == order.wallet
        {
            let back = match emulator.run_settled(&order.wallet, parties.wallet, message)? {
                Emulated::Done(hop) => hop,
                Emulated::Missing(hash) => return Ok(Followed::Missing(hash)),
            };
            flow.refund = u64::try_from(value).map_err(|_| overflow())?;
            flow.refund_fees = back.fees;
        }
    }
    Ok(Followed::Done(flow))
}

struct Run<'a> {
    params: &'a [(i32, Vec<u8>)],
    settings: Settings,
    now: u32,
}

fn refused(contract: &str, hop: &Hop) -> ChainError {
    ChainError::Rejected(match hop.exit_code {
        Some(code) => format!("{contract} would refuse this transfer with exit code {code}"),
        None => format!("{contract} would refuse this transfer"),
    })
}

fn overflow() -> ChainError {
    ChainError::Invalid("fee overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulate::tests::{OURS, SENDER, THEIRS, emulator, emulator_with, order, state, ton};

    fn parties<'a>(
        ours: &'a Address,
        theirs: &'a Address,
        states: &'a (Vec<u8>, Vec<u8>, Vec<u8>),
    ) -> Parties<'a> {
        Parties {
            ours: (ours, &states.0),
            theirs: (theirs, &states.1),
            wallet: &states.2,
        }
    }

    #[test]
    fn a_transfer_is_followed_to_the_refund() {
        let (ours, theirs) = (ton(OURS), ton(THEIRS));
        let states = (state(OURS), state(THEIRS), state(SENDER));
        let Followed::Done(flow) = follow(
            &emulator(),
            &order(1_000_000, 50_000_000),
            &parties(&ours, &theirs, &states),
        )
        .unwrap() else {
            panic!("a finished flow expected");
        };
        assert!(
            flow.refund > 40_000_000 && flow.refund < 50_000_000,
            "{flow:?}"
        );
        assert!(flow.adjustment > 0 && flow.refund_fees > 0, "{flow:?}");
    }

    #[test]
    fn a_refusing_token_wallet_stops_the_quote() {
        let (ours, theirs) = (ton(OURS), ton(THEIRS));
        let states = (state(OURS), state(THEIRS), state(SENDER));
        assert!(matches!(
            follow(
                &emulator(),
                &order(u64::MAX >> 8, 50_000_000),
                &parties(&ours, &theirs, &states),
            ),
            Err(ChainError::Rejected(reason)) if reason.starts_with("your token wallet would refuse")
        ));
        let bare = emulator_with(false);
        assert!(matches!(
            follow(
                &bare,
                &order(1_000_000, 50_000_000),
                &parties(&ours, &theirs, &states)
            ),
            Ok(Followed::Missing(_))
        ));
    }
}
