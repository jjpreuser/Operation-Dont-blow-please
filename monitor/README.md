# BESS monitor (Pi app)

The Raspberry Pi app of the TE Connectivity BESS: a read-only touchscreen
monitor. It reads the BMS (the MCU) over UART, shows the live values, logs
them to SQLite and drives the fans. All safety stays on the MCU; nothing
here is needed to keep the battery safe (S11.27).

The design lives in [`docs/design/`](../docs/design/) (start with
[`architecture.md`](../docs/design/architecture.md) and the decision log
[`decisions.md`](../docs/design/decisions.md)); the tests per requirement are
in [`docs/testing/test-plan.md`](../docs/testing/test-plan.md).

## Crates

| Crate | What it is |
|---|---|
| [`protocol`](protocol/) | The MCU ↔ Pi UART protocol V2 (version `0x03`): frames, CRC-16, typed messages, CONFIG decoding and checks. No I/O, no dependencies. `reference` holds the UART draft's example frames; the `sim` feature adds `SimMcu`, a simulated MCU end of the link. |
| [`bess-core`](bess-core/) | The core service: link supervisor, fan duty, live state, event detector, the SQLite log, one tokio task per box of the architecture, and the `bess-core` binary. |
| [`bess-display`](bess-display/) | The egui touchscreen app. For now a preview: it runs the real core against a demo MCU, with demo controls beside the screen. |

## Build and test

The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
(S1.2); `rustup` installs it on the first `cargo` call. Run everything from
this folder.

```sh
cargo build --workspace
cargo test --workspace
```

The tests need no hardware (S1.5): the end-to-end tests run every core task
against `SimMcu` on tokio's paused clock.

Before pushing, run what CI runs ([`monitor-ci.yml`](../.github/workflows/monitor-ci.yml));
every step must pass with zero warnings:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo test --workspace --locked
cargo deny --locked check   # cargo install cargo-deny
```

The lint levels (`clippy::all` and `clippy::pedantic` as errors) are set in
[`Cargo.toml`](Cargo.toml); a justified exception is a local
`#[expect(lint, reason = "...")]`. [`deny.toml`](deny.toml) holds the allowed
licences of dependencies (DD-19): adding one is a design decision, recorded
in `decisions.md` first.

## Run

**Display preview** (any PC):

```sh
cargo run -p bess-display            # 720 × 1280 portrait screen, scaled down, with demo controls
cargo run -p bess-display -- --kiosk # full-screen at 1:1, as on the Pi
```

**Core service** (on the Pi; the UART is Linux only):

```sh
cargo run --release -p bess-core -- [--db PATH] [--serial DEVICE]
```

Defaults: database `/var/lib/bess/bess.db`, MCU UART `/dev/ttyAMA0`
(raw 115200 8N1, wiring in [`pi-pinout.md`](../docs/design/pi-pinout.md)).
It stops cleanly on SIGTERM or Ctrl-C. The systemd units, the settings file
and the web dashboard are not written yet.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Dependencies are limited to
free, open-source, permissive licences (DD-19), checked by `cargo deny`.
