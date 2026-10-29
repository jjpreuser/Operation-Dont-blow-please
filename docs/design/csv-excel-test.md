# D12 – CSV export: Excel locale test

Status: **Dutch locale tested 2026-10-29**; English locale still to test on a TE laptop. Proposes DD-62 ([decisions.md](decisions.md)). Builds on D3 ([data-lifecycle.md](data-lifecycle.md) §6): CSV with `timestamp_utc` + `time_local` and units in the column names (DD-49).

Requirements: S10.11 (CSV export), S10.13 (CSV opens in Excel with one column per value).

## 1. Test

A 10-minute aggregate export (2 rows, header + `timestamp_utc`, `time_local`, `sample_count` and six signal columns, including a negative current, an empty value and integers) was written in four variants and opened in Excel 16 (Microsoft 365) on Windows 11 with region nl-NL (decimal `,`, list separator `;`), scripted through COM in the same way as a double-click. Excel uses the Windows separators for `.csv` regardless of how it is opened.

| Variant | Columns | `pack_v` 13.284 | `current_a` -1.25 | `t_cell_c` 24.6 | `time_local` |
|---|---|---|---|---|---|
| A: `,` and `.` (D3 format) | **1** (whole row as text in column A) | – | – | – | – |
| B: `sep=,` first line, then A | 9 | **13284** (number, wrong) | text | text | date ✓ |
| C: `;` and decimal `,` | 9 | 13.284 ✓ | -1.25 ✓ | 24.6 ✓ | date ✓ |
| D: `;` and `.` | 9 | **13284** (number, wrong) | text | text | date ✓ |

`timestamp_utc` (`2026-10-29T11:00:00Z`) stays text in every variant, which is fine: `time_local` is the column Excel uses as a date. An empty value stays an empty cell.

Results:

- The current D3 format (A) fails S10.13 in a Dutch-locale Excel.
- `sep=,` (B) is worse than useless: the columns split, but `.` is read as a thousands separator, so values with three decimals are silently multiplied by 1000 and the rest become text. Option 1 from D3 is rejected.
- Only semicolon + decimal comma (C) opens correctly in Dutch Excel. That file would fail in an English-locale Excel in the same way A fails here, so no single CSV works in both by double-click.

## 2. Proposal (DD-62)

The CSV export offers two formats, chosen on the export dialog:

| Format | Separator | Decimal | For |
|---|---|---|---|
| `standard` | `,` | `.` | English-locale Excel, scripts, Python/MATLAB, cloud (default) |
| `excel-nl` | `;` | `,` | Excel on a Dutch (or other decimal-comma) locale |

- The dashboard preselects `excel-nl` when the browser's number format uses a decimal comma (`Intl.NumberFormat`), otherwise `standard`. The user can change it; the last choice is remembered per browser.
- API: `GET /api/export.csv?…&format=standard|excel-nl` (default `standard`). Both are UTF-8 without BOM, ASCII only, CRLF line ends, same header and columns; only separator and decimal mark differ. The file name gets a `-nl` suffix for `excel-nl`.
- No `sep=` line in either format.
- The column layout from D3 stays as it is (`timestamp_utc`, `time_local`, units in the column names).

## 3. Still to test

On a TE laptop with an English (en-US or en-GB) Windows locale, double-click open:

1. `standard`: 9 columns, numbers correct, `time_local` as a date. Expected to pass (Excel's default CSV behaviour).
2. `excel-nl`: expected to fail like A here; confirms the format choice is needed.
3. Whether TE laptops in use are set to English or Dutch region, to pick the default.
