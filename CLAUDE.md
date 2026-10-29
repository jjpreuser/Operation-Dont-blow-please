# Te-connectivity-Pi

Raspberry Pi monitor app for the TE Connectivity BESS (4S3P LiFePO4 battery energy storage system in a wall cabinet). The Pi is a read-only touchscreen monitor: the MCU (BMS firmware) handles all safety; the Pi displays, logs and controls the fans. Safety must never depend on the Pi, website or internet (S11.27).

## Repo layout

- `Pi/` – Pi monitor app (empty so far; Rust, pinned toolchain per S1.2)
- `docs/design/` – Mermaid design diagrams (source of truth, mirrored to Miro) and `decisions.md` (design decision log DD-xx; read before designing/implementing)
- `Research/BESS_Requirements_VX.X.pdf` – full requirements document (tables A–I referenced below live here)
- `Research/STM32_Pi_UART_Interface_Draft_V2.docx` – firmware team's MCU↔Pi UART interface (V2 is current, protocol 0x03 with MCU-calculated SoC; V1 kept for history; MCU STM32G071RB): frame envelope, TELEMETRY/FAULT_UPDATE/CONFIG/HEARTBEAT/FAULT_RESET payloads, error catalogue, CONFIG with limits and fan tables. The Pi takes battery config, limits and fan tables from CONFIG (DD-24, DD-40)

## Miro board (requirements backlog)

Board: https://miro.com/app/board/uXjVEVbEyh4=/ ("My First Board", accessible via the Miro connector).
Contains 4 kanbans, 3 tables (incl. "Mechanical Backlog") and the Mermaid design diagrams (D1–D6, D10), one column per design doc with a title above it. Status lanes: Not Started, Initiated, In Progress, blocked, in review, Sign off, Complete.

- **Kanban Software**: Pi design tasks D1–D13 (architecture, ERD, data lifecycle, fan control, lifecycle, fault handling, UART review, decision log, SoH, fan hardware, CSV/Excel test, fan spin-up test). Each card describes its design doc in `docs/design/` and the DD-xx decisions it covers; keep the card text in sync when a design doc changes. As of 2026-10-29: D1, D2, D5, D6 Complete; D3 in review (proposed, DD-45 to DD-50); D7 in review (Pi review of UART V2 in `firmware-requests.md`, DD-66 to DD-68 proposed; Pi team agrees, then it goes to firmware); D4, D10, D13 blocked (wait for fan parts and bench tests); D9 blocked (waits for firmware: V_stop, same as D11 reminder 1); D12 Sign off (tested, DD-62 agreed); D8 In Progress; D11 In Progress (Ah from V_low to the BMS 85 % stop voltage, DD-63 to DD-65 proposed); three "D11 reminder" cards blocked (V_stop from firmware, 8 A charge curve, charging pattern + accuracy).
- **Kanban general**: project cards P1.1–P1.7 and open items (OI10, OI13–OI17, OI19, OI20); OI14 Initiated; OI17 and OI19 blocked (wait for parts and tests); the rest Not Started.
- **Kanban electrical**, **Kanban Mechanical**: empty.
- **Tables**: the requirement cards (~250). Each card: ID + title, description, MoSCoW (Must/Should), verification method (I = inspection, T = test).

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
