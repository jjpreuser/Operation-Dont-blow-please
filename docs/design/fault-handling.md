# D6 – Fault and event handling

Status: **Agreed 2026-10-27** (open points in section 7). Builds on D2 ([database.md](database.md)): DD-07 (two-stage snapshot), DD-20 (one `event` table), DD-21 (snapshot for every FAULT / CRITICAL), DD-22 (trigger + context values), DD-25 (cloud watermark); and D5 ([lifecycle.md](lifecycle.md)): link states, `uart_tx`.

Requirements: S3.6 (explicit reset, rejected while active), S3.7 (LED priority), S4.1 / Table A (codes, one source), S5.8 (faults ≤ 100 ms), S8.10 (active faults, red banner), S8.11 (yellow/red values), S9.9 / S9.10 (fault and event log), S9.12 (PIN), S10.5 (stored ≤ 1 s, never deleted), S10.6 (±60 s snapshot), S11.7 (faults uploaded ≤ 30 s), S11.13–S11.27 (website, email by the cloud, safety never depends on it).

The MCU decides what a fault is. The Pi only records, shows and forwards it (S11.27). The one exception is E12 FAN_FAULT, which the Pi detects itself because it owns the fans (S6).

## 1. Sources

| Source | Kind | How the Pi learns it |
|---|---|---|
| MCU faults E01–E19 (except E12) | FAULT or WARNING | Fault flags in every telemetry frame + a fast fault message ≤ 100 ms (S5.8) |
| E12 FAN_FAULT | WARNING | `fan_controller` (tach below expected for the set time) |
| Link state changes | SYSTEM / `STATE_CHANGE` | `link_supervisor` (D5) |
| System state changes (IDLE → CHARGING, …) | SYSTEM / `STATE_CHANGE` | State field in telemetry |
| Fault reset requested / accepted / rejected | SYSTEM / `FAULT_RESET` | `api_server` + MCU reply |
| Start-up, shutdown, log gap, FW change, time invalid/adjusted, PIN lock, snapshot deleted | SYSTEM | Core (D5), `maintenance`, `api_server` |

`kind` from Table A severity (DD-33): Warning → `WARNING`; Protection, Latched protection, Critical → `FAULT`. So snapshots (DD-21) are taken for every Protection, Latched and Critical code.

## 2. Raise and clear

The **fault flags in each frame are the truth**; the fast fault message only makes the Pi react sooner (DD-34). `event_detector` keeps the set of active codes and compares each new set with it. That way a lost fault message, a duplicate, or a Pi restart cannot create a double or a missing event.

```mermaid
sequenceDiagram
    autonumber
    participant M as MCU
    participant RX as uart_rx
    participant L as live state
    participant E as event_detector
    participant W as db_writer
    participant API as api_server (SSE)
    participant UI as display / dashboard
    participant UP as cloud_uploader
    participant CL as Cloud (S11)

    M->>RX: fault message E05 (≤ 100 ms after detection)
    RX->>L: active faults += E05 (since = now)
    L-->>API: changed
    API-->>UI: SSE: state, active faults
    Note over UI: fault list (code, name, description, severity, time)<br/>red banner if Critical (S8.10), ≤ 1 s
    RX->>E: fault set changed: +E05
    E->>E: trigger value + context from latest sample (DD-22)
    E->>W: insert event (kind FAULT, cleared_ts NULL)<br/>+ snapshot with 60 s before from ring buffer (DD-07)
    W-->>E: event id
    Note over W: written ≤ 1 s after receipt (S10.5)
    loop every ~10 s until 60 s after the last fault
        E->>W: append snapshot_sample rows
    end
    UP->>W: read events after watermark (≤ 30 s, S11.7)
    UP->>CL: HTTPS upload (event + snapshot)
    Note over CL: website live errors, email for Critical<br/>sent by the cloud, not the Pi (S11.24)

    M->>RX: telemetry, E05 flag gone
    RX->>L: active faults −= E05
    API-->>UI: SSE: fault removed, banner off
    RX->>E: fault set changed: −E05
    E->>W: update event set cleared_ts
    UP->>CL: re-send event by id (DD-25)
```

Notes:

- **Trigger value** (DD-22, S9.9 "measured value at the moment of the fault"): the Table A source file gets a `trigger_signal` column per code (E01 → highest cell group V, E02 → lowest, E03/E04 → current, E05 → highest battery temp, E16 → `t_terminal_c`, E17 → `t_air_out_c`, E12 → `fan_rpm`, …), and the Pi reads it from the sample received with the fault. If the fault message carries the measured value itself, that is used instead (DD-35, D7).
- **Event timestamp** = when the Pi received it (D2). If the clock is not valid, the event waits in RAM (D5 §4).
- **Firmware and Pi version** with every fault (S1.12): two new columns `fw_version` and `app_version` on `event` (DD-36, small D2 change).
- **Repeated faults:** every raise is a new event row; nothing is merged locally (the log is never compacted, S10.5). Counting and the email repeat window are cloud features (S11.17, S11.22).

## 3. Reconciliation (Pi start-up, reconnect, MCU restart)

