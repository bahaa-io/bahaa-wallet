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

You can add any other token by its contract address. Test networks (BSC testnet, Tron Nile and TON testnet) are available too, hidden until you turn them on.

## Features

- **Wallets:** create or import 12- and 24-word phrases, TON's own phrases too, with several wallets and accounts.
- **Balances:** coins and tokens on every network, valued in USD, EUR or IRT.
- **Sending:** exact fees before you send, Max, memos, and warnings for look-alike addresses and dust.
- **Safe signing:** what you see is what you sign, and every send is followed until it's final. Speed up or cancel on EVM networks.
- **Activity:** one timeline of what you sent and received, with spam flagged.

## Security

- **Encrypted vault.** Your recovery phrases are locked with your password, using Argon2id and XChaCha20-Poly1305. Secrets are wiped from memory after use, and are never printed or logged.
- **Encrypted local data.** Your history, contacts, tokens and accounts are encrypted too, and only open after you unlock. The wallet locks itself after a few idle minutes.
- **Verified, not trusted.** TON balances and history are checked against validator-signed proofs. Every network reply is size-limited and read strictly.
- **Built to survive bad data.** The code that reads network data has been fuzzed with more than 100 million hostile inputs. A misbehaving server can at most fail its own request.
- **Pure Rust.** There's no OpenSSL, and unsafe code is forbidden.
- **Tested with real money** on BNB Smart Chain, Tron and TON: receiving, sending, Max, memos, multiple accounts, and restarts in the middle of a send.

## Limitations

- **Coins received on EVM networks** show in the balance but not in activity. Finding them needs paid indexing, and the wallet uses no API keys.
- **Short past history** for imported wallets on EVM networks and Tron, because free servers keep only recent records.
- **Some memos aren't shown:** on received Tron token transfers, and TON's encrypted comments.
- **Speed up and cancel are EVM only.** Tron and TON can't replace a sent transaction.
- **EVM memos only go with coins** sent to a plain address. With tokens and contracts, they would break the call.
- **Only listed networks** for now, and values only for assets Bahaa prices.
- **Public servers:** if a network's servers are down, that network pauses until they're back.
- **Not supported yet:** NFTs, swaps, staking actions, app connections and hardware wallets.

## License

GPL-3.0. See [LICENSE](LICENSE).
