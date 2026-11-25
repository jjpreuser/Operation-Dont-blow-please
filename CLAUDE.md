# Te-connectivity-Pi

Raspberry Pi monitor app for the TE Connectivity BESS (4S3P LiFePO4 battery energy storage system in a wall cabinet). The Pi is a read-only touchscreen monitor: the MCU (BMS firmware) handles all safety; the Pi displays, logs and controls the fans. Safety must never depend on the Pi, website or internet (S11.27).

## Repo layout

- `monitor/` – Pi monitor app, Cargo workspace (Rust, toolchain pinned in `rust-toolchain.toml` per S1.2). Renamed from `Pi/` on 2026-10-29: the repo follows S1.3 (`/monitor`, `/protocol`, `/firmware`, `/docs`, DD-71); `/protocol` and `/firmware` come once the firmware team agrees to work in this repo. Crates:
  - `protocol` – UART frames, CRC-16, typed messages, CONFIG decoding and §7 validation (Miro card C1).
  - `bess-core` – the core service. I/O-free logic modules that run in `cargo test`: `link` = link supervisor (C2); `fan` = fan duty rules and curves (C3); `sample`, `state`, `colour`, `faults`, `live` = the live state for the screen (C4); `signals`, `ring`, `aggregate`, `wallclock`, `db` = the SQLite store with the 1 h ring buffer and the 10-min / daily aggregates (C5; `rusqlite` with bundled SQLite, `jiff` for time because RustSec marks `chrono` unmaintained, DD-95, DD-96); `detector` = event detector with fault snapshots (C6, DD-97). The running core (C7, DD-98, DD-99): `tasks` (one tokio task per D1 box), `runtime`, `writer` (the `db_writer` thread) and the `bess-core` binary (`main.rs`); raw 115200 8N1 on `/dev/ttyAMA0` through rustix (Linux only).
  - `bess-display` – egui preview with a simulated MCU (C4, DD-94): `cargo run -p bess-display` opens the 720 × 1280 portrait screen scaled down with demo controls, `--kiosk` full-screen 1:1.
  - CI in `.github/workflows/monitor-ci.yml` runs fmt, clippy with `pedantic`, rustdoc, `cargo test` and cargo-deny (`monitor/deny.toml`, DD-19 licence list).
- `docs/design/` – Mermaid design diagrams (source of truth, mirrored to Miro), `decisions.md` (design decision log DD-xx; read before designing/implementing) and `pi-pinout.md` (which Pi 4 header pin goes where: MCU UART, both EMC2101 I2C buses, fan tachs, RTC, touchscreen power; DD-88, DD-89; keep in step with `fan-hardware.md` §2)
- `docs/testing/` – `test-levels.md` defines test levels L0–L5 and Cloud; `test-plan.md` lists the tests per requirement (firmware, combined, non-firmware) and, in section 5, the tests already written
- `Research/BESS_Requirements_VX.X.pdf` – full requirements document (tables A–I referenced below live here)
- `Research/STM32_Pi_UART_Interface_Draft_V2.docx` – firmware team's MCU↔Pi UART interface (V2 is current, protocol 0x03 with MCU-calculated SoC; V1 kept for history; MCU STM32G071RB): frame envelope, TELEMETRY/FAULT_UPDATE/CONFIG/HEARTBEAT/FAULT_RESET payloads, error catalogue, CONFIG with limits and fan tables. The Pi takes battery config, limits and fan tables from CONFIG (DD-24, DD-40)

## Branches

Work from `development` (created 2026-11-17): start each change on its own branch off `development` (for example `monitor/<topic>` or `ci/<topic>`) and open a pull request into `development` for review on GitHub. `main` is only updated from `development`. Don't commit straight to `development` or `main`.

## Required tests for software

Any software written in this repo must come with these two test levels (`docs/testing/test-levels.md`, S1.9, S1.12). A change is not finished until both pass.

- **L1 unit tests:** new or changed logic gets unit tests in the same change, run on a PC without hardware (S1.5). Rust uses `cargo test`, C firmware uses Unity/Ceedling. Every error code needs trigger and recovery tests, and the fan curve needs tests.
- **L0 static checks in CI:** run automatically on every push, zero warnings. Rust: `cargo fmt --check`, `cargo clippy` with `clippy::all` and `clippy::pedantic` as errors (set in `monitor/Cargo.toml`; a justified exception is a local `#[expect(lint, reason = "...")]`), `cargo doc` with `-D warnings`, `--locked` builds and `cargo deny check`. C firmware: `-Wall -Wextra`, clang-format and cppcheck. L1 tests also run in CI. If the CI for a part of the repo does not exist yet, set it up with the first code in that part.

## Miro board (requirements backlog)

Board: https://miro.com/app/board/uXjVEVbEyh4=/ ("My First Board", accessible via the Miro connector).
Contains 4 kanbans, 3 tables (incl. "Mechanical Backlog") and the Mermaid design diagrams (D1–D6, D10, D11), one column per design doc with a title above it. Status lanes: Not Started, Initiated, In Progress, blocked, in review, Sign off, Complete.

