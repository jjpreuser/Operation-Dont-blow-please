# Pi review of the UART interface draft V2

Status: **Draft 2026-10-28**, to be agreed in the Pi team and then sent to firmware.

From: Pi software. To: firmware (MCU STM32G071RB). About: [STM32_Pi_UART_Interface_Draft_V2.docx](../../Research/STM32_Pi_UART_Interface_Draft_V2.docx), protocol 0x03.

Thanks for adding SoC. The Pi will implement V2 as written: protocol 0x03, TELEMETRY/FAULT_UPDATE 38 bytes, `soc_cPct` at offset 36 with validity bit 10. The Pi shows and logs the MCU's SoC and does not calculate its own (DD-44). The points below are what is still open from the Pi side.

No energy counters are needed from the MCU: the Pi integrates Ah and Wh per 10 minutes and per day from the 1 s telemetry itself (S10.3, S10.4).

## 1. SoH needs an SoC reference that is not Coulomb counted

S10.9 defines SoH as *Ah counted between two rest points / ΔSoC*, divided by `CAP_AH`. If ΔSoC comes from the Coulomb-counted SoC, which itself is Ah counted / `capacity_mAh`, the result is always 100 % by construction. SoH only measures something if the SoC at the two points comes from an independent reference: open-circuit voltage at rest, or a full-charge / empty reference.

LFP has a very flat OCV curve between roughly 20 % and 85 %, so OCV-based SoC is only usable near the ends. V2 §4a already lists "validated full-charge reference conditions" and "periodic correction" as TBC. Request: when the MCU re-references SoC (full-charge point or a validated OCV point), say so in TELEMETRY, e.g. a `soc_reference_counter` u8 that increments on each re-reference plus the reference type. The Pi then counts Ah between two references for SoH.

Pi plan (D11, [soh.md](soh.md)): we understand the 85 % charge stop is on a pack voltage (please confirm). The Pi uses that stop as the top reference and counts the Ah from a lower voltage up to it, so the `soc_reference_counter` is not needed. Request instead: send the charge-stop voltage `V_stop` in CONFIG (and say whether it is temperature-compensated), so the Pi uses the same value as the MCU.

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
