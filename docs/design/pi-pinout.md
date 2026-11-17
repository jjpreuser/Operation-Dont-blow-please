# Raspberry Pi 4 pin map

Status: **Proposed 2026-11-17** (DD-88), to be confirmed with electronics against the wiring diagram; the touchscreen is agreed (DD-89). The single list of what goes on which Pi pin; the reasons are in the design docs it points to. Fan pins come from D10 ([fan-hardware.md](fan-hardware.md), DD-58 agreed, DD-86 proposed); the MCU UART from the firmware UART draft V2 (115200 8N1, DD-40); the RTC from D5 ([lifecycle.md](lifecycle.md) §4, DD-79); the touchscreen is the Raspberry Pi Touch Display 2, 5-inch (DD-89, [Raspberry Pi documentation](https://www.raspberrypi.com/documentation/accessories/touch-display-2.html)). Open points in section 5.

GPIO numbers are BCM numbers (what the software uses); "pin" is the physical pin on the 40-pin header. Pin 1 is the square pad, the corner nearest the SD card slot; odd pins are on the inner row, even pins on the board edge.

## 1. Connections

| Function | Pi pin | GPIO / signal | Goes to | Notes |
|---|---|---|---|---|
| MCU UART, Pi transmits | 8 | GPIO14 TXD0 | STM32 USART RX | Linux `/dev/ttyAMA0` (PL011, section 3). 3.3 V on both sides, no level shifter |
| MCU UART, Pi receives | 10 | GPIO15 RXD0 | STM32 USART TX | Nothing else may write on this port, also no Linux serial console (UART draft §3) |
| MCU UART ground | 14 | GND | STM32 GND | Required: the UART has no other ground reference |
| Touchscreen power | 2 | 5 V | Touch Display 2 `J1`, red wire | Supplied cable; its 3-pin plug sits on pins 2, 4 and 6 (pin 4 covered, not connected). A wrong plug can damage the display |
| Touchscreen ground | 6 | GND | Touch Display 2 `J1`, black wire | Same plug |
| Fan 1 controller SDA | 3 | GPIO2 SDA1 | EMC2101 #1 SDA | I2C-1, `/dev/i2c-1`, address 0x4C. Fixed 1.8 kΩ pull-up on the Pi |
| Fan 1 controller SCL | 5 | GPIO3 SCL1 | EMC2101 #1 SCL | As above |
| Fan 2 controller SDA | 7 | GPIO4 (I2C-3 SDA) | EMC2101 #2 SDA | I2C-3 via `dtoverlay=i2c3`, `/dev/i2c-3`, address 0x4C (fixed, so it needs its own bus, DD-86). No pull-up on the Pi: the breakout's own is needed (section 5) |
| Fan 2 controller SCL | 29 | GPIO5 (I2C-3 SCL) | EMC2101 #2 SCL | As above |
| Fan controllers power | 1 | 3.3 V | VIN of both EMC2101 breakouts | Each breakout's FAN pull-up jumper is cut (DD-56) |
| Fan controllers ground | 9 | GND | GND of both EMC2101 breakouts | |
| Fan 1 tach | 11 | GPIO17 | Fan 1 tach wire | 10 kΩ pull-up to **3.3 V** (never 12 V) + RC filter (DD-58); edge events through `/dev/gpiochip0` |
| Fan 2 tach | 13 | GPIO27 | Fan 2 tach wire | As above |
| Tach pull-up supply | 17 | 3.3 V | Both 10 kΩ tach pull-ups | |
| Fan signal ground | 20 | GND | 12 V fan supply ground | The tach and PWM signals are measured against the fans' ground, so the 12 V supply ground must join the Pi ground |
| RTC SDA / SCL (proposal) | 3 / 5 | GPIO2 / GPIO3 | RTC module | Shares I2C-1 with fan 1 if the module's address is not 0x4C (a DS3231 is 0x68). Module not chosen yet (section 5) |
| RTC power (proposal) | 1 | 3.3 V | RTC module VCC | Shared with the EMC2101 VIN |
| RTC ground (proposal) | 25 | GND | RTC module GND | |

The fan PWM lines do **not** go to the Pi: each EMC2101 FAN pin drives its fan's PWM wire through the thermostat contact (D10 §2). The fans' 12 V comes through the safety relay (E7.7), not from the Pi. The touchscreen's picture and touch go over the DSI ribbon cable (section 4), not over header pins; only its power comes from the header.

## 2. Header overview

| Used for | Signal | Pin | Pin | Signal | Used for |
|---|---|---:|:---|---|---|
| EMC2101 ×2 VIN, RTC | 3.3 V | 1 | 2 | 5 V | Touchscreen power (red) |
| Fan 1 controller, RTC | GPIO2 SDA1 | 3 | 4 | 5 V | covered by the touchscreen plug |
| Fan 1 controller, RTC | GPIO3 SCL1 | 5 | 6 | GND | Touchscreen ground (black) |
| Fan 2 controller | GPIO4 (I2C-3 SDA) | 7 | 8 | GPIO14 TXD0 | MCU UART → STM32 RX |
| EMC2101 ground | GND | 9 | 10 | GPIO15 RXD0 | MCU UART ← STM32 TX |
| Fan 1 tach | GPIO17 | 11 | 12 | GPIO18 | free |
| Fan 2 tach | GPIO27 | 13 | 14 | GND | MCU UART ground |
| free | GPIO22 | 15 | 16 | GPIO23 | free |
| Tach pull-ups | 3.3 V | 17 | 18 | GPIO24 | free |
| free | GPIO10 | 19 | 20 | GND | 12 V fan supply ground |
| free | GPIO9 | 21 | 22 | GPIO25 | free |
| free | GPIO11 | 23 | 24 | GPIO8 | free |
| RTC ground | GND | 25 | 26 | GPIO7 | free |
| **do not use** (HAT EEPROM) | GPIO0 ID_SD | 27 | 28 | GPIO1 ID_SC | **do not use** (HAT EEPROM) |
| Fan 2 controller | GPIO5 (I2C-3 SCL) | 29 | 30 | GND | spare |
| free | GPIO6 | 31 | 32 | GPIO12 | free |
| free | GPIO13 | 33 | 34 | GND | spare |
| free | GPIO19 | 35 | 36 | GPIO16 | free |
| free | GPIO26 | 37 | 38 | GPIO20 | free |
| spare | GND | 39 | 40 | GPIO21 | free |

## 3. Pi configuration

`/boot/firmware/config.txt`:

```ini
enable_uart=1
dtoverlay=disable-bt      # PL011 UART on GPIO14/15 instead of the mini UART
dtparam=i2c_arm=on        # I2C-1 on GPIO2/3
dtoverlay=i2c3            # I2C-3 on GPIO4/5 (the overlay's default pins)
# dtoverlay=i2c-rtc,ds3231  once the RTC module is chosen (section 5)
```

- **Touchscreen:** a Pi 4 detects the Touch Display 2 by itself, so `config.txt` needs no display line (the `vc4-kms-dsi-ili9881-5inch` overlay is only for Compute Modules, or to pass touch options such as `invx`, `invy`, `swapxy`). The panel is natively portrait, 720 × 1280, output `DSI-1`. Landscape means rotating the output in cage and checking that touch rotates with it (section 5); the `cmdline.txt` `video=DSI-1:…,rotate=` option only turns the text console.
- Keep the Raspberry Pi OS and EEPROM up to date: the display needs a recent OS.

- **Serial console off:** remove `console=serial0,115200` from `/boot/firmware/cmdline.txt` (or `raspi-config` → Interface → Serial: login shell *No*, hardware *Yes*), so Linux never writes text on the MCU port. `hciuart.service` is not needed once Bluetooth is off.
- **Why the PL011:** the Pi 4 puts the mini UART on GPIO14/15 by default; its baud rate follows the core clock and it has a small FIFO. The PL011 has a fixed clock and a deeper FIFO, which suits the UART draft's burst handling (§3). Bluetooth is not used.
- `bess-core` opens `/dev/ttyAMA0` (or the `/dev/serial0` link), `/dev/i2c-1`, `/dev/i2c-3` and `/dev/gpiochip0`; its service user needs the `dialout`, `i2c` and `gpio` groups.

## 4. Other Pi connections (not on the header)

| Connector | Goes to | Notes |
|---|---|---|
| USB-C | 5 V supply | Pi power; it also feeds the touchscreen through pin 2 (section 5) |
| `DISPLAY` (DSI, 15-way) | Touch Display 2, 5-inch | The supplied 15-way to 15-way ribbon (the 22-to-15-way one is for the Pi 5); contacts face the Ethernet and USB-A ports. Carries picture and the 5-point capacitive touch. egui inside cage (DD-87, DD-89) |
| Ethernet | Laptop for the S9 dashboard, NTP when connected | DD-03, DD-79 |
| USB-A | USB stick for backups | S10.14, DD-48 |

On the bench the MCU is a Nucleo board on USB (`/dev/ttyACM0`, ST-LINK virtual COM port), as in Jelle's EMC2101 test rig. That is for testing only; in the cabinet the MCU board connects to GPIO14/15 as in section 1. `bess-core` takes the port name from its settings, so both work.

## 5. Open points

- **Pi power:** in the cabinet (E8 power supply) through USB-C; pins 2 and 4 are taken by the touchscreen plug, so they are not a power input. The supply must carry the Pi 4 and the display together: the official 5.1 V 3 A supply or an equal one from the cabinet's supply. The display's current is not published; measure it on the bench.
- **Screen orientation:** portrait (native, nothing to set) or landscape (rotated in cage, touch to check). Choose it with the screen layouts (S8).
- **Start-up time:** Raspberry Pi says the display can take up to a minute to show output after boot. S7.4 asks for the app full-screen within 60 s, so time it on the bench early (D8 reminder, DD-87).
- **RTC module:** not chosen (DD-79 needs one, the Pi 4 has none). The proposal is a DS3231 on I2C-1. If it has its own pull-ups, check the combined pull-up strength on I2C-1 together with the Pi's 1.8 kΩ.
- **I2C-3 pull-ups:** GPIO4/5 have no pull-ups on the Pi, so the EMC2101 #2 breakout must have its own SDA/SCL pull-ups to 3.3 V. Check on the Adafruit board.
- **Pin choice:** GPIO17/27 (tach) and GPIO4/5 (I2C-3) are the D10 proposal (DD-58, DD-86); GPIO14/15 for the MCU follows the Pi's standard UART. All to confirm with the wiring diagram and the electronics team; this doc and fan-hardware.md §2 change together.
- **Reset button / E-stop:** not on the Pi. Physical resets stay on the BMS (UART draft §8, E7.5).