- **Kanban Software**: Pi design cards D1–D13 (architecture, ERD, data lifecycle, fan control, lifecycle, fault handling, UART review, decision log, SoH, fan hardware, CSV/Excel test, fan spin-up test), code cards C1–C7 (protocol crate, link supervisor, fan duty rules, sample and display logic + display preview, database, event detector, bess-core runtime) and reminder cards ("D8 reminder", "D10 reminder", "D11 reminder"). Rules:
  - Each D card describes its design doc in `docs/design/` and the DD-xx decisions it covers; keep the card text in sync when a design doc changes.
  - D8 is the exception: it shows only the status counts and the open (Proposed) decisions grouped by what they wait on, so update it whenever a DD is added or changes status.
  - Each C card says what is merged (with the PR number) and what is still open; keep its text in sync with the code in `monitor/`.
  - The current status lives in three places; read them rather than a copy here: the card's lane and text, the "Status:" line at the top of each design doc, and the status column of `decisions.md`.
- **Requirements tracker (Pi)** (kanban below Kanban Software, created 2026-11-17): one card per Pi requirement, the 99 of `test-plan.md` sections 2 (combined) and 3 (non-firmware), without the S11.10 stub; the 35 firmware-only requirements are not on it. Lanes (no Not Started lane, removed 2026-11-17 on Jelle's request: every card sits where its real state is): Designed (covered by design docs and DDs, also when only the data side is designed and the page or screen layout is still to draw), In Progress (code being written), Tested (code done and L0–L2 pass, higher test levels still to do), Done (passes every level in the test plan), Blocked. Each card holds what is done and what is left, "Done when" from the test plan, MoSCoW and verification; columns Priority (Must = High, Should = Medium, Could = Low), Tags (area plus "Needs firmware"), Test level, Design (D-cards, DDs) and Code (C-cards). Move a card when its design, code or tests change. The Software Backlog table keeps the full requirement text; its own Status column is unused (all Not Started).
- **Kanban general**: project cards P1.1–P1.7 and open items (OI10, OI13–OI17, OI19, OI20). P1.6 (trade-offs) is partly in `decisions.md` (T1–T11 cover the software and fans; the electrical trade-offs are still to do).
- **Kanban electrical**, **Kanban Mechanical**: empty.
- **Tables**: the requirement cards (~250: Software Backlog 135, Electrical 83, Mechanical Backlog 25). Each card: ID + title, description, MoSCoW (Must, Should, Could; some marked with *), verification method (I = inspection, T = test, D = demonstration, A = analysis). E2.8 (NTC4 at the air inlet) is a marked draft: requirements V3 has no E2.8 row, only a reference in M2.3, so the requirements owner still has to confirm its text, MoSCoW and verification. S11.10 is a stub: the requirement moved to P1.7.

| Prefix | Area |
|---|---|
| P1.x | Project: deliverables, safety docs, suppliers, TE parts, 12 A upgrade path, TE IT approval for external hosting |
| OI | Open issues / TBDs (fuses, cabinet dims, wiring, thermostat set point, storage SoC, slew-rate limits) |
| S1 | Software process: C11 firmware, Rust Pi app, repo structure (`/firmware`, `/monitor`, `/protocol`, `/docs`), zero warnings, MISRA, CI, semver |
| S2–S4 | Firmware config, protection logic, error code table (Table A, single source + CI check) |
| S5 | Telemetry + MCU↔Pi UART link: binary frames with CRC-16, spec in `/protocol`, Rust tests decoding C reference frames |
| S6 | Fan control on the Pi: temperature table interpolation, max(battery, air), hysteresis, 100 % on E05/E17, fan fault E12, hardware fallback |
| S7 | Debug/tooling; S7.4 Pi auto-starts monitor app full-screen |
| S8 | Pi display: voltages, current, temps, cell groups, faults, state, Table B values, red/yellow colouring, "BMS communication lost", time to empty/full |
| S9 | Pi-served offline dashboard: Overview, Cells, Graphs (time ranges, tooltips, min-max bands), Fault log, Event log, System info, PIN-protected actions |
| S10 | Pi database: SQLite WAL, 1 h of 1 s data in RAM, 10-min min/avg/max, daily compaction, fault snapshots (±60 s), retention, size limit, SoH, UTC timestamps, CSV export (Excel-friendly), backups, optional cloud upload, NTP + RTC |
| S11 | Cloud website: login (TE SSO/admin), roles, graphs, logs, encrypted upload, heartbeat, email alerts |
| E1–E11 | Electrical: battery, measurements, protection (Table E), balancing, charger, USB outputs, E-stop, power supply, PCB, wiring/EMC, mains |
| M1–M5 | Mechanical: cabinet, layout, sensor mounting, environment, thermal tests |

**Pi-relevant cards:** S1.2, S5.8–S5.11, S6, S7.4, S8, S9, S10 (and S11 for the upload side).
