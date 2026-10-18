# Bahaa Wallet

A small, secure, non-custodial crypto wallet for the terminal. Windows, macOS, and Linux.

> Early development. Not ready for real funds. Wait for v0.6

## Goals

- Your keys never leave your machine. No accounts, no API keys, no telemetry.
- Full mouse and keyboard support.
- Send and receive, live balances and rates, precise fees.
- Lightweight and fast.

## Networks

EVM (BNB Smart Chain, Ethereum, Polygon, Arbitrum, Avalanche, Base, Optimism), Tron, and TON (FOR NOW).

## About

I write C and Go, and I'm learning Rust through this project, with help from Claude (Anthropic's AI). Rust is a good fit for a wallet: it's memory-safe, gives full control over secrets, and produces small, fast binaries.

Rates are provided by [Bahaa](https://bahaa.io).

## Roadmap

The engine comes first, for every network, and the app is built on top of it. The engine is done; the app is next.

- [x] 0. Setup and endpoint tests
- [x] 1. Keys, mnemonics, encrypted vault
- [x] 2. Engine
- [x] 3. EVM networks
- [x] 4. Base and Optimism
- [x] 5. Tron
- [x] 6. TON
- [ ] 7. App interface
- [ ] 8. Developer Mode, hardening and public beta (0.6)
- [ ] 1.0: security audit, hardware wallets

## License

GPL-3.0. See [LICENSE](LICENSE).
