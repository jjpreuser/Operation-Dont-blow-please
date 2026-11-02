# Te-connectivity-Pi

Raspberry Pi monitor app for the TE Connectivity BESS (4S3P LiFePO4 battery energy storage system in a wall cabinet). The Pi is a read-only touchscreen monitor: the MCU (BMS firmware) handles all safety; the Pi displays, logs and controls the fans. Safety must never depend on the Pi, website or internet (S11.27).

## Repo layout

- `monitor/` – Pi monitor app, Cargo workspace (empty so far; Rust, pinned toolchain per S1.2). Renamed from `Pi/` on 2026-10-29: the repo follows S1.3 (`/monitor`, `/protocol`, `/firmware`, `/docs`, DD-71); `/protocol` and `/firmware` come once the firmware team agrees to work in this repo
- `docs/design/` – Mermaid design diagrams (source of truth, mirrored to Miro) and `decisions.md` (design decision log DD-xx; read before designing/implementing)
- `docs/testing/` – test plan; `test-levels.md` defines test levels L0–L5 and Cloud
- `Research/BESS_Requirements_VX.X.pdf` – full requirements document (tables A–I referenced below live here)
- `Research/STM32_Pi_UART_Interface_Draft_V2.docx` – firmware team's MCU↔Pi UART interface (V2 is current, protocol 0x03 with MCU-calculated SoC; V1 kept for history; MCU STM32G071RB): frame envelope, TELEMETRY/FAULT_UPDATE/CONFIG/HEARTBEAT/FAULT_RESET payloads, error catalogue, CONFIG with limits and fan tables. The Pi takes battery config, limits and fan tables from CONFIG (DD-24, DD-40)

## Required tests for software

Any software written in this repo must come with these two test levels (`docs/testing/test-levels.md`, S1.9, S1.12). A change is not finished until both pass.

- **L1 unit tests:** new or changed logic gets unit tests in the same change, run on a PC without hardware (S1.5). Rust uses `cargo test`, C firmware uses Unity/Ceedling. Every error code needs trigger and recovery tests, and the fan curve needs tests.
- **L0 static checks in CI:** run automatically on every push, zero warnings. Rust: `cargo fmt --check` and `cargo clippy`. C firmware: `-Wall -Wextra`, clang-format and cppcheck. L1 tests also run in CI. If the CI for a part of the repo does not exist yet, set it up with the first code in that part.

## Miro board (requirements backlog)

Board: https://miro.com/app/board/uXjVEVbEyh4=/ ("My First Board", accessible via the Miro connector).
Contains 4 kanbans, 3 tables (incl. "Mechanical Backlog") and the Mermaid design diagrams (D1–D6, D10, D11), one column per design doc with a title above it. Status lanes: Not Started, Initiated, In Progress, blocked, in review, Sign off, Complete.

- **Kanban Software**: Pi design tasks D1–D13 (architecture, ERD, data lifecycle, fan control, lifecycle, fault handling, UART review, decision log, SoH, fan hardware, CSV/Excel test, fan spin-up test). Each card describes its design doc in `docs/design/` and the DD-xx decisions it covers; keep the card text in sync when a design doc changes. D8 is the exception: it shows only the status counts and the open (Proposed) decisions grouped by what they wait on, so update it whenever a DD is added or changes status. As of 2026-11-02: D1, D2, D5, D6 back to in review (they gained proposed DDs, found in a consistency check 2026-10-29, D2 added 2026-11-02 for DD-72 and DD-73; D1 also new channels in D1b and the `bess-fan-failsafe` crate); D3 in review (DD-45 to DD-50 and DD-74 agreed 2026-11-02, DD-75 parked); D7 in review (Pi review of UART V2 in `firmware-requests.md`, DD-66 to DD-68 proposed; Pi team agrees, then it goes to firmware); D4, D10, D13 blocked (wait for fan parts and bench tests); D9 blocked (waits for firmware: V_stop, same as D11 reminder 1); D12 Sign off (tested, DD-62 agreed); D8 In Progress (2026-10-29: status set Proposed / Agreed / Superseded / Parked, P1.6 trade-off section T1–T11 in `decisions.md`, DD-70 to DD-83 from a sweep of D1–D6, DD-84 to DD-86 for switching off a dead fan (one fan controller per fan); 2026-11-02: DD-19 revised (free open-source software only, Slint dropped), DD-87 proposed (egui + cage), DD-77 revised for it; 2026-11-02: the 22 Pi-team decisions settled by Jelle, 20 agreed, DD-77 kept proposed (Jelle will come back on it) and DD-75 parked (reason not recorded); 87 decisions, 72 agreed, 14 proposed, 1 parked; reminder cards "D10 reminder: EMC230x watchdog" and "D8 reminder: check egui + cage on the Pi 4 touchscreen (DD-87)"); D11 In Progress (Ah from V_low to the BMS 85 % stop voltage, DD-63 to DD-65 proposed); three "D11 reminder" cards blocked (V_stop from firmware, 8 A charge curve, charging pattern + accuracy).
- **Kanban general**: project cards P1.1–P1.7 and open items (OI10, OI13–OI17, OI19, OI20); OI14 Initiated; OI17 and OI19 blocked (wait for parts and tests); P1.6 In Progress (T1–T11 in `decisions.md` cover the software and fans; the electrical trade-offs are still to do); the rest Not Started.
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
