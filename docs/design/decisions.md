# Design decisions (D8)

Decisions for the Pi monitor app, made during the design phase. This file is the source of truth; the Miro card D8 (Kanban Software) shows only the counts and the open decisions. The key choices have their options and reasons in [Trade-offs for key choices](#trade-offs-for-key-choices-p16) (P1.6).

**Status** is one of:

| Status | Meaning |
|---|---|
| Proposed *date* | Written down, not yet agreed by the Pi team (and, where the text says so, by firmware or electronics). |
| Agreed *date* | Agreed; the design builds on it. |
| Superseded by DD-xx *date* | Replaced by another decision; kept so old references still resolve. |
| Parked *date* | Deliberately not done for now; the text says what would bring it back. |

The date is that of the last status change. A decision reworded after agreement keeps its ID and is agreed again; the brackets say what changed it, e.g. "Agreed 2026-10-28 (revised for UART draft)".

On 2026-10-27 the firmware team delivered the UART interface draft ([Research/STM32_Pi_UART_Interface_Draft_V1.docx](../../Research/STM32_Pi_UART_Interface_Draft_V1.docx), protocol 0x02, called **UART draft** below); decisions it changed say "revised for UART draft". On 2026-10-28 it was replaced by [V2](../../Research/STM32_Pi_UART_Interface_Draft_V2.docx): protocol 0x03, MCU-calculated SoC added to TELEMETRY/FAULT_UPDATE (38 bytes). "UART draft" now means V2. The revised decisions were agreed on 2026-10-28, when D1, D2, D5 and D6 went to Complete.

| ID | Decision | Refs | Status |
|---|---|---|---|
| DD-01 | Diagrams are Mermaid sources in `docs/design/` (source of truth), mirrored to Miro for review. | S1 | Agreed 2026-10-27 |
| DD-02 | Two processes: a headless core service (systemd: UART, fan control, database, web dashboard server) and a separate display app. A UI crash never stops fan control or logging. Display ↔ core over a Unix socket. | S6, S7.4, S8, S9 | Agreed 2026-10-27 |
| DD-03 | Touchscreen UI is native Rust, no browser/kiosk. The S9 web dashboard is still served by the core service for laptops on Ethernet. | S1.2, S8, S9 | Agreed 2026-10-27 |
| DD-04 | Storage tiers per S10: 1 s data RAM-only (1 h ring buffer), 10-min min/avg/max records on disk, daily records on disk. No raw 1 s data on disk except fault snapshots. | S10.2–S10.4, S10.8 | Agreed 2026-10-27 |
| DD-05 | Wide tables: one column per signal (fixed 4S3P signal set). | S10.3, S10.11 | Agreed 2026-10-27 |
| DD-06 | Timestamps stored as UTC ISO 8601 text, fixed format `YYYY-MM-DDTHH:MM:SS.sssZ` (sorts lexically); shown in local time. Replaces the earlier integer-ms idea. | S10.10 | Agreed 2026-10-27 |
| DD-07 | Fault snapshot written in two stages: fault record + 60 s before immediately (≤ 1 s), the 60 s after appended every ~10 s. A fault inside an open window extends that snapshot. | S10.5, S10.6 | Agreed 2026-10-27 |
| DD-08 | The Pi integrates Ah and Wh (charged and discharged) itself from the 1 s `current_a` and `pack_v` of each valid TELEMETRY sample; no MCU counters. Charge flowing while the Pi or the link is down is not counted (gap visible through `sample_count`, DD-10). Replaces the earlier request for MCU counters. | S10.3, S10.4 | Agreed 2026-10-28 (revised: Pi integrates) |
| DD-09 | Daily record is written at 00:00 local time for the finished day. "Compaction" = deleting 10-min records older than 30 days. DST days (23/25 h) handled explicitly. | S10.4, S10.7 | Agreed 2026-10-27 |
| DD-10 | Each aggregate stores a `sample_count`; daily averages are weighted by it; graphs show gaps. | S10.3, S10.4, S9 | Agreed 2026-10-27 |
| DD-11 | No records are timestamped until the clock is valid (NTP or RTC); a "time invalid" event is logged. | S10.10, S10.16 | Agreed 2026-10-27 |
| DD-12 | SQLite WAL with `synchronous=FULL` (write load is tiny) to meet "power loss loses at most the last record". | S10.1 | Agreed 2026-10-27 |
| DD-13 | The 1 h RAM buffer is not persisted; live graphs start empty after a reboot. | S10.2 | Agreed 2026-10-27 |
| DD-14 | One HTTP/JSON API (axum) for both the touchscreen app and the laptop dashboard: served on a Unix socket for the display, on TCP for laptops. The display app has no database access. | S8, S9 | Agreed 2026-10-27 |
| DD-15 | A single `db_writer` thread owns the only SQLite write connection; all other tasks send it messages (mpsc). Readers use separate read-only connections (WAL). | S10.1 | Agreed 2026-10-27 |
| DD-16 | Fan behaviour on BMS comm loss (OFFLINE) comes from the MCU CONFIG: `fan_offline_action` 1 = hold last duty, then off after `fan_offline_ms`; 2 = 100 % while OFFLINE; 0 = undecided (Pi then runs 100 %, see DD-28). The policy is owned by the MCU/system team, not the Pi. | S6, S8.12, UART draft §6–7 | Agreed 2026-10-28 (revised for UART draft) |
| DD-17 | Crash policy: `panic = "abort"`, systemd `Restart=always` + `WatchdogSec`; any task failure restarts the whole core. No task-level restarts. While the core is down the heartbeat stops, so after > 5 s the MCU raises E11 and switches charging off (UART draft §8). Fan behaviour while the core is down: proposed in DD-59 (D10). | S3.8, S6, S7.4 | Agreed 2026-10-27 (fan fallback proposed in DD-59) |
| DD-18 | Live data is pushed to display and dashboard with Server-Sent Events; actions use normal HTTP requests. | S8, S9.4 | Agreed 2026-10-27 |
| DD-19 | Touchscreen UI toolkit: Slint. | S1.2, S8 | Agreed 2026-10-27 |
| DD-20 | One `event` table for faults, warnings and system events (`kind` = FAULT / WARNING / SYSTEM); the fault log and event log pages are filters on it. | S9.9, S9.10, S10.5 | Agreed 2026-10-27 |
| DD-21 | A fault snapshot (±60 s of 1 s data) is taken for every `FAULT` event, i.e. every Protection, Latched protection and Critical code (DD-33); Warnings get no snapshot. | S10.6 | Agreed 2026-10-27 |
| DD-22 | Every MCU fault event stores all signal values of the frame that raised it (TELEMETRY or FAULT_UPDATE carry the complete measurement set), plus its validity. These are the values *at report*, not the exact trip values (UART draft §9). | S9.9, S10.5 | Agreed 2026-10-28 (revised for UART draft) |
| DD-23 | Retention: 10-min records 30 days; daily records, events and SoH kept forever; snapshots deleted oldest-first only when the DB passes `DB_SIZE_SOFT_LIMIT` (proposal 150 MB), logged as an event. No automatic `VACUUM`. | S10.5, S10.7, S10.8 | Agreed 2026-10-27 |
| DD-24 | Battery configuration, protection limits, fan tables, fan fault and offline parameters and timeouts come **only** from the MCU CONFIG message; the Pi keeps no copy of its own. Runtime settings (PIN hash, wrong attempts, lock) live in the DB `setting` table. The TOML file in `/etc/bess/` holds Pi-only settings (serial device, ports, DB limits, PIN rules, backup path, cloud). | S2.1–S2.4, S6, S9.12 | Agreed 2026-10-28 (revised for UART draft) |
| DD-25 | Cloud upload keeps a watermark per table (last acknowledged key) instead of a queue table; events whose `cleared_ts` changed are re-sent by `id`. | S10.15 | Agreed 2026-10-27 |
| DD-26 | Core has a `uart_tx` task: HEARTBEAT every 1 s, CONFIG_REQUEST every 1 s in WAIT_CONFIG, CONFIG_ACK after validation, FAULT_RESET on user action. The heartbeat carries the accepted `boot_counter` + `config_revision` (both 0 before acceptance) and is only sent while `link_supervisor` and `fan_controller` are alive, so the MCU's E11 also covers a hung Pi app. Until it gets an identity-matched heartbeat the MCU keeps charging inhibited. | S2.4, S3.6, S3.8, UART draft §8 | Agreed 2026-10-28 (revised for UART draft) |
| DD-27 | BMS link states OFFLINE / WAIT_CONFIG / ONLINE, 3 s (`pi_data_timeout_ms`) on the monotonic clock from the last valid frame. Telemetry carries `boot_counter` and `config_revision`; a change of either sends the Pi back to WAIT_CONFIG. ONLINE needs a CONFIG that passes validation, a sent CONFIG_ACK (accepted) and telemetry with the same identity. After a short loss with unchanged identity the Pi resumes ONLINE without re-reading. | S2.4, S5, S8.12, UART draft §4, §8 | Agreed 2026-10-28 (revised for UART draft) |
| DD-28 | Before the first valid CONFIG (no fan table yet) and while `fan_offline_action` = 0 (undecided), the fans run at 100 %. Without a CONFIG there is no `fan_offline_ms`, so they stay at 100 % until a CONFIG arrives. | S2.4, S6 | Agreed 2026-10-27 (revised for UART draft) |
| DD-29 | While OFFLINE, a last-known ESTOP or FAULT state stays shown (with the comm-lost banner); otherwise OFFLINE is shown. | S8.9, S8.10, S8.12 | Agreed 2026-10-27 |
| DD-30 | `core_state.last_alive_ts` written every 60 s; each start logs `LOG_GAP` marked `clean` / `crash` / `power loss`. | S10.17 | Agreed 2026-10-27 |
| DD-31 | Clock validity read from `chrony`; events from before the clock was valid wait in a RAM queue (max 100); clock steps > 2 s logged as `TIME_ADJUSTED`. | S10.10, S10.16 | Agreed 2026-10-27 |
| DD-32 | Display shows "Monitor service restarting…" when the core is gone, separate from "BMS communication lost". | S7.4, S8.12 | Agreed 2026-10-27 |
| DD-33 | Event `kind` follows Table A severity: Warning → `WARNING`; Protection, Latched protection, Critical → `FAULT` (all get a snapshot, DD-21). Banner/list order Critical > Protection > Warning, then lowest code (S3.22). | S4.1, S10.6 | Agreed 2026-10-27 |
| DD-34 | Every TELEMETRY and FAULT_UPDATE frame carries the complete 32-bit fault bitmap (bits 0–18 = E01–E19). `event_detector` processes every frame in arrival order and raises/clears from the difference with the previous bitmap, so short faults reported only in FAULT_UPDATE frames are logged too. Pi-local E12 is merged in. | S5.8, S10.5, UART draft §4–5 | Agreed 2026-10-28 (revised for UART draft) |
| DD-35 | The fault log's "measured value" is picked from the stored frame values (DD-22) by a `trigger_signal` column per code in the Table A source file, and labelled as the value at report. | S4.1, S9.9 | Agreed 2026-10-28 (revised for UART draft) |
| DD-36 | `event` has `fw_version` (from CONFIG `fw_major.minor.patch`), `app_version`, `boot_counter` and `config_revision` columns. | S1.8, S1.12, UART draft §9 | Agreed 2026-10-28 (revised for UART draft) |
| DD-37 | On core start, reconnect and MCU restart, open events in the DB are reconciled with the first frame's fault set; nothing is raised or cleared while OFFLINE. | S9.9, S10.5 | Agreed 2026-10-27 |
| DD-38 | Fault reset per UART draft §8: FAULT_RESET with a nonzero `request_id` plus target `boot_counter`/`config_revision`; RESET_ACK result 0 accepted, 1 fault still active, 2 identity mismatch, 3 not ready. 2 s timeout, no automatic retry. The last `request_id` is kept in `core_state` so a restarted core never reuses one. Request and result are both logged. Reset source in telemetry (BMS button) is not in the draft: still optional. | S3.6, S9.12 | Agreed 2026-10-28 (revised for UART draft) |
| DD-39 | E12 FAN_FAULT is detected, shown and logged only by the Pi; not sent to the MCU (bit stays 0 in MCU frames). Detection: fan duty > 0 and RPM < `fan_speed_min_pct` of the table's expected RPM for `fan_fault_ms`; disabled (shown "calibration unavailable") while the CONFIG RPM calibration flag is clear. | S6, S3.7, UART draft §5, §7 | Agreed 2026-10-28 (revised for UART draft) |
| DD-40 | The `protocol` crate implements the UART draft: 115200 8N1, header `AA 55`, version 0x03 (V2; TELEMETRY/FAULT_UPDATE 38 bytes), little-endian, CRC-16/CCITT-FALSE over version…payload, exact length per message id, resync after bad input, bounded buffers; bad frames counted (S5.9). The reference frames in the draft are unit tests; C-generated frames are still required (S5.11). | S5.1–S5.11 | Agreed 2026-10-28 (revised for UART V2) |
| DD-41 | Telemetry `validity` bits (bit 10 = SoC since V2): an invalid or absent field is stored as NULL, shown as "N/A" and left out of min/avg/max (DD-10 `sample_count` counts frames). NTC4 presence from CONFIG `ntc_present_mask`. | S8, S10.3, UART draft §4 | Agreed 2026-10-28 (revised for UART V2) |
| DD-42 | E-STOP state: `fan_controller` commands the fans off; this overrides every other rule (E7.7). E05/E17 still force 100 % in all other states. | E7.7, S6 | Agreed 2026-10-27 |
| DD-43 | Every CONFIG the Pi validates is stored in a `mcu_config` table (identity, firmware version, raw 190 bytes, accepted/rejected status), for System info (S9.11) and to interpret old data. | S2.4, S9.11 | Agreed 2026-10-27 |
| DD-44 | SoC comes only from the MCU (`soc_cPct`, Coulomb counting on the MCU, UART draft V2 §4a). The Pi never calculates a second SoC; while validity bit 10 is clear SoC is shown as "N/A" and stored as NULL. | S8, S9.4, S10.3, UART draft V2 §4a | Agreed 2026-10-28 |
| DD-45 | 10-min intervals aligned to UTC `:00`/`:10`/…; the first sample at or after the boundary closes the interval (2 s timer fallback); intervals without samples are not written. See [data-lifecycle.md](data-lifecycle.md). | S10.3 | Proposed 2026-10-28 |
| DD-46 | Ah/Wh: each sample with a valid current counts for one `telemetry_ms`; missing samples are not counted; independent of the wall clock. Refines DD-08. | S10.3, S10.4 | Proposed 2026-10-28 |
| DD-47 | Daily job order: close 23:50 interval → daily record → compaction (only days with a daily row) → size guard → backup. Missing daily rows are caught up at start-up. | S10.4, S10.7, S10.14 | Proposed 2026-10-28 |
| DD-48 | Daily SQLite online backup to a USB stick if mounted, else the SD card; keep newest 7; failures logged as `BACKUP_FAILED`. DB download uses the same mechanism. | S10.12, S10.14 | Proposed 2026-10-28 |
| DD-49 | Graphs and CSV read the tier that covers the range: ≤ 1 h ring buffer, ≤ 30 days `agg_10min`, older `agg_daily`; coarser on request. CSV has `timestamp_utc` + `time_local` columns. | S9, S10.11, S10.13 | Proposed 2026-10-28 |
| DD-50 | Cloud upload also sends complete snapshots; order events → snapshots → aggregates; read-only, never blocks logging. Extends DD-25. | S10.15, S11.1, S11.7 | Proposed 2026-10-28 |
| DD-51 | `fan_controller` runs on every valid TELEMETRY, on FAULT_UPDATE with an E05/E17/E-STOP change, on link changes and on a 1 s timer; writes the PWM each run, then feeds the heartbeat liveness flag. Rule order: E-STOP → no CONFIG → OFFLINE policy → E05/E17 → curves; the winning rule is shown as the control reason. See [fan-control.md](fan-control.md). | S6, E7.7 | Proposed 2026-10-28 |
| DD-52 | Battery curve from NTC1, air curve from NTC3; an invalid input makes that curve demand 100 %. | S6.3 | Agreed 2026-10-28 |
| DD-53 | Each curve has its own hysteresis state; while ON the demand is at least `fan_min_pct`; outside the table the end point's duty is used; a curve starts ON when the temperature is above its off point. | S6.2, S6.4 | Proposed 2026-10-28 |
| DD-54 | E12 expected RPM is interpolated at the commanded duty from the driving curve's table (forced 100 % uses the 100 % point). Raise after `fan_fault_ms` below `fan_speed_min_pct`; clear after `fan_fault_ms` at or above it, or when the fans go below `fan_min_pct`. | S6.6 | Proposed 2026-10-28 |
| DD-55 | When the MCU identity changes, the old CONFIG is dropped and the fans run at 100 % until the new CONFIG is accepted. Extends DD-28. | S2.4, S6 | Agreed 2026-10-28 |
| DD-56 | Fan controller: Adafruit EMC2101 breakout on Pi 4 I2C-1 (0x4C), host-controlled (LUT not used); both fans' PWM inputs on the one FAN output; breakout FAN pull-up jumper cut so the fans' own pull-ups give 100 % when the EMC2101 is unpowered or the line is broken; NC thermostat (opens at 50 °C, OI19) in series with the PWM line. See [fan-hardware.md](fan-hardware.md). | S6.1, S6.7, E7.7, M5 | Proposed 2026-10-29 |
| DD-57 | PWM ≈ 5.8 kHz, 62 steps, duty rounded to the nearest step (± 0.81 %); spin-up bypassed; registers read back after each write, re-initialised on mismatch (`FAN_CTRL_RESET`). Fallback 25.7 kHz / 14 steps needs an S6.2 change. | S6.2 | Agreed 2026-10-29 |
| DD-58 | Each fan's tach on its own Pi GPIO (2 pulses/rev, 1 s window); E12 when either fan is low, event detail names the fan; both speeds stored and shown (`fan1_rpm`, `fan2_rpm`; D2 signal set 14 → 15). | S6.6, S8, M5, S10.3 | Agreed 2026-10-29 |
| DD-59 | Core-down fallback: `bess-fan-failsafe` writes 100 % from `ExecStopPost=` and an early boot unit; core writes 100 % on SIGTERM; BCM2711 hardware watchdog; unpowered EMC2101 → 100 %; thermostat and MCU E11 cover a hung Pi. Completes DD-17. | S6.7, DD-17 | Proposed 2026-10-29 |
| DD-60 | Pi proposal to firmware: production `fan_offline_action` = 2 (100 % while OFFLINE). | S6, DD-16 | Proposed 2026-10-29 |
| DD-61 | Fan selection: 12 V 4-wire PWM fan with internal PWM pull-up, open-collector tach 2 pulses/rev, starts at 30 % from standstill, works at 5.8 kHz. | S6, M5 | Proposed 2026-10-29 |
| DD-62 | CSV export targets English-locale Excel: `,` separator, `.` decimal, UTF-8 without BOM, no `sep=` line (Dutch Excel then reads 13.284 as 13284). A Dutch-locale variant (`;` and decimal `,`) is parked. See [csv-excel-test.md](csv-excel-test.md). | S10.11, S10.13 | Agreed 2026-10-29 |
| DD-63 | SoH = Ah charged during the 8 A CC charge from `V_low` up to the BMS charge-stop voltage `V_stop` (the 85 % stop is on voltage, confirmed 2026-10-29), divided by the same window on the new battery (baseline per temperature band) × 100 %; measured capacity = SoH × `CAP_AH`. Replaces the rest-point method of S10.9 (requirements change proposed). See [soh.md](soh.md). | S10.9, S8.13 | Proposed 2026-10-29 |
| DD-64 | An SoH window counts only with CHARGING and 7.6–8.4 A throughout, armed below `V_low` − 50 mV, ended by `V_stop` and the BMS stop without a fault, no telemetry gap > 2 s, all fields valid, NTC1 15–35 °C at start and < 5 °C change, duration 0.5–1.5× baseline; compared within its temperature band (15–25 / 25–35 °C); at most 1 result per day. | S10.9 | Proposed 2026-10-29 |
| DD-65 | Shown SoH = median of the last 5 results; "N/A – being measured" and 100 % in calculations until a baseline exists; PIN-protected baseline reset on battery replacement; `soh` table columns changed for the window method (soh.md §5). | S10.9, S8.13, S9.11 | Proposed 2026-10-29 |
| DD-66 | `uart_rx` compares the `sequence` of consecutive valid MCU frames; a jump means frames were lost: counted with the S5.9 statistics (`frames_lost_total`) and logged as SYSTEM event `FRAMES_LOST` (count, at most 1 per minute), because a lost FAULT_UPDATE can hide a short fault. The comparison restarts after OFFLINE and on a new MCU identity. Corrects D6 §2. See [firmware-requests.md](firmware-requests.md) §5. | S5.9, S10.5, UART draft §3 | Proposed 2026-10-29 |
| DD-67 | The Pi's CONFIG validation is exactly the shared UART spec list (§7), with no Pi-only rules, so the Pi never rejects a CONFIG the MCU accepts (that would keep charging inhibited). The Pi's reading of "threshold ordering" and "table endpoints consistent" is proposed to firmware (firmware-requests.md §6). | S2.3, S2.4, UART draft §7 | Proposed 2026-10-29 |
| DD-68 | TELEMETRY / FAULT_UPDATE: reserved validity bits 11–15 and byte 27 are ignored (not a frame error); a set fault bit 19–31 is shown and logged as an unknown fault (kind FAULT, `code` = bit + 1, name `UNKNOWN`); a `state` outside 0–4 is shown as unknown and logged. | S4.1, S8.9, UART draft §4–5 | Proposed 2026-10-29 |
| DD-69 | In WAIT_CONFIG the Pi uses the fault bitmap of every valid TELEMETRY / FAULT_UPDATE frame as when ONLINE: active fault list, red banner, shown state E-STOP / FAULT with "Waiting for BMS configuration", and `event_detector` raises and clears events (reconciled at the first valid frame after core start, OFFLINE or an MCU restart, DD-37). Measurements stay hidden and are not sampled (S2.4), so FAULT events raised there get no snapshot; `fw_version` is NULL until a CONFIG for that identity is received. Without this, E15 CONFIG_INVALID (the MCU then sends no valid CONFIG) is never shown or logged. See [fault-handling.md](fault-handling.md) §1, §3 and [lifecycle.md](lifecycle.md) §2, §6. | S2.4, S8.9, S8.10, S9.9, S10.5, UART draft §8 | Proposed 2026-10-29 |
| DD-70 | `bess-core` is one process: tokio async tasks plus the `db_writer` thread (DD-15). Tasks share state only through channels: `watch` for latest-value state (config, live state), `broadcast` for every sample and every fault frame, `mpsc` for work queues to a single owner. `uart_tx` is the only writer to the serial port. See [architecture.md](architecture.md) §2. | S1.2 | Proposed 2026-10-29 |
| DD-71 | Cargo workspace in `Pi/` with `protocol` (lib, DD-40), `bess-api` (lib, API types), `bess-core` (bin) and `bess-display` (bin, talks only to `bess-api` over the Unix socket). S1.3 asks for `/monitor` in one repository next to `/firmware` and `/protocol`; this repository has `Pi/` and `docs/`, so either the layout or S1.3 changes (to settle with firmware and the requirements owner). See [architecture.md](architecture.md) §3. | S1.2, S1.3, S5.10 | Proposed 2026-10-29 |
| DD-72 | Stored signals are `REAL` in SI units with the unit in the column name (`pack_v`, `current_a`, `t_cell_c`, …), so the CSV header needs no mapping. The current sign is inverted once, at decoding: `current_a` is positive = charging in the DB, the CSV, the cloud and the graphs, as on the screen (S8.8 asks the inversion for display only). The wire value (negative = charging, E5.8) is never stored. See [database.md](database.md) §1. | S8.8, E5.8, S10.11, S10.13 | Proposed 2026-10-29 |
| DD-73 | All tables are `STRICT`; the schema version is kept in `PRAGMA user_version`; migrations are built into `bess-core` and run at start-up before any task starts. See [database.md](database.md) §2. | S10.1 | Proposed 2026-10-29 |
| DD-74 | Samples are made only from TELEMETRY frames while the link is ONLINE; FAULT_UPDATE frames go to `event_detector` only. Samples and events carry the Pi's receive time (the MCU sends no time): UTC for storage, monotonic time for the ring buffer and all timeouts. Without a valid clock (DD-11) samples still go to the ring buffer, so live graphs work, but nothing is aggregated or written. See [data-lifecycle.md](data-lifecycle.md) §1–2. | S2.4, S10.2, S10.5, S10.10 | Proposed 2026-10-29 |
| DD-75 | Cloud upload is off unless enabled in the TOML file, which needs TE IT approval first (P1.7). Credentials live only in the TOML file or a key file, never in the source code. See [data-lifecycle.md](data-lifecycle.md) §7. | P1.7, S10.15 | Proposed 2026-10-29 |
| DD-76 | E12 does not change the fan duty: the duty follows the rules of DD-51 as before, and E12 is only shown and logged (WARNING). See [fan-control.md](fan-control.md) §5. | S6.6 | Proposed 2026-10-29 |
| DD-77 | `bess-display` is started by systemd as a dedicated user and runs the Slint app directly on the KMS/DRM backend: no desktop, no login (S7.4). It does not wait for the core: it shows "Starting…", connects to the Unix socket every 1 s, and treats a closed SSE stream or 3 s without a message as core lost (DD-32); the core sends a keep-alive at least every 1 s. See [lifecycle.md](lifecycle.md) §1, §5. | S7.4, S8.12, DD-03, DD-18 | Proposed 2026-10-29 |
| DD-78 | The PIN-protected "Pi shutdown" (S9.12) stops `bess-core` cleanly (API actions refused, open 10-min interval written as a partial record, open snapshot finished, `SHUTDOWN` logged, DB closed) and then powers off the Pi only; the BMS keeps running. The fans go to 100 % and stay there until the Pi starts again (DD-59); the confirmation dialog says so. No reboot action (not in the requirements). See [lifecycle.md](lifecycle.md) §1. | S9.12, S10.17 | Proposed 2026-10-29 |
| DD-79 | The clock is valid when chrony reports an NTP sync, or when the RTC is readable and its year is at least the build year. An NTP sync also sets the RTC; after more than 24 h without NTP the Pi keeps running on the RTC. See [lifecycle.md](lifecycle.md) §4. | S10.10, S10.16 | Proposed 2026-10-29 |
| DD-80 | Every link-state change (D5) and every MCU state change (D6) is logged as a SYSTEM `STATE_CHANGE` event; link changes repeating within 10 s are logged once with a count, so a loose connector cannot flood the event log. See [lifecycle.md](lifecycle.md) §2. | S9.10, S10.5 | Proposed 2026-10-29 |
| DD-81 | Value colours come from the value itself, not from events: red at the protection limit from CONFIG (Table E), yellow at the S8.11 warning level, so a value can be red before the MCU trips. Open: CONFIG V2 has no warning levels (only the S8.11 SoC levels match `soc_min_cPct` / `soc_max_cPct`); either the Pi keeps the S8.11 yellow levels as display settings in the TOML file (extends DD-24) or firmware adds them to CONFIG. See [fault-handling.md](fault-handling.md) §4. | S8.11, S2.4 | Proposed 2026-10-29 |
| DD-82 | An event row stores the code, `kind` and `severity` (for the fault log filters); names and descriptions are not stored but looked up in the generated Table A module when shown or exported, so every screen and the cloud use the same text. See [fault-handling.md](fault-handling.md) §4. | S4.1, S9.9 | Proposed 2026-10-29 |
| DD-83 | Every raise of a fault is a new event row; the Pi does not merge or count repeated faults (the log is never compacted, S10.5). Counting and the email repeat window are cloud features (S11.17, S11.22). See [fault-handling.md](fault-handling.md) §2. | S10.5, S11.17, S11.22 | Proposed 2026-10-29 |

## Trade-offs for key choices (P1.6)

P1.6 asks for the key design choices in a trade-off table with the options and the reasons for the choice. This section covers the Pi software and the fans; the electrical examples in P1.6 (conductor material, wire, fuses) belong to the electrical design. The chosen or proposed option is in bold. **Source** says where the reasons come from; "reconstructed 2026-10-29" means they were not written down when the choice was made, so the Pi team confirms or corrects them.

### T1 Process split (DD-02)

Source: reconstructed 2026-10-29.

| Option | For | Against |
|---|---|---|
| One process (UI and core) | Simplest; no API between UI and core | A UI fault (toolkit, graphics driver, panic) also stops fan control, logging and the heartbeat; with `panic = "abort"` (DD-17) every UI panic restarts everything |
| **Core service + display app** | A UI crash never stops fan control or logging; the core runs without a screen; the display restarts on its own; the laptop dashboard uses the same API (DD-14) | Two services and an API to keep in step |
| One process per function (UART, fans, DB, web) | Most isolation | Many interfaces for a small app; fans and logging need the UART data anyway, so they fail together |

Why: fan control and logging must keep running whatever the UI does; two processes is the smallest split that gives that.

### T2 Touchscreen UI (DD-03, DD-19)

Source: reconstructed 2026-10-29. Slint licence checked 2026-10-29 in its `LICENSE.md` (release 1.13 and later).

| Option | For | Against |
|---|---|---|
| Browser in kiosk mode showing the S9 web dashboard | One UI for the touchscreen and the laptop | A desktop or compositor plus a full browser on the Pi 4 (memory, start-up time within S7.4's 60 s); touch has no hover; browser updates |
| egui / eframe (first pick, before DD-19) | Pure Rust, simple code; graphs with `egui_plot`; MIT / Apache licence | Needs X11 or Wayland (winit), so a compositor on the Pi; immediate mode rebuilds the UI every frame |
| **Slint** | UI files made for embedded touch screens; runs directly on KMS/DRM without a desktop (DD-77); low CPU and memory use | No chart widget, so graphs are drawn by hand; licence: the royalty-free licence excludes embedded systems, which leaves GPLv3 (the Pi app published under a GPL-compatible licence) or a paid licence |

Why: native, no browser (DD-03), and it starts straight on the screen without a desktop (S7.4).
Revisit if: TE accepts neither GPLv3 for the Pi app nor a paid Slint licence. To check before implementation: whether the cabinet Pi counts as an embedded system under Slint's royalty-free licence.

### T3 Display ↔ core interface (DD-14, DD-18)

Source: reconstructed 2026-10-29.

| Option | For | Against |
|---|---|---|
| Display reads the SQLite file | No API code | Ties the display to the DB schema; live values are not in the DB (DD-04), so they still need another channel |
| Own binary protocol on the Unix socket | Small and fast | A second interface next to the laptop dashboard's HTTP API |
| **One HTTP/JSON API, live data as Server-Sent Events** | Same API for the touchscreen (Unix socket) and laptops (TCP); testable with a browser or `curl`; SSE is plain HTTP push, enough for 1 s values; actions are normal requests | JSON overhead, small at 1 Hz |
| WebSocket for live data | Two-way | Two-way is not needed (actions are requests); more to handle than SSE |

Why: one API for both screens; SSE is the simplest push that fits 1 s updates.

### T4 Storage tiers (DD-04, DD-13)

Source: S10.2–S10.8 set the tiers; sizes from [database.md](database.md) §4.

| Option | For | Against |
|---|---|---|
| Every 1 s sample on disk | Full detail for ever | 86 400 rows a day, ≈ 13 MB a day at ≈ 150 B per row, ≈ 19 GB after 4 years against the 200 MB of S10.8; constant writes wear the SD card |
| **1 s in RAM for 1 h; 10-min and daily records on disk; 1 s snapshots around faults** | ≈ 25 MB after 4 years; 1 s detail kept where it matters (±60 s around each fault); few writes | 1 s detail older than 1 h is gone except around faults, and the RAM hour is lost on a restart (DD-13) |
| Time-series database (e.g. InfluxDB) | Downsampling and retention built in | S10.1 asks for SQLite; another service on the Pi |

Why: meets S10.8 and spares the SD card; S10.1 prescribes SQLite.

### T5 Table layout and timestamps (DD-05, DD-06)

Source: DD-06 replaced the integer-ms idea (2026-10-27); the reasons are reconstructed 2026-10-29.

| Option | For | Against |
|---|---|---|
| **Wide tables: one column per signal** | One row per interval; the CSV is the table as it is; simple queries | A new signal needs a migration (happened once: `fan2_rpm`, DD-58) |
| Long table: one row per signal and time | New signals without a schema change | 15 times the rows; the CSV export needs a pivot |
| Timestamps as integer ms | 8 bytes, fast to compare | Unreadable in DB tools and the CSV without conversion |
| **Timestamps as UTC ISO 8601 text, fixed format** | Readable everywhere; the fixed format sorts as text; SQLite date functions read it | 24 bytes instead of 8; irrelevant at this database size |

Why: people open the database and the CSV; size is not a constraint.

### T6 SQLite durability (DD-12)

Source: DD-12 ("write load is tiny"); options reconstructed 2026-10-29. S10.1 already asks for WAL mode.

| Option | For | Against |
|---|---|---|
| **`synchronous=FULL`** | A committed record survives a power cut | One fsync per commit; at a few commits a minute that costs nothing |
| `synchronous=NORMAL` | Fewer fsyncs; the usual setting with WAL | A power cut can roll back the last commits (the file stays consistent), which can be more than the "last record" S10.1 allows |
| `synchronous=OFF` | Fastest | A power cut can corrupt the database |

Why: S10.1 allows losing at most the last record, and the write load is tiny.

### T7 Ah and Wh counting (DD-08, DD-46)

Source: DD-08, revised 2026-10-28 (replaced the request for MCU counters).

| Option | For | Against |
|---|---|---|
| Energy counters on the MCU, sent in telemetry | Also counts while the Pi or the link is down | Firmware and protocol change; the Pi still turns counters into values per interval |
| **The Pi integrates the 1 s telemetry** | No firmware change; the same samples as the graphs, so the numbers agree; values per interval directly | Charge while the Pi or the link is down is not counted; the gap shows in `sample_count` (DD-10) |

Why: no dependency on firmware, and the gap is visible.
Revisit if: energy totals must be complete across Pi downtime.

### T8 Fan controller and fallback (DD-56, DD-59)

Source: [fan-hardware.md](fan-hardware.md) §1, §4 and §7; requirements comments SR22 and SR23 on S3.8. The EMC2301 row was added 2026-10-29 and is not weighed yet.

Fan controller:

| Option | For | Against |
|---|---|---|
| **Adafruit EMC2101 breakout (proposed)** | I2C, ready-made breakout; one PWM output fits the one shared duty; host-controlled | No watchdog, so it holds the last duty when the Pi stops writing; powers up at 0 %; one tach input, so both tachs go to Pi GPIOs; at 25 kHz only 14 steps (DD-57 uses 5.8 kHz) |
| Microchip EMC2301 (same family) | Its family datasheet lists a watchdog timer, which could cover a hung Pi or I2C bus inside the chip | Not weighed yet: what happens on a watchdog timeout, the PWM and tach details and a board for it are still to be checked in the datasheet |
| EMC2101 TCRIT with a diode near the battery (extra layer on the EMC2101) | The chip itself forces 100 % above TCRIT while the core is down | Works only after the Pi has configured it once after power-up; does not replace the thermostat; not planned unless the thermostat cannot be sourced |

When the Pi does not control the fans:

| Option | For | Against |
|---|---|---|
| MCU drives the fans at 100 % when the Pi is lost (SR23 alternative) | Cooling never depends on the Pi | A second fan drive path from the MCU; fan control is on the Pi (S6.1) |
| **MCU stops charging on Pi loss (S3.8, E11; kept by team decision, SR22), plus a Pi-independent fallback: the fans' own pull-ups, an NC thermostat in the PWM line and `bess-fan-failsafe` (proposed)** | Charging, the main heat source, stops; 100 % needs no power, Pi or I2C: a dead controller, a cut wire or an open thermostat all give 100 %; systemd covers crashes | A hung Pi or I2C bus holding a low duty is covered only by E11 and the thermostat above 50 °C |

Why: the fallback works with every part of the Pi dead, and E11 stops the main heat source.

### T9 Fan type (DD-61)

Source: reconstructed 2026-10-29 from [fan-hardware.md](fan-hardware.md).

| Option | For | Against |
|---|---|---|
| **12 V 4-wire PWM fan (proposed)** | Speed set by a signal line; the fan's own pull-up gives 100 % when the line or the controller fails (DD-56); tach for E12 | Many do not stop at 0 % (open in D10); the fan specification asks for about 25 kHz, DD-57 uses 5.8 kHz (bench test) |
| 3-wire fan, speed set by the supply voltage | Stops at 0 V | Needs a voltage driver in the 12 V path; a dead driver means fans off, not 100 % |
| 2-wire on/off fan | Simplest | No speed table (S6.2) and no tach, so no E12 (S6.6) |

Why: only the 4-wire fan gives speed control, a tach and 100 % as the failure default.

### T10 CSV format (DD-62)

Source: [csv-excel-test.md](csv-excel-test.md), tested 2026-10-29.

| Option | English Excel | Dutch Excel | Outcome |
|---|---|---|---|
| **A: `,` separator, `.` decimal** | ✓ | ✗ one column | Chosen: passes S10.13 in English Excel |
| B: `sep=,` line, then A | ✓ | ✗ 13.284 becomes 13284 | Never: silently corrupts values |
| C: `;` separator, `,` decimal | ✗ | ✓ | Parked as an optional Dutch variant |
| D: `;` separator, `.` decimal | ✗ | ✗ | Rejected |

Why: no single file opens correctly in both locales by double-click; English Excel is the target, and Dutch users import with Data → From Text/CSV.

### T11 SoH method (DD-63)

Source: [soh.md](soh.md) §1–2.

| Option | For | Against |
|---|---|---|
| Ah counted / ΔSoC of the MCU SoC | Simple; any charge or discharge | Circular: the SoC is itself Ah counted / `CAP_AH`, so SoH is always 100 % |
| Rest points on the open-circuit voltage (current S10.9) | The textbook method | LFP rest voltage is flat between about 20 % and 90 %; the BMS keeps the battery in 20–85 % (E1.7), so readable rest points never happen |
| **Ah charged at 8 A from `V_low` up to the BMS stop voltage `V_stop` (proposed)** | The top point is a real measurement (the BMS stop); the current sensor's gain error cancels against the baseline; works inside the 20–85 % window | Needs charges that start below `V_low`; resistance and temperature errors, limited by temperature bands (DD-64); needs a baseline at commissioning |

Why: the only option that is not circular and works inside the BMS window.
