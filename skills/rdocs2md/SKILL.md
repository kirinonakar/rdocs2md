---
name: rdocs2md
description: Convert and read local documents (PDF, HWP/HWPX, DOC/DOCX, PPT/PPTX, XLS/XLSX, ODT/ODS/ODP, RTF, EPUB) to UTF-8 Markdown, HTML, JSON, or CSV with the offline rdocs2md CLI. Use when a task needs to extract, inspect, search, summarize, or re-render the content of a document file instead of guessing from the extension.
license: MIT
---

# rdocs2md

Offline Windows x64 CLI that converts 14 document formats — PDF, HWP, HWPX, DOC, DOCX, PPT, PPTX, XLS, XLSX, ODT, ODS, ODP, RTF, EPUB — to UTF-8 Markdown, HTML, JSON, or CSV. The format comes from the file signature, not the extension. No OCR.

`rdocs2md` is on `PATH`; if it is not resolvable, ask for the install directory and run `.\rdocs2md.exe` there. `--help` prints usage and `--list-formats` the JSON feature matrix.

## Quick start

```powershell
rdocs2md "report.pdf"                                     # writes report.md beside the input
rdocs2md "meeting-notes.hwpx" --stdout
rdocs2md "presentation.pptx" --slides 2-3 --mode layout --json "layout.json"
rdocs2md "report.pdf" --search "revenue" --pages 2-5 --limit 10
rdocs2md "book.xlsx" --format csv --sheet "Summary" -o "Summary.csv"
rdocs2md "reports/**/*.pdf" --recursive --out-dir "converted"
```

Default output is a UTF-8 `.md` file next to the input. Existing results may be overwritten; an output that resolves to an input file is an error.

## Options

| Option | Behavior |
|---|---|
| `-o`, `--output PATH`, `--out-dir DIR` | Result file for one input, directory for many |
| `--stdout` | Print one document, or JSON Lines per input (`input`, `content`) for a batch |
| `-r`, `--recursive` | Recurse into directories and wildcard scans |
| `--format md\|html\|json\|csv` | Output format, default `md` |
| `--mode semantic\|hybrid\|layout` | Rendering mode, default `hybrid` |
| `--pages RANGE` (`--slides`, `--sheets`, `--chapters`, `--range`) | 1-based inclusive units, e.g. `1-3,5` |
| `--sheet NAME` | Only the named workbook sheet; repeatable |
| `--json PATH` | Also write the JSON IR |
| `--extract-images`, `--image-dir DIR` | Export embedded raster images |
| `--search QUERY`, `--case-sensitive`, `--limit N` | Search text, return JSON |
| `-j`, `--jobs N` | Batch workers, 1-64, default CPU count |
| `--max-inflight-bytes SIZE` | Cap on input bytes converted at once, default `1GiB`, `0` removes it |
| `--timeout SECONDS` | Stop page or section parsing after this long, `0` removes it |
| `--version`, `--licenses`, `--help`, `--list-formats` | Info commands |

Exit codes: `0` success, `1` conversion or batch failure (each failed input is reported and the rest still run), `2` invalid option.

Batch inputs are files, directories, and quoted globs such as `"reports/**/*.pdf"`. Results keep the expanded input order, duplicates run once, and repeated base names get suffixes such as `report-2`. `--` stops option and wildcard parsing.

## Choosing a mode

- `semantic` — headings, lists, paragraphs, tables. Prefer it for summarizing, searching, and Markdown-aware tooling.
- `hybrid` (default) — logical structure plus HTML for vertical text and RTL paragraphs. Best general choice.
- `layout` — HTML paragraphs keeping line breaks, spacing, alignment, and character attributes. Only when placement matters.

## Ranges

A unit is a PDF page, slide, workbook sheet, EPUB spine item, or a stored section or page break in flow formats; some Word files split footnote and endnote groups into separate sections. Indexed formats skip units outside the range, so pass `--pages` instead of converting a whole large file. Sections keep their original 1-based `index`, and `sections` lists the selectable units.

## Search

`--search QUERY` matches literal text inside paragraphs and table cells and writes JSON to stdout instead of a converted file. Matching is case-insensitive unless `--case-sensitive`, `--limit` caps matches per file (default 50, max 200), and `--pages` narrows the searched units. It cannot be combined with `-o`, `--out-dir`, `--json`, or image extraction.

Cite from a hit using `block_id` (`s2-b1`, plus `-r2c3` for a table cell), `source_page` for PDFs and slides, and `address` for spreadsheet cells. Each hit carries `before`/`text`/`after` with up to 60 characters of context per side and `start_char`/`end_char` offsets. The reply reports `format`, `warnings`, `matches`, and `truncated`, which is `true` when hits exist beyond `--limit`.

## CSV and sheets

`--format csv` writes xls, xlsx, and ods workbooks as RFC 4180, quoting fields that contain a comma, a double quote, or a line break. Values are the displayed text, so cached formula results, dates, and percentages appear as shown; `--mode` has no effect and CSV is rejected for other formats.

`--sheet NAME` selects by title (ASCII case-insensitive, repeatable; an unknown name fails with the available titles) and `--sheets RANGE` by workbook position. Without either, every sheet is written in workbook order, each preceded by a `# Sheet: NAME` line — convert one sheet at a time when a consumer needs plain CSV. `--sheet` filters after the range.

## Images

`--extract-images` writes supported raster images next to the output, with converter-generated names and links relative to the output location; `--image-dir` sets the destination (a subdirectory per document in a batch).

## Memory and limits

Parsers hold each input and its major parts in memory. A single input is limited to 2 GiB, and reaching any limit fails that input with an error while the rest of a batch continues.

For large files or batches, lower `--jobs` or `--max-inflight-bytes` (default 1 GiB of input bytes in flight, `0` removes the cap). It counts input file bytes, not the parsed working set, so it reduces the peak without bounding it exactly; an input larger than the budget still runs alone. `--timeout SECONDS` bounds parsing time, not memory, and a parse that hits it stops early.

## Warnings

`warnings` in the JSON IR, and on stderr during conversion, flag recoverable problems. Hidden, white, sub-3 pt, and out-of-bounds text stays in the IR with `visibility_flags`; hidden sheets and `hidden_rows`/`hidden_columns` are marked. `ocr_needed_for_image_text` means a PDF page has pictures but no text layer — this tool does not run OCR, so report the gap instead of inferring the content.