# Pi review of the UART interface draft 0.4

Status: **Draft 2026-10-27**, to be agreed in the Pi team and then sent to firmware.

From: Pi software. To: firmware (MCU STM32G071RB). About: [STM32_Pi_UART_Interface_Draft_V1.docx](../../Research/STM32_Pi_UART_Interface_Draft_V1.docx), review draft 0.4, protocol 0x02.

The draft works for the Pi as it stands: framing, CONFIG handshake, identity, fault bitmap and fault reset are all implementable without changes. Request 1 below is the only one that blocks Pi features.

## Request 1: SoC and cumulative energy counters in TELEMETRY and FAULT_UPDATE

**Problem.** TELEMETRY has no state of charge and no energy counters. The Pi needs both for:

| Pi feature | Requirement | Needs |
|---|---|---|
| SoC gauge, SoC graph, SoC colouring (< 20 %, > 85 %) | S8.11, S9.4, S9.6, S10.3 | SoC |
| Time to empty / time to full | S8.13 | SoC |
| Ah and Wh charged/discharged per 10 min and per day, equivalent cycles | S10.3, S10.4 | energy counters |
| SoH (Ah counted between two rest points / ΔSoC) | S10.9 | energy counters + SoC |

**Why the MCU.** The MCU already needs SoC: CONFIG carries `soc_min_cPct` / `soc_max_cPct` (E1.7). It samples current every 100 ms; the Pi only sees 1 s values. And Pi counting is lost on every Pi reboot, while an MCU counter carries on.

**Proposed layout** (payload grows from 36 to 54 bytes, same for TELEMETRY and FAULT_UPDATE; offsets 0–35 unchanged):

| Offset | Field | Type | Unit or meaning |
|---|---|---|---|
| 36 | soc_cPct | u16 | 0.01 % (same unit as CONFIG `soc_min_cPct`) |
| 38 | chg_mAh_total | u32 | mAh charged since MCU boot, monotonic |
| 42 | dis_mAh_total | u32 | mAh discharged since MCU boot, monotonic |
| 46 | chg_mWh_total | u32 | mWh charged since MCU boot, monotonic |
| 50 | dis_mWh_total | u32 | mWh discharged since MCU boot, monotonic |

- **Validity:** bit 10 = SoC, bit 11 = energy counters (currently unused, so no conflict). SoC may be invalid after boot until the MCU has an estimate; the Pi then shows "N/A".
- **Counters only need to count since MCU boot.** No flash persistence is needed: the Pi sees `boot_counter` change and takes a new baseline. Charge and discharge are separate so the Pi never has to split a net value.
- **Range:** u32 mAh/mWh is enough for decades at 9.6 Ah / ~123 Wh.
- **Rounding:** accumulate internally at a finer resolution and only truncate when sending, so the counters do not drift by a rounding step every 100 ms.

If firmware prefers, protocol version 0x03 can mark the change; the Pi will support only the agreed version.

**If this is not possible:** the Pi integrates current × voltage at 1 Hz and estimates SoC itself. That loses the count on every Pi reboot, gives a second SoC that can disagree with the MCU's E1.7 limits, and makes SoH (S10.9) much less reliable.

## Request 2 (optional): reset source

The Pi logs fault resets it sends. A reset with the BMS button is only visible to the Pi as faults clearing. Proposal for the reserved byte 27: bits 0–5 = counter of accepted resets (wraps at 64), bits 6–7 = source of the last one (1 = UART, 2 = button). The Pi logs a `FAULT_RESET` event with the source when the counter changes.

## Answers to points the draft leaves open for the Pi

| Draft point | Pi position |
|---|---|
| `fan_offline_action` = 0 (undecided) | The Pi runs the fans at 100 % while OFFLINE. Before any valid CONFIG the Pi also runs 100 %, with no time limit. We ask firmware/system to choose the production value (1 or 2) and `fan_offline_ms`. |
| Reset `request_id` policy (§8) | The Pi stores the last `request_id` in its database and continues from it after a restart, so it never reuses an id. Ids start at 1. |
| Pi-origin E12 (§5) | Stays Pi-only: detected, shown and logged on the Pi, not sent to the MCU. |
| Heartbeat health (§8) | The Pi sends HEARTBEAT only while its link task and fan control task are both running, so a hung fan task also triggers E11. |
| Logged identity (§9) | The Pi stores firmware version, `boot_counter` and `config_revision` with every event, and every CONFIG it validates (raw bytes + ACK status). |

## Editorial

- The document header says "review draft 0.4", the file name says "V1". Please use one version number.
- The draft notes requirement S7.1 still names TI/XDS110 instead of STM32/ST-LINK.
