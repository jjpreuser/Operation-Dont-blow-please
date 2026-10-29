# Te-connectivity-Pi

Raspberry Pi monitor app for the TE Connectivity BESS (4S3P LiFePO4 battery energy storage system in a wall cabinet). The Pi is a read-only touchscreen monitor: the MCU (BMS firmware) handles all safety; the Pi displays, logs and controls the fans. Safety must never depend on the Pi, website or internet (S11.27).

## IMPORTANT: Miro sync pending (do this first)

On 2026-10-29 the Miro board was checked against `docs/design/`. The repo was fixed the same day, but **the board was not**: the Miro MCP daily limit ran out (Free plan: 100 tool calls per day for the whole organisation, reset the next day). **At the start of the next session, before any other Miro work, offer to do this sync.** Read every card or diagram before changing it. When all items are done, delete this section and the "Miro sync" notes further down.

Kanban Software cards:

1. **D1, D5, D6 → "in review"** (D2 stays Complete). D1 text: D1b channels added (`uart_rx`, `clock_manager`, `fan_controller` and `api_server` write to `db_writer`; E12 from `fan_controller` to `event_detector`; "re-enable fan" from `api_server` to `fan_controller`; RESET_ACK from `uart_rx` to `api_server`), fifth crate `bess-fan-failsafe`, DD-71 proposed again; decision list as in `architecture.md` §4 (DD-14 to DD-19, DD-26, DD-27, DD-40, DD-42). D5 and D6 text: in review for the decisions proposed 2026-10-29.
2. **D8**: 86 decisions, 52 agreed, 34 proposed; DD-71 (revised: failsafe crate) joins group "(1) Pi team only" (now 22); DD-19 is being re-decided (T2: the cabinet Pi counts as an embedded system under Slint's royalty-free licence).
3. **D4**: D4c has a FAN_OFF state; DD-54 notes the DD-85 exception. **D2**: `core_state.energy_baseline` removed. **D3**: open points cloud heartbeat (S11.15) and device ID (S11.11). **D11**: drop "option 3 on the 'D11 SoH explainer' frame" (that frame no longer exists); point to trade-off T11 in `decisions.md`.

Diagrams (copy from the Mermaid in `docs/design/`):

4. D1a: `bess-fan-failsafe` node. D1b: the new channels (item 1).
5. D2 ERD: drop `energy_baseline`; `event.code` "1-19, 20-32 = unknown bit (DD-68)"; `snapshot_id` "NULL if no snapshot".
6. D4c: FAN_OFF state and its note.
7. D6a: `cloud_uploader` reads with its own read-only connection, uploads the event within 30 s, writes its watermark through `db_writer` and sends the snapshot once complete (DD-50); email references S11.18 and S11.25.
8. Title text areas: D1 + D2 "D1 in review, D2 agreed"; D5 and D6 "agreed, in review for the 2026-10-29 DDs".
9. **New column "D11 SoH (proposed)"**: title text area plus the state diagram from `soh.md` §2.
10. Optional: D5c gets the CONNECTING retry loop and "SSE closed".

Other cards:

11. **S7.1** (Software Backlog): rewrite for the STM32G071RB (decided 2026-10-29; the UART draft V2 also asks for this update). Proposed text, to confirm with the user before writing because it drops Keil: "The MCU (STM32G071RB; NUCLEO-G071RB board for development, if selected) shall be programmable and debuggable through SWD: on the development board with its on-board ST-LINK over USB, and on the BMS PCB through a 10-pin Cortex Debug header (3.3 V) using an ST-LINK or another Arm SWD probe, with STM32CubeIDE (STM32CubeMX for configuration)." Value, MoSCoW and verification stay as they are.
12. **P1.6** (Kanban general): In Progress; the description notes that the software and fan trade-offs are T1–T11 in `decisions.md` and the electrical ones are still to do.
13. **Add the missing cards E2.8** (NTC4 at the air inlet, if fitted) **and S11.10**, with the text from the requirements PDF: ask the user to paste it (no PDF tool on this machine) or to add the cards.
14. Check the 7th text area on the board (not found before the limit ran out) and that the D12 card title (stored as rich-text JSON) renders correctly.

Miro call budget: read many widgets in one `canvas_read_as_svg` call (a list of `widget_ids`), and read the requirement tables with `table_list_rows` in pages of about 40 rows (a larger page failed on the Electrical table).

## Repo layout

- `monitor/` – Pi monitor app, Cargo workspace (empty so far; Rust, pinned toolchain per S1.2). Renamed from `Pi/` on 2026-10-29: the repo follows S1.3 (`/monitor`, `/protocol`, `/firmware`, `/docs`, DD-71); `/protocol` and `/firmware` come once the firmware team agrees to work in this repo
- `docs/design/` – Mermaid design diagrams (source of truth, mirrored to Miro) and `decisions.md` (design decision log DD-xx; read before designing/implementing)
- `Research/BESS_Requirements_VX.X.pdf` – full requirements document (tables A–I referenced below live here)
- `Research/STM32_Pi_UART_Interface_Draft_V2.docx` – firmware team's MCU↔Pi UART interface (V2 is current, protocol 0x03 with MCU-calculated SoC; V1 kept for history; MCU STM32G071RB): frame envelope, TELEMETRY/FAULT_UPDATE/CONFIG/HEARTBEAT/FAULT_RESET payloads, error catalogue, CONFIG with limits and fan tables. The Pi takes battery config, limits and fan tables from CONFIG (DD-24, DD-40)

## Miro board (requirements backlog)

Board: https://miro.com/app/board/uXjVEVbEyh4=/ ("My First Board", accessible via the Miro connector).
Contains 4 kanbans, 3 tables (incl. "Mechanical Backlog") and the Mermaid design diagrams (D1–D6, D10; a D11 column is to be added, decided 2026-10-29), one column per design doc with a title above it. Status lanes: Not Started, Initiated, In Progress, blocked, in review, Sign off, Complete.

- **Kanban Software**: Pi design tasks D1–D13 (architecture, ERD, data lifecycle, fan control, lifecycle, fault handling, UART review, decision log, SoH, fan hardware, CSV/Excel test, fan spin-up test). Each card describes its design doc in `docs/design/` and the DD-xx decisions it covers; keep the card text in sync when a design doc changes. D8 is the exception: it shows only the status counts and the open (Proposed) decisions grouped by what they wait on, so update it whenever a DD is added or changes status. As of 2026-10-29: D2 Complete; D1, D5, D6 back to in review (decided 2026-10-29 in a consistency check: they gained proposed DDs, D1 also new channels in D1b and the `bess-fan-failsafe` crate; the board still shows them Complete until the Miro sync); D3 in review (proposed, DD-45 to DD-50, DD-74, DD-75); D7 in review (Pi review of UART V2 in `firmware-requests.md`, DD-66 to DD-68 proposed; Pi team agrees, then it goes to firmware); D4, D10, D13 blocked (wait for fan parts and bench tests); D9 blocked (waits for firmware: V_stop, same as D11 reminder 1); D12 Sign off (tested, DD-62 agreed); D8 In Progress (2026-10-29: status set Proposed / Agreed / Superseded / Parked, P1.6 trade-off section T1–T11 in `decisions.md`, DD-70 to DD-83 from a sweep of D1–D6, DD-84 to DD-86 for switching off a dead fan (one fan controller per fan); 86 decisions, 52 agreed, 34 proposed (DD-71 proposed again 2026-10-29 with the `bess-fan-failsafe` crate; the D8 card still shows 53/33 until the Miro sync); reminder cards "D10 reminder: EMC230x watchdog" and "D8 reminder: choose the touchscreen UI toolkit (Slint licence)", to decide in the next session); D11 In Progress (Ah from V_low to the BMS 85 % stop voltage, DD-63 to DD-65 proposed); three "D11 reminder" cards blocked (V_stop from firmware, 8 A charge curve, charging pattern + accuracy).
- **Kanban general**: project cards P1.1–P1.7 and open items (OI10, OI13–OI17, OI19, OI20); OI14 Initiated; OI17 and OI19 blocked (wait for parts and tests); P1.6 to go to In Progress (decided 2026-10-29: T1–T11 in `decisions.md` cover the software and fans; Miro sync pending); the rest Not Started.
- **Kanban electrical**, **Kanban Mechanical**: empty.
- **Tables**: the requirement cards (~250: Software Backlog 134, Electrical 82, Mechanical Backlog 25). Each card: ID + title, description, MoSCoW (Must, Should, Could; some marked with *), verification method (I = inspection, T = test, D = demonstration, A = analysis). Cards E2.8 (NTC4 at the air inlet) and S11.10 are missing by mistake and still have to be added.

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
