# D1 – Pi software architecture

Status: **Agreed 2026-10-27**, revised the same day for the firmware UART draft (DD-40, [decisions.md](decisions.md)); open points below. Follows DD-02 (core service + display app) and DD-03 (native Rust UI).

## 1. Processes and external interfaces

```mermaid
flowchart LR
    MCU["MCU STM32G071RB / BMS firmware"]
    FANS["Fans (PWM out, tach in)"]
    RTC["RTC module"]
    NTP["NTP server"]
    LAPTOP["Laptop browser (Ethernet)"]
    CLOUD["Cloud website (S11, optional)"]

    subgraph PI["Raspberry Pi"]
        CORE["bess-core\n(systemd service, headless)"]
        DISP["bess-display\n(Slint UI, full-screen)"]
        DB[("SQLite WAL\n/var/lib/bess/bess.db")]
        BAK[("Daily backups\nlast 7")]
    end

    MCU -- "TELEMETRY 1 s, FAULT_UPDATE ≤ 100 ms,\nCONFIG, RESET_ACK" --> CORE
    CORE -- "HEARTBEAT 1 s, CONFIG_REQUEST,\nCONFIG_ACK, FAULT_RESET" --> MCU
    CORE -- "PWM duty" --> FANS
    FANS -- "tach pulses" --> CORE
    RTC -. "time at boot" .-> CORE
    NTP -. "time sync" .-> CORE
    CORE <--> DB
    CORE --> BAK
    DISP <-- "Unix socket: HTTP/JSON API + SSE" --> CORE
    LAPTOP <-- "TCP: same API + web dashboard (S9)" --> CORE
    CORE -. "encrypted upload, queued + retried (S10.15)" .-> CLOUD
```

UART: 115200 8N1, binary frames `AA 55`, protocol version 0x02, CRC-16/CCITT-FALSE (UART draft, DD-40).

## 2. Inside bess-core

Boxes are async tasks (tokio) unless marked *thread*. Arrows are channels: `watch` = latest value only, `broadcast` = every sample to each listener, `mpsc` = work queue to a single owner.

```mermaid
flowchart TB
    subgraph IN["UART link"]
        UART["uart_rx\nsplit frames, CRC-16, decode (protocol crate)\nTELEMETRY, FAULT_UPDATE, CONFIG, RESET_ACK"]
        LINK["link_supervisor\nOFFLINE / WAIT_CONFIG / ONLINE (DD-27)\nvalidates CONFIG, tracks boot_counter + config_revision"]
        TX["uart_tx\nHEARTBEAT 1 s (only while link + fans alive, DD-26),\nCONFIG_REQUEST, CONFIG_ACK, FAULT_RESET"]
        CLOCK["clock_manager\nchrony status, gates timestamps (DD-11, DD-31)"]
    end

    subgraph STATE["State hub"]
        CFG["mcu config\nwatch&lt;Option&lt;McuConfig&gt;&gt;\nlimits, CAP, fan tables (DD-24)"]
        LIVE["live state\nwatch&lt;LiveState&gt;\nlatest sample, link + shown state (S8.9)"]
        SAMPLES["sample stream\nbroadcast&lt;Sample&gt; 1 Hz (TELEMETRY only)"]
        FRAMES["fault frames\nbroadcast: every TELEMETRY + FAULT_UPDATE\nin arrival order (DD-34)"]
        RING["ring buffer\n3600 × 1 s, RAM only (DD-04, DD-13)"]
    end

    subgraph CTRL["Control"]
        FAN["fan_controller\nCONFIG tables, max(batt, air), hysteresis,\n100 % on E05/E17, E-STOP → off (DD-42),\nE12 from RPM (DD-39), OFFLINE per CONFIG (DD-16)"]
    end

    subgraph LOG["Logging"]
        AGG["aggregator\n10-min min/avg/max + sample_count,\ndaily at 00:00 local (DD-09, DD-10)"]
        EVT["event_detector\nbitmap diffs → events, reconcile (DD-37),\nopens fault snapshots (DD-07)"]
        WRITER["db_writer (thread)\nsole write connection,\nsynchronous=FULL (DD-12, DD-15)"]
        MAINT["maintenance\nretention delete, size check,\ndaily backup keep 7"]
        UP["cloud_uploader (optional)\nwatermark per table (DD-25)"]
    end

    subgraph API["Serving"]
        HTTP["api_server (axum)\nREST/JSON, SSE, CSV export,\nPIN-protected actions (S9)"]
        UDS["listener: Unix socket\n(display app)"]
        TCP["listener: TCP\n(laptop dashboard)"]
    end

    UART --> LINK
    UART --> LIVE
    UART --> SAMPLES
    UART --> FRAMES
    LINK --> CFG
    LINK --> LIVE
    LINK -- "request / ACK" --> TX
    CLOCK --> LIVE
    CFG --> FAN
    SAMPLES --> RING
    LIVE --> FAN
    FAN -- "duty, rpm, E12" --> LIVE
    FAN -. "alive" .-> TX
    LINK -. "alive" .-> TX
    SAMPLES --> AGG
    FRAMES --> EVT
    RING -- "60 s pre-fault" --> EVT
    AGG -- "mpsc" --> WRITER
    EVT -- "mpsc" --> WRITER
    LINK -- "config, link events (mpsc)" --> WRITER
    MAINT -- "mpsc" --> WRITER
    UP -- "watermark (mpsc)" --> WRITER
    HTTP -- "fault reset (request_id)" --> TX
    WRITER --> DBX[("SQLite")]
    DBX -. "read-only conns" .-> HTTP
    DBX -. "read-only conn" .-> UP
    LIVE --> HTTP
    CFG --> HTTP
    RING --> HTTP
    HTTP --- UDS
    HTTP --- TCP
```

`uart_tx` is the only writer to the serial port; `link_supervisor` and `api_server` send it messages. RESET_ACK goes from `uart_rx` back to the waiting `api_server` request (matched by `request_id`, 2 s timeout, DD-38).

## 3. Proposed crate layout (Cargo workspace in `Pi/`)

| Crate | Kind | Contents |
|---|---|---|
| `protocol` | lib | Frame envelope, CRC-16/CCITT-FALSE, encoder + decoder for all UART draft messages, CONFIG validation; tested against the draft's reference frames and C-generated frames (S5.11, DD-40). Shared with the `/protocol` spec. |
| `bess-api` | lib | Serde types for the HTTP/JSON API and live stream (used by core, display and dashboard tests). |
| `bess-core` | bin | All tasks above. |
| `bess-display` | bin | Native touchscreen UI; only talks to `bess-api` over the Unix socket. |

## 4. Decisions

See [decisions.md](decisions.md):

- DD-14 One API (axum) for display (Unix socket) and laptop dashboard (TCP).
- DD-15 Single `db_writer` thread; read-only connections for readers.
- DD-16 Fan behaviour on comm loss from CONFIG `fan_offline_action` / `fan_offline_ms` (revised, UART draft).
- DD-17 `panic = "abort"` + systemd restart/watchdog; a dead core stops the heartbeat → MCU E11, charging off.
- DD-18 Live data via Server-Sent Events.
- DD-19 Display toolkit: Slint.
- DD-26 / DD-27 `uart_tx` and link states (from D5, revised for the UART draft).
- DD-40 `protocol` crate implements the UART draft.
- DD-42 E-STOP → fans off.

Open: fan controller hardware and fan behaviour while the core is down (D10); SoC and Ah/Wh source (D9).
