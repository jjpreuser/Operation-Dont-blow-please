# Pi review of the UART interface draft V2

Status: **In review 2026-10-29** in the Pi team, then sent to firmware. Drafted 2026-10-28; extended 2026-10-29 (§1 rewritten for D11, §3 extended, §4–§6 added).

From: Pi software. To: firmware (MCU STM32G071RB). About: [STM32_Pi_UART_Interface_Draft_V2.docx](../../Research/STM32_Pi_UART_Interface_Draft_V2.docx), protocol 0x03.

Thanks for adding SoC. The Pi will implement V2 as written: protocol 0x03, TELEMETRY/FAULT_UPDATE 38 bytes, `soc_cPct` at offset 36 with validity bit 10. The Pi shows and logs the MCU's SoC and does not calculate its own (DD-44). The points below are what is still open from the Pi side.

No energy counters are needed from the MCU: the Pi integrates Ah and Wh per 10 minutes and per day from the 1 s telemetry itself (S10.3, S10.4).

| § | Point | What we ask | Priority |
|---|---|---|---|
| 4 | Reference TELEMETRY frame has validity bits 5 and 9 swapped | Fix the frame and its generator | High |
| 6 | CONFIG validation must be the same list on both sides | Confirm our reading of two §7 rules, or send yours | High |
| 5 | A lost FAULT_UPDATE can hide a short fault | Confirm how `sequence` counts; optional `faults_seen` field | Medium |
| 1 | SoH needs the BMS charge-stop voltage | Value, fixed or compensated; send it in CONFIG | Medium |
| 3 | Pi answers to points V2 leaves open | Confirm, incl. production `fan_offline_action` = 2 and `state` 5 | Medium |
| 2 | Reset source in telemetry | Optional | Low |

## 1. SoH: the charge-stop voltage in CONFIG

SoH calculated from the Coulomb-counted SoC is always 100 %: Ah counted divided by a ΔSoC that is itself Ah counted / `capacity_mAh`. So the Pi measures SoH on the 8 A charge itself (D11, [soh.md](soh.md), DD-63). It counts the Ah from a lower pack voltage `V_low` up to the pack voltage where the BMS stops charging at 85 % (`V_stop`; the stop is on pack voltage, confirmed 2026-10-29), and compares that with the same window on the new battery. This replaces our earlier request for a `soc_reference_counter`, which is no longer needed.

The Pi must use the same `V_stop` as the MCU. Requests:

- The value of `V_stop`, and whether it is fixed or temperature-compensated (if compensated: the rule).
- Send it in CONFIG, so a change in `bess_config.h` reaches the Pi without a Pi update (the Pi keeps no copy of MCU settings, DD-24). Proposal: `charge_stop_mV` u16 (pack voltage, mV) appended at payload offset 190, offsets 0–189 unchanged, CONFIG schema 2, N = 192. Validation: nonzero and below 4 × `ov_trip_mV`. This changes the exact CONFIG length, so it needs a new protocol version; it can go in together with the optional `faults_seen` of §5.

Until it is in CONFIG, the Pi takes the value from its own setting file (soh.md §2). That copy must be updated by hand whenever the firmware value changes.

## 2. Reset source (optional, not in V2)

A reset with the BMS button is only visible to the Pi as faults clearing. Proposal for reserved byte 27: bits 0–5 = counter of accepted resets (wraps at 64), bits 6–7 = source of the last one (1 = UART, 2 = button). Low priority.

## 3. Pi answers to points V2 leaves open

| Draft point | Pi position |
|---|---|
| `fan_offline_action` = 0 (undecided) | The Pi runs the fans at 100 % while OFFLINE. Before any valid CONFIG the Pi also runs 100 %, with no time limit. For production the Pi proposes **2 (100 % while OFFLINE)** (DD-60): while OFFLINE the Pi has no temperatures, and if only the MCU → Pi direction fails the MCU keeps charging on our heartbeats, so "hold then off" could stop the fans during charging. `fan_offline_ms` is then unused. |
| Reset `request_id` policy (§8, "persistent/global ID policy remains P review") | The Pi stores the last `request_id` in its database and continues from it after a restart, so it never reuses an id. Ids start at 1. |
| Pi-origin E12 (§5) | Stays Pi-only: detected, shown and logged on the Pi, not sent to the MCU. |
| Heartbeat health (§8) | The Pi sends HEARTBEAT only while its link task and fan control task are both running, so a hung fan task also triggers E11. |
| Logged identity (§9) | The Pi stores firmware version, `boot_counter` and `config_revision` with every event, and every CONFIG it validates (raw bytes + ACK status). |
| SoC invalid (§4a) | The Pi shows "N/A" and stores NULL while bit 10 is clear; no SoC-derived values (time to empty/full) are shown then. |
| TELEMETRY `state` = 5 (OFFLINE, §4) | The Pi derives OFFLINE itself from the 3 s timeout (S8.12). We assume the MCU never sends 5; please confirm, or say when it does. A value outside 0–4 is shown as "unknown state" and logged (DD-68). |
| Reserved bits and bytes in TELEMETRY / FAULT_UPDATE (§4–5) | Validity bits 11–15 and byte 27 are ignored, not a frame error, so byte 27 can be used later (§2) without breaking older Pi software. A set fault bit 19–31 is shown and logged as an unknown fault (kind FAULT): it means firmware and Pi do not match, and hiding it would be worse (DD-68). |
| Reference frames (§10) | Checked by the Pi: CRC and length of all four are correct, and CONFIG decodes to exactly the §6–7 table values. TELEMETRY has a validity error, see §4. |

