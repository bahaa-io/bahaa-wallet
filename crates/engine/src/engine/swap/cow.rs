use bahaa_chain::{Approval, Step, Transfer};
use bahaa_store::{SentStatus, SwapRecord, SwapStatus};
use bahaa_swap::{Order, Request, Route, SwapError, TypedOrder};

use super::{Ordered, Picked, Quote, SwapLink, Swappers, Unavailable, network_fee};
use crate::chains::AnyChain;
use crate::engine::EngineError;
use crate::engine::send::Planned;

pub(super) async fn listed(
    chain: &AnyChain,
    swappers: &Swappers,
    account: u32,
    asked: &Request,
    (provider, mut route, order): (bahaa_swap::Provider, Route, Order),
) -> Result<Quote, Unavailable> {
    let approval = approval(swappers, account, asked, &order);
    let (_, fee) = chain
        .prepare_approval(&approval)
        .await
        .map_err(|err| Unavailable {
            provider: provider.name(),
            reason: err.to_string(),
        })?;
    if !fee.expected().is_zero() {
        network_fee(&mut route, &fee);
    }
    Ok((provider, route, Some(order)))
}

pub(super) async fn ordered(
    chain: &AnyChain,
    swappers: &Swappers,
    picked: &Picked,
    request: &mut Request,
    (mut route, order): (Route, Order),
) -> Result<Ordered, EngineError> {
    request.amount = order.amount;
    let typed = order
        .typed
        .clone()
        .ok_or(SwapError::Untrusted("the route has no order to sign"))?;
    let approval = approval(swappers, picked.account, request, &order);
    let (approvals, fee) = chain.prepare_approval(&approval).await?;
    if !fee.expected().is_zero() {
        network_fee(&mut route, &fee);
    }
    let mut steps: Vec<Step> = approvals
        .iter()
        .map(|plan| plan.planned().step.clone())
        .collect();
    steps.push(Step::Swap);
    let transfer = Transfer {
        account: picked.account,
        from: request.from,
        to: order.deposit,
        asset: request.sell.clone(),
        amount: order.amount,
        max: false,
        memo: None,
    };
    Ok(Ordered {
        route,
        order,
        plan: Planned::Order {
            approvals,
            typed: Box::new(typed),
            account: picked.account,
            from: request.from,
        },
        transfer,
        fee,
        steps,
        checked: false,
    })
}

fn approval(swappers: &Swappers, account: u32, request: &Request, order: &Order) -> Approval {
    Approval {
        account,
        from: request.from,
        asset: request.sell.clone(),
        spender: swappers.cow.relayer(),
        amount: order.amount,
    }
}

pub(in crate::engine) fn record(
    link: SwapLink,
    typed: &TypedOrder,
    signature: &[u8; 65],
    approval: Option<String>,
) -> SwapRecord {
    let SwapLink { request, order } = link;
    let now = bahaa_chain::now();
    SwapRecord {
        id: typed.uid(),
        network: request.sell.network().clone(),
        provider: order.provider.key().to_owned(),
        reference: typed.uid(),
        from: request.from,
        to: request.to,
        deposit: order.deposit,
        sell: request.sell,
        amount: order.amount,
        buy: request.buy,
        expected: order.expected,
        minimum: order.minimum,
        created_at: now,
        updated_at: now,
        status: SwapStatus::Waiting,
        order: Some(typed.body(signature).to_string()),
        approval,
    }
}

pub(super) async fn placed(
    swappers: &Swappers,
    swap: SwapRecord,
    body: &str,
    approval: Option<SentStatus>,
) -> Option<SwapRecord> {
    match approval {
        None | Some(SentStatus::Confirmed) => {}
        Some(SentStatus::Failed(_) | SentStatus::Dropped) => {
            return Some(SwapRecord {
                order: None,
                status: SwapStatus::Failed(
                    "the approval did not go through, nothing was swapped".to_owned(),
                ),
                updated_at: bahaa_chain::now(),
                ..swap
            });
        }
        Some(SentStatus::Sending | SentStatus::Pending) => return None,
    }
    let status = match swappers
        .cow
        .place(&swap.network, body, &swap.reference)
        .await
    {
        Ok(()) => SwapStatus::Swapping,
        Err(SwapError::Refused(reason)) => SwapStatus::Failed(reason),
        Err(_) => return None,
    };
    Some(SwapRecord {
        order: None,
        status,
        updated_at: bahaa_chain::now(),
        ..swap
    })
}
