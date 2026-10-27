# D1 – Pi software architecture

Status: **Agreed 2026-10-27** (open points below). Follows DD-02 (core service + display app) and DD-03 (native Rust UI). Decisions made in this discussion go into [decisions.md](decisions.md).

## 1. Processes and external interfaces

```mermaid
flowchart LR
    MCU["MCU / BMS firmware"]
    FANS["Fans (PWM out, tach in)"]
    RTC["RTC module"]
    NTP["NTP server"]
    LAPTOP["Laptop browser (Ethernet)"]
    CLOUD["Cloud website (S11, optional)"]

    subgraph PI["Raspberry Pi"]
        CORE["bess-core\n(systemd service, headless)"]
        DISP["bess-display\n(native Rust UI, full-screen)"]
        DB[("SQLite WAL\n/var/lib/bess/bess.db")]
        BAK[("Daily backups\nlast 7")]
    end

    MCU -- "UART binary frames, CRC-16 (S5)" --> CORE
    CORE -- "UART: heartbeat, config request, fault reset" --> MCU
    CORE -- "PWM duty" --> FANS
    FANS -- "tach pulses" --> CORE
    RTC -. "time at boot" .-> CORE
    NTP -. "time sync" .-> CORE
    CORE <--> DB
    CORE --> BAK
    DISP <-- "Unix socket: HTTP/JSON API + live stream" --> CORE
    LAPTOP <-- "TCP: same API + web dashboard (S9)" --> CORE
    CORE -. "encrypted upload, queued + retried (S10.15)" .-> CLOUD
```

## 2. Inside bess-core

Boxes are async tasks (tokio) unless marked *thread*. Arrows are channels: `watch` = latest value only, `broadcast` = every sample to each listener, `mpsc` = work queue to a single owner.

```mermaid
flowchart TB
    subgraph IN["Input"]
        UART["uart_rx\nread serial, split frames,\nCRC-16, decode (protocol crate)"]
        LINK["link_supervisor\nOFFLINE / WAIT_CONFIG / ONLINE,\n3 s timeout (S8.12, D5)"]
        TX["uart_tx\nheartbeat 1 s, config request,\nfault reset (DD-26)"]
        CLOCK["clock_manager\nNTP/RTC status, gates timestamps (DD-11)"]
    end

    subgraph STATE["State hub"]
        LIVE["live state\nwatch&lt;LiveState&gt;\nlatest sample + derived state (S8.9)"]
        SAMPLES["sample stream\nbroadcast&lt;Sample&gt; 1 Hz"]
        RING["ring buffer\n3600 × 1 s, RAM only (DD-04, DD-13)"]
    end

    subgraph CTRL["Control"]
        FAN["fan_controller\ntable interp, max(batt, air), hysteresis,\n100 % on E05/E17, tach → E12 (S6)\nOFFLINE: hold last duty, off after timeout (DD-16)"]
    end

    subgraph LOG["Logging"]
        AGG["aggregator\n10-min min/avg/max + sample_count,\ndaily at 00:00 local (DD-09, DD-10)"]
        EVT["event_detector\nfault/state edges → events,\nopens fault snapshots (DD-07)"]
        WRITER["db_writer (thread)\nsole write connection,\nsynchronous=FULL (DD-12)"]
        MAINT["maintenance\nretention delete, size check,\ndaily backup keep 7"]
        UP["cloud_uploader (optional)\nreads upload queue, retries"]
    end

    subgraph API["Serving"]
        HTTP["api_server (axum)\nREST/JSON, live stream, CSV export,\nPIN-protected actions (S9)"]
        UDS["listener: Unix socket\n(display app)"]
        TCP["listener: TCP\n(laptop dashboard)"]
    end

    UART --> LIVE
    UART --> SAMPLES
    LINK --> LIVE
    LINK -- "alive" --> TX
    FAN -- "alive" --> TX
    HTTP -- "fault reset (mpsc)" --> TX
    CLOCK --> LIVE
    SAMPLES --> RING
    LIVE --> FAN
    FAN -- "fan duty / fan fault" --> LIVE
    SAMPLES --> AGG
    SAMPLES --> EVT
    RING -- "60 s pre-fault" --> EVT
    AGG -- "mpsc" --> WRITER
    EVT -- "mpsc" --> WRITER
    MAINT -- "mpsc" --> WRITER
    UP -- "mark sent (mpsc)" --> WRITER
    WRITER --> DBX[("SQLite")]
    DBX -. "read-only conns" .-> HTTP
    DBX -. "read-only conn" .-> UP
    LIVE --> HTTP
    RING --> HTTP
    HTTP --- UDS
    HTTP --- TCP
```

## 3. Proposed crate layout (Cargo workspace in `Pi/`)

| Crate | Kind | Contents |
|---|---|---|
| `protocol` | lib | Frame types, CRC-16, decoder; tested against C reference frames (S5). Shared with `/protocol` spec. |
| `bess-api` | lib | Serde types for the HTTP/JSON API and live stream (used by core, display and dashboard tests). |
| `bess-core` | bin | All tasks above. |
| `bess-display` | bin | Native touchscreen UI; only talks to `bess-api` over the Unix socket. |

## 4. Decisions

Agreed 2026-10-27, see [decisions.md](decisions.md):

- DD-14 One API (axum) for display (Unix socket) and laptop dashboard (TCP).
- DD-15 Single `db_writer` thread; read-only connections for readers.
- DD-16 On comm loss the fans hold their last duty; off if the link does not recover within `FAN_OFFLINE_TIMEOUT` (value open).
- DD-17 `panic = "abort"` + systemd restart/watchdog. Fan behaviour while the core is down: open (D10, fan controller hardware not chosen yet).
- DD-18 Live data via Server-Sent Events.
- DD-19 Display toolkit: Slint.

Open: `FAN_OFFLINE_TIMEOUT` value, fan controller hardware and S6 hardware fallback (D10).