## 4. Reference TELEMETRY frame: validity bits 5 and 9 swapped

The §10 TELEMETRY example is described as "NTC4 absent, charging at −1.50 A". Its `validity` (bytes `DF 07` = 0x07DF, bits 0–4 and 6–10) says the opposite for two fields:

| Field | Value in the frame | Validity bit in the frame | Description says |
|---|---|---|---|
| `pack_current` | −150 (−1.50 A) | bit 5 = 0 (invalid) | valid, charging |
| `temperature[3]` (NTC4) | 0 | bit 9 = 1 (valid) | absent |

The CRC (`3D E4`) matches the published bytes, so the generator itself produced 0x07DF. Matching the description needs 0x05FF (bits 0–8 and 10). Corrected full frame:

`AA 55 03 01 26 00 E4 0C E4 0C E4 0C E4 0C 90 33 6A FF FD 00 FE 00 FF 00 00 00 01 00 00 00 00 FF 05 00 01 00 00 00 01 00 00 00 7E 1D BF CF`

Please fix the frame and check the generator's bit list before the frame goes into the C and Rust tests (S5.11). Otherwise both sides can pass the same test with the wrong meaning of bits 5 and 9.

## 5. A lost FAULT_UPDATE can hide a short fault

The Pi logs faults from the difference between the bitmaps of consecutive frames (DD-34). A fault that is raised and cleared between two TELEMETRY frames appears only in its two FAULT_UPDATE frames. If the raising frame is lost (CRC error), the Pi sees the bit stay 0 and the fault is never logged. A lost clearing frame only delays the clear to the next TELEMETRY.

The Pi will detect lost frames from the `sequence` byte (DD-66): a jump between two consecutive valid MCU frames is counted with the communication errors (S5.9) and logged as a system event "frames lost", so the log shows where a short fault may be missing. Requests:

- Confirm that `sequence` is one counter for all MCU → Pi frames (TELEMETRY, FAULT_UPDATE, CONFIG, RESET_ACK), +1 per frame.
- Also advance it for a frame the MCU drops itself (e.g. full TX queue), so the Pi sees that loss too.
- Optional, with the next protocol version: `faults_seen` u32 in TELEMETRY = every fault bit that was set at any time since the previous TELEMETRY. The Pi can then log a short fault even when its FAULT_UPDATE frames were lost (time known to within 1 s).

## 6. CONFIG validation: one shared list

If the Pi rejects a CONFIG that the MCU accepted (CONFIG_ACK status 2), the Pi stays in WAIT_CONFIG: no live values on the display, fans at 100 %, and no identity-matched heartbeat, so the MCU keeps charging inhibited. Both sides must therefore check exactly the same rules. The Pi checks the §7 list and adds no rules of its own (DD-67). Two §7 items are not exact enough to implement the same way twice; this is how the Pi reads them:

| §7 item | Pi reading (proposal) |
|---|---|
| Threshold ordering | `ov_recover < ov_trip`; `uv_trip < uv_recover < ov_recover`; temperature pairs with the recover value on the safe side: `chg_ut_trip < chg_ut_recover`, `chg_ot_recover < chg_ot_trip`, `dis_ut_trip < dis_ut_recover`, `dis_ot_recover < dis_ot_trip`, `air_recover < air_ot`; fan hysteresis `battery_fan_off < battery_fan_on`, `air_fan_off < air_fan_on` |
| Table endpoints consistent | Per table: first active point at or below the off point with 0 %; last active point 100 %; every active point at or above the on point at least `fan_min_pct` |

The example CONFIG of §10 passes both. Please confirm, or send your exact list. Proposal: the list lives in the `/protocol` spec (S5) as the single source that both the C and the Rust tests check, like the error table (S4.1).