```mermaid
flowchart TB
    A["First valid frame after:<br/>core start, OFFLINE → ONLINE,<br/>or MCU restart"] --> B["Open events in DB<br/>(cleared_ts IS NULL)"]
    A --> C["Active fault set<br/>in this frame"]
    B --> D{"code in both?"}
    C --> D
    D -- "yes" --> E["Keep the open event<br/>(no duplicate)"]
    D -- "only in DB" --> F["Set cleared_ts = now<br/>detail: 'cleared while Pi/link was down'"]
    D -- "only in frame" --> G["New event, ts = now<br/>detail: 'active at reconnect, start time unknown'<br/>snapshot pre-part may have gaps"]
```

This is why the event detector loads all open events from the DB at start-up instead of starting with an empty set (DD-37). It also means a Pi reboot during an active Critical fault does not show the fault as new, and a fault that cleared during the reboot is closed.

While OFFLINE the Pi does not raise or clear anything: it cannot see the MCU, so the last known faults stay shown (D5 §6).

## 4. Display rules

| What | Rule |
|---|---|
| Active fault list | Sorted like the LED (S3.7): highest severity first, then lowest code. Shows code, name, description, severity, local time of occurrence (S8.10). |
| Red banner | On every page while any Critical fault is active (S8.10). Stays while OFFLINE (D5 §6). |
| Value colours | Yellow at the warning level, red at the protection limit (S8.11). Value-based: from the config limits (S2.4), not from events. A value can be red while the MCU has not tripped yet (detection delay). |
| Fault log page | `event WHERE kind IN ('FAULT','WARNING')`: time, code, name, severity, trigger value + unit, cleared time; filter by severity and date, sort by column (S9.9). |
| Event log page | `event WHERE kind = 'SYSTEM'` (S9.10). |
| Names, descriptions, severities | Only from the generated Table A module (S4.1). The DB stores only the code; text is looked up when shown, so all screens and the cloud use the same text. |

## 5. Fault reset (S3.6, S9.12)

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant UI as display / dashboard
    participant API as api_server
    participant W as db_writer
    participant TX as uart_tx
    participant M as MCU

    U->>UI: "Reset faults"
    UI->>U: confirmation dialog + PIN
    U->>UI: PIN
    UI->>API: POST /actions/fault-reset {pin}
    alt PIN wrong
        API->>W: setting pin_fail_count += 1 (lock after 5, 5 min)
        API-->>UI: 403 (attempts left / locked)
    else PIN locked
        API-->>UI: 423 locked until …
    else PIN OK
        API->>W: event FAULT_RESET "requested" (source: display or dashboard)
        API->>TX: send reset command (request id)
        TX->>M: RESET(request id)
        M-->>TX: RESET_ACK(request id, accepted | rejected: fault still active)
        TX-->>API: result (timeout 2 s → "no answer")
        API->>W: event FAULT_RESET accepted / rejected / no answer
        API-->>UI: result shown to the user
        Note over M: latched faults clear in the next frames<br/>→ normal clear path (section 2)
    end
```

- The Pi never clears a fault itself; only the MCU does, and the fault flags show it.
- Resets with the BMS button are seen only as faults clearing. A reset source field in the telemetry would let the Pi log them too (DD-38, optional).
- The dashboard on the laptop can reset (S9.12); the cloud website cannot (S11.9).

## 6. Events the Pi raises itself

| Event | Condition |
|---|---|
| E12 FAN_FAULT | Fan duty > 0 and tach below the expected speed for the set time (S6). Cleared when the speed is back. Shown and logged like any Table A code. Not sent to the MCU (DD-39). |
| STATE_CHANGE | MCU state field changed, or link state changed (D5). |
| PIN_LOCK | 5 wrong PINs. |
| SNAPSHOT_DELETED | Size guard removed old snapshots (DD-23). |

## 7. Decisions

Agreed 2026-10-27, see [decisions.md](decisions.md):

| ID | Decision |
|---|---|
| DD-33 | `kind` follows Table A severity: Warning → `WARNING`; Protection, Latched protection, Critical → `FAULT`. Snapshots for all `FAULT` events (DD-21). |
| DD-34 | Fault flags in every telemetry frame are the truth; the fault message is only a fast path. `event_detector` raises and clears from the difference between consecutive fault sets. |
| DD-35 | Trigger value from a `trigger_signal` column per code in the Table A source file, unless the fault message carries the measured value (D7). |
| DD-36 | `event` gets `fw_version` and `app_version` columns (S1.12). Change to D2. |
| DD-37 | On core start, reconnect and MCU restart, open events in the DB are reconciled with the first frame's fault set (section 3). Nothing is raised or cleared while OFFLINE. |
| DD-38 | Fault reset is a request/acknowledge with a request id and 2 s timeout; requested + result are both logged. Optional: reset source in telemetry so button resets are logged too. |
| DD-39 | E12 is detected, shown and logged only by the Pi; it is not sent to the MCU (no LED blink for E12). |

Open:

- Fault message contents (value, reset ack, boot counter) → D7 / `/protocol`.
- Exact "set time" for E12 detection → S6 / D4.
