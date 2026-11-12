# Bahaa Wallet

A small, secure, non-custodial crypto wallet for EVM networks, Tron and TON, written in Rust.

## Few words

I write C and Go, and I'm learning Rust through this project. Rust is a good fit for a wallet: it's memory-safe, gives full control over secrets, and produces small, fast programs.

A big shout-out to Claude, Anthropic's AI. It built and tested most of this engine with me, explained Rust to me as we went, and kept the code clean and safe the whole way. Claude also wrote this README.

## What it is

Bahaa Wallet keeps your coins under your control.

- **Your keys never leave your machine.** Transactions are signed locally, and only the signed result is sent out.
- **No accounts, no API keys, no telemetry, no backend.** The wallet talks directly to public network nodes.
- **One recovery phrase covers every network.** TON's own 24-word phrases work too.
- **Fast and light.** It's built to be one small native program, with no browser and no web view.

Prices come from [Bahaa](https://bahaa.io), and the wallet only asks for the assets you actually hold.

## Install

### Terminal app (TUI)

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/bahaa-io/bahaa-wallet/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/bahaa-io/bahaa-wallet/main/install.ps1 | iex
```

Then run `bahaa-wallet`. You can also download the `bahaa-wallet-tui-*` file for your system from [Releases](https://github.com/bahaa-io/bahaa-wallet/releases/latest).

## Update

- **In the app:** Settings → Updates (Stable, Beta or Off).
- **Install command:** run it again.
- **By hand:** replace the file with the new one from [Releases](https://github.com/bahaa-io/bahaa-wallet/releases/latest).

Your wallets and settings are kept.

## Networks

| Network | Coin | Built-in tokens |
|---|---|---|
| BNB Smart Chain | BNB | USDT, USDC |
| Ethereum | ETH | USDT, USDC |
| Polygon | POL | USDT, USDC |
| Arbitrum | ETH | USDT, USDC |
| Avalanche | AVAX | USDT, USDC |
| Base | ETH | USDC |
| Optimism | ETH | USDT, USDC |
| Tron | TRX | USDT |
| TON | GRAM | USDT |
| Solana | SOL | USDT, USDC |
| Bitcoin | BTC | none |

You can add any other token by its contract address. Test networks (BSC testnet, Tron Nile, TON testnet and Solana Devnet) are available too, hidden until you turn them on.

## Features

- **Wallets:** create or import 12- and 24-word phrases, TON's own phrases, or a single private key for any network, with several wallets and accounts.
- **Balances:** coins and tokens on every network, valued in USD, EUR or IRT.
- **Bitcoin:** Native SegWit and Taproot, found from your phrase's history, with a fresh address for every payment.
- **Sending:** exact fees before you send, Max, memos, and warnings for look-alike addresses and dust.
- **Safe signing:** what you see is what you sign, and every send is followed until it's final. Speed up or cancel on EVM networks and Bitcoin.
- **Swaps:** swap coins and tokens on one network or across networks, through decentralized exchanges and keyless cross-network services. Every route is ranked by what you really receive, with the exact cost shown, and you can pick any of them. No app fee.
- **NFTs:** your NFTs on TON, Solana, Ethereum, Optimism and Avalanche, each checked on chain, with its picture right in the terminal. Known collections show, the rest wait in Hidden, and you choose what to show. Send them like coins.
- **Activity:** one timeline of what you sent and received, with spam flagged and hidden.
- **Privacy:** hide every balance with one switch.

## Security

- **Encrypted vault.** Your recovery phrases are locked with your password, using Argon2id and XChaCha20-Poly1305. Secrets are wiped from memory after use, and are never printed or logged.
- **Encrypted local data.** Your history, contacts, tokens and accounts are encrypted too, and only open after you unlock. The wallet locks itself after a few idle minutes.
- **Verified, not trusted.** TON balances and history are checked against validator-signed proofs. Bitcoin coins and history are checked against block headers and their proof of work. Every network reply is size-limited and read strictly.
- **Built to survive bad data.** The code that reads network data has been fuzzed with more than 100 million hostile inputs. A misbehaving server can at most fail its own request.
- **Signed updates.** The app only installs releases signed with the Bahaa release key, and checks each file's checksum first.
- **Pure Rust.** There's no OpenSSL, and unsafe code is forbidden.
- **Tested with real money** on BNB Smart Chain, Tron and TON: receiving, sending, Max, memos, multiple accounts, and restarts in the middle of a send.

## Limitations

- **Some coins received on EVM networks aren't listed:** a payout made from inside a contract when the wallet can't tell which transaction paid it, and coins that came while the app was closed longer than free servers keep old balances (minutes on Arbitrum, hours or days elsewhere). The balance is always right.
- **Short past history** for imported wallets on EVM networks and Tron, because free servers keep only recent records.
- **Some memos aren't shown:** on received Tron token transfers, and TON's encrypted comments.
- **Speed up and cancel are EVM and Bitcoin only.** Tron, TON and Solana can't replace a sent transaction.
- **Expected waits are estimates:** from the servers' fee estimates on Bitcoin, from typical block times elsewhere.
- **Bitcoin:** Native SegWit and Taproot only (not older 1… and 3… wallets), no test network yet, and coins on the way count before their first block.
- **Small Taproot coins stay put:** Bitcoin coins of 10,000 sats or less on Taproot addresses are never spent, because they often carry Ordinals or Runes.
- **Solana Token-2022 tokens** can't be added yet, only classic Solana tokens.
- **EVM memos only go with coins** sent to a plain address. With tokens and contracts, they would break the call.
- **Only listed networks** for now, and values only for assets Bahaa prices.
- **Public servers:** if a network's servers are down, that network pauses until they're back.
- **Swaps across networks go through a provider:** it holds your coins for seconds to minutes and refunds you if it fails.
- **Avalanche swaps aren't simulated before signing:** its public servers can't.
- **Only tracked tokens can be swapped.**
- **NFTs:** on five networks only and at most 256 per account and network. SVG pictures aren't shown yet. Staked or listed ones, and some compressed Solana NFTs, can't be sent.
- **Not supported yet:** staking actions, app connections and hardware wallets.

## License

GPL-3.0. See [LICENSE](LICENSE).
