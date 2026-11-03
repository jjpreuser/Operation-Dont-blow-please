# Software test plan

Draft, 2026-11-02. Covers all 135 software requirements (S1–S11) from the Miro Software Backlog. Levels L0–L5 and Cloud are defined in [test-levels.md](test-levels.md).

| Section | Scope | Requirements | Test status |
|---|---|---|---|
| [1. Firmware](#1-firmware) | Only the BMS firmware on the MCU | 35 | Listed only; tests to be written with the firmware team |
| [2. Combined](#2-combined) | Firmware and the Pi or the whole repo | 21 | Tests below |
| [3. Non-firmware](#3-non-firmware) | Pi app, fans and cloud | 79 | Tests below |

Fan requirements are all in section 3, including those that use a fault bit from the MCU.

Columns: **M** = MoSCoW, **V** = verification method on the card (I inspection, T test, D demonstration, A analysis). Limits come from the requirement cards; a limit marked TBC on the card is not final until confirmed (see [Open points](#4-open-points)). Tests of Pi behaviour use the MCU simulator (L3) unless they say otherwise.

## 1. Firmware

Tests to be written later.

| Area | Requirements |
|---|---|
| Process and code | S1.1, S1.4, S1.6, S1.7, S1.10 |
| Configuration | S2.1, S2.2, S2.3 |
| Protection logic | S3.1–S3.23 |
| AFE link | S5.7 |
| Debug tooling | S7.1, S7.2, S7.3 |

## 2. Combined

The tests marked "real MCU" need the firmware and a Pi connected over UART. Until then, only the Pi side can run, against the simulator.

### Repo and process

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S1.3 | Must | I | L0 | Check the repo layout | `/firmware`, `/monitor`, `/protocol` and `/docs` exist with the agreed content |
| S1.5 | Should | T | L1 | Run the hardware-free logic of both code bases on a PC | State machine, SoC, fan curve and limit checks build and pass with no hardware attached |
| S1.8 | Should | D | L3, L5 | Read both versions on the dashboard, raise a fault, read the stored event | Both are MAJOR.MINOR.PATCH, shown correctly and stored with the fault (DD-36) |
| S1.9 | Should | T | L0 | CI runs the build, format and static checks of both code bases | C: `-Wall -Wextra`, clang-format, cppcheck clean. Rust: `cargo fmt`, `cargo clippy` clean. Zero warnings |
| S1.11 | Should | I | L0 | Check the branch protection and a sample of merged pull requests | One repo; merges to main only through a pull request reviewed by another team member |
| S1.12 | Should | T | L0, L1 | CI runs the Unity/Ceedling and `cargo test` suites on a push | Runs on every push; every error code has trigger and recovery tests; the fan curve has tests |
| S1.13 | Must | I | L0 | Review `/docs` against the list on the card | All eight documents are present and current |
| S1.14 | Should | I | L0 | Follow each README on a clean machine; build the Doxygen and rustdoc output | Both READMEs work as written; public C functions and Rust items are documented |

### Configuration and error codes

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S2.4 | Must | T | L3, real MCU | Start with no CONFIG, then a valid one, an invalid one, and one with a new `boot_counter` or `config_revision` | No live values until a valid CONFIG is accepted; the Pi uses the received values and keeps no copy; a changed identity sends it back to WAIT_CONFIG (DD-24, DD-27). Faults still show while waiting (DD-69) |
| S4.1 | Must | I | L0 | CI regenerates all error tables from the one source file and compares them | Firmware, Rust, dashboard and documentation tables are identical; a deliberate mismatch fails CI. Check on real MCU that a code means the same in UART, LED, dashboard and database |

### UART link

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S5.1 | Must | D | L2, real MCU | Send known cell-group voltages and compare with the Pi's values | All 4 groups arrive in every telemetry frame, with the right values |
| S5.2 | Must | D | L2, real MCU | Same for pack voltage | Arrives in every telemetry frame, with the right value |
| S5.3 | Must | D | L2, real MCU | Same for current, while charging and discharging | Wire value is negative when charging (E5.8); the Pi inverts it once (DD-72) |
| S5.4 | Must | D | L2, real MCU | Same for all temperatures, with and without NTC4 | All fitted sensors arrive; an absent sensor is shown as N/A (DD-41) |
| S5.5 | Must | D | Real MCU | Trigger and clear faults; timestamp the fault detection and the FAULT_UPDATE on the wire with a logic analyser | ≤ 100 ms for new and cleared faults, over at least 20 trials |
| S5.6 | Should | D | L2, real MCU | Step through all states | The state in the telemetry matches the MCU state; an unknown value is shown as unknown (DD-68) |
| S5.8 | Must | T | Real MCU | Check the UART settings and log frame times for 1 h | 115200 8N1; telemetry every 1 s ± 10 %; fault timing as S5.5 |
| S5.9 | Must | T | L1, L2 | Decode valid frames; corrupt the CRC, length, header and version; send truncated frames and random bytes | Valid frames decode; bad frames are discarded and counted in S9.11; the parser resyncs and never crashes |
| S5.10 | Must | I | L0 | Review `/protocol` against the card; check both sides use it | IDs, field order, types, units, little-endian and checksum are all defined in one document |
| S5.11 | Should | T | L2 | CI generates reference frames from the C code and decodes them in Rust | Every message type decodes to the expected values; a changed field fails CI |

### Firmware update

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S7.5 | Should | D | Real MCU | Flash a new firmware version through SWD with the Pi running | Update succeeds; a version change event appears in the event log |

## 3. Non-firmware

### Pi app basics

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S1.2 | Must | I | L0 | Check the source and `rust-toolchain.toml` | Pi app is Rust; toolchain version is pinned |
| S7.4 | Must | D | L5 | Cold-start the Pi 10 times and time power-on to app on screen | Full-screen without login, ≤ 60 s every time |

### Fans

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S6.1 | Must | I | L5 | Inspect the wiring and code path | The Pi sets the duty through the fan controller; the MCU does not |
| S6.2 | Must | T | L1, L5 | L1: temperatures at, between and outside the table points, with 2 to 8 points, and a changed CONFIG table. L5: measure the PWM duty with a scope at each point | Linear interpolation; the table comes from CONFIG; duty within ± 1 % |
| S6.3 | Must | T | L1 | Battery higher, air higher, equal; each input invalid | Duty is the higher of the NTC1 and NTC3 tables; an invalid input demands 100 % (DD-52) |
| S6.4 | Must | T | L1 | Ramp up and down across 27, 30 and 45 °C; hold inside the band; start the core inside the band | On at ≥ 30 °C with 30 % duty; stays at ≥ 30 % until ≤ 27 °C; 100 % at ≥ 45 °C; no oscillation in the band |
| S6.5 | Must | T | L1, L3 | Set and clear E05 at a low temperature; set E05 and E-stop together | 100 % while E05 is active, curve again after it clears; E-stop gives fans off. The MCU owns the > 55 °C and ≤ 50 °C for 5 s limits |
| S6.6 | Should | T | L1, L5 | L1: simulated RPM just above and below the limit, shorter and longer than 5 s. L5: stall each fan in turn, then both | E12 after 5 s below 70 % of expected speed, naming the fan; not raised on a short dip; shown and logged by the Pi only (DD-39, DD-58). Dead-fan behaviour per DD-76 and DD-85 |
| S6.7 | Should | T | L5 | Disconnect the PWM line; remove the controller's power; stop the Pi; heat the thermostat past its set point; press the E-stop during the fallback | Fans go to 100 % in each case with no Pi software involved in the PWM-loss and thermostat cases; the E-stop stops the fans (E7.7) |
| S6.8 | Should | T | L1, L3 | Set and clear E17 at a low battery temperature | 100 % while E17 is active, curve again after it clears. The MCU owns the > 45 °C and ≤ 40 °C for 5 s limits |

### Touchscreen display

Run on the real touchscreen (L5) with values from the simulator. Automated checks at L3 run against the API (DD-14) that feeds the display.

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S8.1 | Should | D | L3, L5 | Send known pack voltages | Shown value matches the input |
| S8.2 | Should | D | L3, L5 | Send known currents | Shown value matches the input |
| S8.3 | Should | D | L3, L5 | Send known temperatures, including an invalid one | Shown value matches the input; invalid shows N/A (DD-41) |
| S8.4 | Could | D | L3, L5 | Send different values for each cell group | Each of the 4 groups shows its own value |
| S8.5 | Must | D | L3, L5 | Raise each code in Table A | Every code is displayed |
| S8.6 | Should | D | L3, L5 | Send charging, discharging and idle | The right indication is shown for each |
| S8.7 | Must | T | L3 | Check every Table B value against an input script; measure the update rate; cross midnight for "today" values | Unit, resolution and update rate match Table B; "today" resets at 00:00 local time |
| S8.8 | Must | D | L1, L3 | Send a known negative and positive wire current with a known voltage | Charging shows positive, discharging negative; power = pack voltage × current |
| S8.9 | Must | D | L1, L3, L5 | Step through all six states; send combinations of states | Priority is E-STOP > FAULT > OFFLINE > CHARGING / DISCHARGING > IDLE; a last-known ESTOP or FAULT stays shown while OFFLINE (DD-29) |
| S8.10 | Must | D | L3, L5 | Raise a warning, a protection fault and a critical fault; open every page; clear them | Code, name, description, severity and time are shown; a red banner on every page until a critical fault clears |
| S8.11 | Must | D | L1, L5 | Send values just either side of each yellow and red limit | Colour changes at the limits on the card and from CONFIG (DD-81) |
| S8.12 | Must | T | L3 | Stop the frames, then resume them; stop the core separately | Within 3 s "BMS communication lost" with all live values grey; recovers when frames return; a stopped core shows "Monitor service restarting…" instead (DD-32) |
| S8.13 | Could | D | L1, L3 | Known current, SoC, capacity and SoH; current either side of 0.2 A | Result matches the formula, shown as hh:mm; hidden when \|I\| < 0.2 A |

### Web dashboard

Run from a laptop on Ethernet (DD-03) against the Pi, with the simulator for repeatable values.

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S9.1 | Must | D | L3, L5 | Open the dashboard from the laptop | The Pi serves it |
| S9.2 | Must | T | L3 | Disconnect all networks except the laptop link; watch the browser's network log | Everything loads; no request leaves the Pi |
| S9.3 | Must | I | L3 | Open every page | Overview, Cells, Graphs, Fault log and System info exist |
| S9.4 | Must | D | L3 | Compare with known input | SoC, SoH, voltage, current, power, max temperature, state and faults are right; refresh 1 s |
| S9.5 | Should | D | L3 | Send cell groups with a known spread | Bar chart and table are right; min, max and ΔV are highlighted |
| S9.6 | Must | D | L3 | Replay a known profile | Graphs exist for fan speed, all temperatures, SoC, SoH, voltage, current and power, and match the input |
| S9.7 | Should | D | L3 | Select 1 h, 24 h, 7 days and 30 days; tap and hover on points | Each range shows the right period; the tooltip shows exact value and time |
| S9.8 | Could | D | L3 | Graph 10-minute and daily data with a known spread | Min–max band and average line match the stored data |
| S9.9 | Must | D | L3 | Raise and clear faults; use every filter and sort | All columns are right, labelled as value at report (DD-35); filters and sorting work |
| S9.10 | Should | D | L3 | Cause start-up, shutdown, state change, fault reset and firmware change | Each one appears in the event log |
| S9.11 | Should | D | L3 | Compare with CONFIG and known counters; send bad frames | Versions, battery configuration, uptime, cycle count, database size and error count are right |
| S9.12 | Should | D | L1, L3, real MCU | Reset and shutdown with right and wrong PINs; 5 wrong tries; wait out the lock; inspect the database; reset an active and a cleared fault on a real MCU | Read-only without a PIN; dialog and PIN required; lock for 5 min after 5 wrong tries; PIN stored hashed; reset results handled and logged (DD-38) |

### Data storage

Tests over days or years use a controllable clock in the Pi app.

| Req | M | V | Level | Test | Pass when |
|---|---|---|---|---|---|
| S10.1 | Must | T | L3, L5 | Check the journal mode; cut the Pi's power during writes, at least 20 times | WAL mode; `PRAGMA integrity_check` passes every time; at most the last record is lost |
| S10.2 | Should | I | L1, L3 | Run past 1 h and check the buffer; watch database writes | Holds the last 1 h of 1 s data; none of it goes to disk except snapshots (DD-04) |
| S10.3 | Must | T | L1, L3 | Replay a known profile across several intervals; include gaps and invalid fields | Min, avg, max, Ah and Wh match values calculated by hand; intervals align to :00, :10, … (DD-45); gaps and NULLs handled (DD-10, DD-41) |
| S10.4 | Must | T | L1, L3 | Advance the clock past midnight, including DST days and a Pi that was off at midnight | Daily record is right; 10-minute records over 30 days are deleted only for days with a daily record; missed days are caught up (DD-09, DD-47) |
| S10.5 | Must | T | L3 | Raise faults and events; time the write; try to age them out | Stored ≤ 1 s with timestamp, code, severity and values; never compacted or deleted automatically |
| S10.6 | Must | T | L3 | Raise a fault after 2 min of known data; raise a second inside the window; raise one just after start-up | ± 60 s of 1 s data is saved; an overlapping fault extends the snapshot (DD-07); warnings get no snapshot (DD-21) |
| S10.7 | Must | A | L3 | Analysis of the retention rules, checked by a run with an advanced clock | 10-minute data ≥ 30 days; daily, faults and events ≥ 2 years |
| S10.8 | Must | A | L3 | Size calculation, checked by filling a database with 4 years of simulated data | < 200 MB after 4 years; the soft limit removes oldest snapshots first and logs it (DD-23); storage ≥ 16 GB |
| S10.9 | Must | T | L1, L5 | Blocked: method not final (see Open points). Replay known charge or rest data | SoH matches a hand calculation; at most once per day; each value is stored and shown in the graph |
| S10.10 | Must | T | L1, L3 | Inspect stored values; display them in a summer and winter time zone setting; start without a valid clock | Stored as UTC ISO 8601 (DD-06), shown in local time; nothing is timestamped until the clock is valid (DD-11) |
| S10.11 | Must | D | L1, L3 | Export a chosen range and set of values, including a range across tiers | UTF-8, commas, header with units, ISO 8601 timestamps; content matches the database |
| S10.12 | Should | D | L3, L5 | Download from System info during logging; copy with `scp` over Ethernet | Both copies open and pass `PRAGMA integrity_check` |
| S10.13 | Could | D | Manual | Open an export in English-locale Excel (DD-62, D12 method) | One column per value; units in the header |
| S10.14 | Should | T | L3, L5 | Run 8 or more days with an advanced clock; with and without a USB stick; with the target full | One copy a day; newest 7 kept; failure logged as `BACKUP_FAILED`, logging continues (DD-48) |
| S10.15 | Could | T | L3 | Upload to a stub server; take it down; slow it down; search the source for credentials | Queued and retried without loss; local logging never blocked; no credentials in the source |
| S10.16 | Must | T | L5 | Compare with a reference clock with NTP; disconnect the network for 24 h; power-cycle offline | Error ≤ 2 s with NTP; drift ≤ 2 s/day without; the RTC keeps time |
| S10.17 | Should | T | L3, L5 | Clean restart, killed process and power cut | Logging resumes ≤ 60 s; the gap is logged as `clean`, `crash` or `power loss` (DD-30) |

### Cloud

All at the Cloud level, or by inspection. These tests start once TE IT approves external hosting (P1.7). The cloud design is not done yet, so these tests stay general until it is.

| Req | M | V | Test | Pass when |
|---|---|---|---|---|
| S11.1 | Could | D | Upload known data; view it as a TE user | All uploaded data types are viewable and match |
| S11.2 | Must* | T | Open every URL without login; log in with TE SSO and with an admin-made account | No page or data without login; both logins work |
| S11.3 | Should* | D | Try admin actions as a Viewer, directly through the API as well | Viewer is refused; Admin can manage users and recipients |
| S11.4 | Could | D | Compare with the uploaded values | All values and the last update time are right |
| S11.5 | Could | D | Select 24 h, 7 days, 30 days and 1 year | Same graphs as the dashboard; ranges are right |
| S11.6 | Could | D | Filter by date and severity; export CSV | Filters work; CSV matches the table |
| S11.7 | Should* | T | Time a 10-minute record and a fault from the Pi to the website | Records ≤ 2 min; faults ≤ 30 s |
| S11.8 | Must* | I | Inspect the traffic and key storage; try plain HTTP; revoke a key and try again | TLS only; device key in a protected file, not in the source; a revoked key is refused |
| S11.9 | Must* | T | Review the Pi's inbound paths; try to send commands from the website | No path exists for commands, settings or resets from the website to the BESS |
| S11.10 | – | I | None: stub, moved to P1.7 | Not applicable |
| S11.11 | Could | I | Upload from two device IDs | IDs are unique; both systems are shown separately |
| S11.12 | Could | T | Lighthouse on every page at Slow 4G; view at 360 px wide | Load < 3 s; usable at ≥ 360 px |
| S11.13 | Could | T | Raise and clear a fault; time both | Active error with all fields ≤ 60 s; in the fault log ≤ 60 s after clearing |
| S11.14 | Could | D | Force each condition | Green, yellow, red and grey at the right times |
| S11.15 | Could | T | Stop the heartbeat with an active and with no active error | Sent every 60 s; "Offline" and last-seen time after 5 min; never "no errors" while offline |
| S11.16 | Could | I | CI compares the website table with the single source (S4.1) | Codes and descriptions are identical |
| S11.17 | Could | D | Upload a known fault history, including faults older than 30 days | Count per code covers only the last 30 days |
| S11.18 | Could | T | Raise a critical fault and a warning, with recipients with and without warnings selected | Critical goes to all; warning only to those who chose it |
| S11.19 | Could | D | Add, edit and remove recipients as Admin; try 0 and 11 | Works without a software change; limited to 1–10 |
| S11.20 | Could | T | Time from error to email arrival | ≤ 5 min |
| S11.21 | Could | I | Review a received email | Subject format and all body fields are right; the link works |
| S11.22 | Could | T | Repeat one code within and after 30 min, using a second code as a check | No second email inside the window; the next email states the count |
| S11.23 | Could | T | Clear an error | A follow-up email is sent |
| S11.24 | Could | T | Stop the Pi for more than 15 min | The cloud sends an "offline" email; the Pi sends nothing |
| S11.25 | Could | I | Review the code and configuration | Emails come only from the cloud; no credentials in the source |
| S11.26 | Could | I | Compare the log with emails sent | Each has time, recipient and error code |
| S11.27 | Must | A | Safety analysis; check on the bench (L5) by stopping the website, internet and Pi during a fault | All protection stays on the BMS; no protection depends on these |

## 4. Open points

- **Firmware needed:** the "real MCU" tests in section 2 need the firmware team to work in this repo or supply a test build.
- **S6.6:** the card raises E12 at duty ≥ 30 %, DD-39 at duty > 0. Limits and time come from CONFIG. The dead-fan rule in DD-54 is still proposed.
- **S6.7:** depends on proposed DD-56, DD-59 and DD-86, and on the fan parts and bench tests (D10, D13).
- **S8.7:** expected values must be taken from Table B in the requirements PDF.
- **S9.3:** the card's page list leaves out the event log, but S9.10 requires one. DD-20 makes it a filter on the same table; to confirm with the requirements owner.
- **S10.9:** the card's rest-point method differs from the window method in proposed DD-63 to DD-65. The test can't be finalised until this is settled and firmware gives V_stop.
- **TBC limits:** S5.8, S6.7, S7.4, S8.11, S8.13, S9.12, S10.9, S10.16 and S10.17 contain values marked TBC on the card.
- **Verification method:** S5.5, S7.4, S8.9 and S9.12 have measurable limits or logic but are marked D; S10.2 and S11.8 are marked I. The tests above already measure them; ask the requirements owner to change the method.
- **Proposed numbers:** the trial counts (S5.5 20, S7.4 10, S10.1 20) and the 1 h log in S5.8 are proposals from this plan, not requirements.
