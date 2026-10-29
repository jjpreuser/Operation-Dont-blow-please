# D12 – CSV export: Excel locale test

Status: **Tested 2026-10-29**; DD-62 agreed 2026-10-29 ([decisions.md](decisions.md)). The CSV targets English-locale Excel; a Dutch-locale variant is parked. Builds on D3 ([data-lifecycle.md](data-lifecycle.md) §6): CSV with `timestamp_utc` + `time_local` and units in the column names (DD-49).

Requirements: S10.11 (CSV export), S10.13 (CSV opens in Excel with one column per value).

## 1. Test

A 10-minute aggregate export (2 rows, header + `timestamp_utc`, `time_local`, `sample_count` and six signal columns, including a negative current, an empty value and integers) was written in four variants and opened in Excel 16 (Microsoft 365) on Windows 11, scripted through COM in the same way as a double-click. Excel uses the Windows number settings for `.csv` regardless of how it is opened. Two runs:

- **English:** decimal `.`, list separator `,`, thousands `,` (the Windows number settings switched for the test, then restored).
- **Dutch:** region nl-NL, decimal `,`, list separator `;`, thousands `.`.

| Variant | English Excel | Dutch Excel |
|---|---|---|
| A: `,` and `.` (D3 format) | ✓ 9 columns, all values correct numbers, `time_local` a date, empty value an empty cell | ✗ whole row in one column |
| B: `sep=,` first line, then A | ✓ same as A | ✗ columns split, but 13.284 becomes **13284** and -1.25 / 24.6 become text |
| C: `;` and decimal `,` | ✗ split on the decimal commas, garbage | ✓ all correct |
| D: `;` and `.` | ✗ whole row in one column | ✗ same corruption as B |

`timestamp_utc` (`2026-10-29T11:00:00Z`) stays text in every variant; `time_local` is the column Excel uses as a date.

Results:

- The D3 format (A) passes S10.13 in English-locale Excel.
- No single CSV works by double-click in both locales.
- `sep=,` must never be used: in Dutch Excel it silently multiplies values with three decimals by 1000.

## 2. Decision (DD-62)

The CSV export targets English-locale Excel: one format, comma-separated, `.` as decimal point, UTF-8 without BOM (ASCII only), CRLF line ends, no `sep=` line, columns as in D3. Agreed 2026-10-29.

Parked (detail for later): an optional Dutch-locale variant (`;` and decimal `,`, variant C above), selectable on export. Until then a Dutch-locale user opens the file with Data → From Text/CSV and chooses comma and `.`.

## 3. Still to check

- English run on a TE laptop by double-click (the run above switched only the number settings on a Dutch Windows, not the whole region). Expected to pass.
