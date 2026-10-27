# Design decisions (D8)

Decisions for the Pi monitor app, agreed during the design phase. Mirrored on the Miro board (Kanban Software, card D8).

| ID | Decision | Refs | Status |
|---|---|---|---|
| DD-01 | Diagrams are Mermaid sources in `docs/design/` (source of truth), mirrored to Miro for review. | S1 | Agreed 2026-10-27 |
| DD-02 | Two processes: a headless core service (systemd: UART, fan control, database, web dashboard server) and a separate display app. A UI crash never stops fan control or logging. Display ↔ core over a Unix socket. | S6, S7.4, S8, S9 | Agreed 2026-10-27 |
| DD-03 | Touchscreen UI is native Rust, no browser/kiosk. The S9 web dashboard is still served by the core service for laptops on Ethernet. | S1.2, S8, S9 | Agreed 2026-10-27 |
| DD-04 | Storage tiers per S10: 1 s data RAM-only (1 h ring buffer), 10-min min/avg/max records on disk, daily records on disk. No raw 1 s data on disk except fault snapshots. | S10.2–S10.4, S10.8 | Agreed 2026-10-27 |
| DD-05 | Wide tables: one column per signal (fixed 4S3P signal set). | S10.3, S10.11 | Agreed 2026-10-27 |
| DD-06 | Timestamps stored as UTC ISO 8601 text, fixed format `YYYY-MM-DDTHH:MM:SS.sssZ` (sorts lexically); shown in local time. Replaces the earlier integer-ms idea. | S10.10 | Agreed 2026-10-27 |
| DD-07 | Fault snapshot written in two stages: fault record + 60 s before immediately (≤ 1 s), the 60 s after appended every ~10 s. A fault inside an open window extends that snapshot. | S10.5, S10.6 | Agreed 2026-10-27 |
| DD-08 | MCU sends cumulative Ah/Wh counters (charged and discharged) in telemetry; the Pi only takes differences, so Pi reboots lose no energy counting or SoH measurement. | S5, S10.3, S10.9 | **Open: to agree with firmware** |
| DD-09 | Daily record is written at 00:00 local time for the finished day. "Compaction" = deleting 10-min records older than 30 days. DST days (23/25 h) handled explicitly. | S10.4, S10.7 | Agreed 2026-10-27 |
| DD-10 | Each aggregate stores a `sample_count`; daily averages are weighted by it; graphs show gaps. | S10.3, S10.4, S9 | Agreed 2026-10-27 |
| DD-11 | No records are timestamped until the clock is valid (NTP or RTC); a "time invalid" event is logged. | S10.10, S10.16 | Agreed 2026-10-27 |
| DD-12 | SQLite WAL with `synchronous=FULL` (write load is tiny) to meet "power loss loses at most the last record". | S10.1 | Agreed 2026-10-27 |
| DD-13 | The 1 h RAM buffer is not persisted; live graphs start empty after a reboot. | S10.2 | Agreed 2026-10-27 |
| DD-14 | One HTTP/JSON API (axum) for both the touchscreen app and the laptop dashboard: served on a Unix socket for the display, on TCP for laptops. The display app has no database access. | S8, S9 | Agreed 2026-10-27 |
| DD-15 | A single `db_writer` thread owns the only SQLite write connection; all other tasks send it messages (mpsc). Readers use separate read-only connections (WAL). | S10.1 | Agreed 2026-10-27 |
| DD-16 | On BMS comm loss (OFFLINE, 3 s) the fans hold their last duty while waiting for the link to come back (e.g. MCU restart). If it does not recover within `FAN_OFFLINE_TIMEOUT` the fans are switched off. Timeout value: **open** (proposal 5 min). | S6, S8.12 | Agreed 2026-10-27 (value open) |
| DD-17 | Crash policy: `panic = "abort"`, systemd `Restart=always` + `WatchdogSec`; any task failure restarts the whole core. No task-level restarts. Fan behaviour while the core is down depends on the fan controller hardware and S6 hardware fallback: **open** (see D10). | S6, S7.4 | Agreed 2026-10-27 (fallback open) |
| DD-18 | Live data is pushed to display and dashboard with Server-Sent Events; actions use normal HTTP requests. | S8, S9.4 | Agreed 2026-10-27 |
| DD-19 | Touchscreen UI toolkit: Slint. | S1.2, S8 | Agreed 2026-10-27 |
| DD-20 | One `event` table for faults, warnings and system events (`kind` = FAULT / WARNING / SYSTEM); the fault log and event log pages are filters on it. | S9.9, S9.10, S10.5 | Agreed 2026-10-27 |
| DD-21 | A fault snapshot (±60 s of 1 s data) is taken for every FAULT and for every event with severity CRITICAL; other warnings get no snapshot. | S10.6 | Agreed 2026-10-27 |
| DD-22 | Every event stores the trigger value and unit plus a fixed set of context values (pack voltage, current, cell temperature, min/max cell group, SoC) at that moment. | S9.9, S10.5 | Agreed 2026-10-27 |
| DD-23 | Retention: 10-min records 30 days; daily records, events and SoH kept forever; snapshots deleted oldest-first only when the DB passes `DB_SIZE_SOFT_LIMIT` (proposal 150 MB), logged as an event. No automatic `VACUUM`. | S10.5, S10.7, S10.8 | Agreed 2026-10-27 |
| DD-24 | Runtime-changed settings (PIN hash, wrong attempts, lock) live in the DB `setting` table; static configuration (fan table, limits, `CAP_AH`) in a TOML file in `/etc/bess/`. Keys to be aligned with the MCU config file. | S6, S9.12 | Agreed 2026-10-27 (keys open) |
| DD-25 | Cloud upload keeps a watermark per table (last acknowledged key) instead of a queue table; events whose `cleared_ts` changed are re-sent by `id`. | S10.15 | Agreed 2026-10-27 |
| DD-26 | Core has a `uart_tx` task: heartbeat every 1 s, config request while waiting for config, fault reset command. The heartbeat stops if `link_supervisor` or `fan_controller` stops, so the MCU's E11 also covers a hung Pi app. | S2.4, S3.6, S3.8 | Agreed 2026-10-27 |
| DD-27 | BMS link states OFFLINE / WAIT_CONFIG / ONLINE, 3 s timeout on the monotonic clock from the last valid frame. Telemetry carries an MCU boot counter (or uptime) so the Pi detects an MCU restart and re-reads the config. | S2.4, S5, S8.12 | Agreed 2026-10-27 (field in D7) |
| DD-28 | Before any config is received (no fan table yet) the fans run at 100 %; after `FAN_OFFLINE_TIMEOUT` without a BMS they turn off. | S2.4, S6 | Agreed 2026-10-27 |
| DD-29 | While OFFLINE, a last-known ESTOP or FAULT state stays shown (with the comm-lost banner); otherwise OFFLINE is shown. | S8.9, S8.10, S8.12 | Agreed 2026-10-27 |
| DD-30 | `core_state.last_alive_ts` written every 60 s; each start logs `LOG_GAP` marked `clean` / `crash` / `power loss`. | S10.17 | Agreed 2026-10-27 |
| DD-31 | Clock validity read from `chrony`; events from before the clock was valid wait in a RAM queue (max 100); clock steps > 2 s logged as `TIME_ADJUSTED`. | S10.10, S10.16 | Agreed 2026-10-27 |
| DD-32 | Display shows "Monitor service restarting…" when the core is gone, separate from "BMS communication lost". | S7.4, S8.12 | Agreed 2026-10-27 |
| DD-33 | Event `kind` follows Table A severity: Warning → `WARNING`; Protection, Latched protection, Critical → `FAULT` (all get a snapshot, DD-21). | S4.1, S10.6 | Agreed 2026-10-27 |
| DD-34 | Fault flags in every telemetry frame are the truth; the fault message is only a fast path. Raise/clear from the difference between consecutive fault sets. | S5.8, S10.5 | Agreed 2026-10-27 |
| DD-35 | Trigger value from a `trigger_signal` column per code in the Table A source file, unless the fault message carries the measured value. | S4.1, S9.9 | Agreed 2026-10-27 |
| DD-36 | `event` has `fw_version` and `app_version` columns. | S1.12 | Agreed 2026-10-27 |
| DD-37 | On core start, reconnect and MCU restart, open events in the DB are reconciled with the first frame's fault set; nothing is raised or cleared while OFFLINE. | S9.9, S10.5 | Agreed 2026-10-27 |
| DD-38 | Fault reset is request/acknowledge with a request id and 2 s timeout; request and result are both logged. Optional: reset source in telemetry so BMS button resets are logged too. | S3.6, S9.12 | Agreed 2026-10-27 |
| DD-39 | E12 FAN_FAULT is detected, shown and logged only by the Pi; not sent to the MCU. | S6, S3.7 | Agreed 2026-10-27 |
