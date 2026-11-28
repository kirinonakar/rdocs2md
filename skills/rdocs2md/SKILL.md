---
name: rdocs2md
description: Read, search, or convert local PDF (.pdf), Hangul (.hwp, .hwpx), Office (.doc, .docx, .ppt, .pptx, .xls, .xlsx), OpenDocument (.odt, .ods, .odp), RTF (.rtf), and EPUB (.epub) files with the offline rdocs2md CLI.
license: MIT
---

# rdocs2md

Run `rdocs2md INPUT... [OPTIONS]` from PATH or use the installed `rdocs2md.exe` by its path. The tool detects formats by content. `--list-formats` lists supported features and limitations.

```powershell
rdocs2md "report.pdf" --pages 2-5 --stdout
rdocs2md "reports/**/*.pdf" --search "revenue" --limit 10
rdocs2md "book.xlsx" --format csv --sheet "Sales" -o "sales.csv"
```

## Conversion and batch options

| Option | Behavior |
|---|---|
| `--stdout` | Write one result to stdout. Batch output is JSON Lines with `input` and `content`. |
| `-o` / `--output PATH`, `--out-dir DIR` | Save a single file or write a batch to a directory. |
| `--format FORMAT` | `md` (default), `html`, `json`, or `csv` for XLS/XLSX/ODS. Select one sheet for plain CSV. |
| `--mode MODE` | Markdown rendering. `semantic` favors structure, `hybrid` (default) uses HTML for vertical or RTL text, and `layout` retains spacing and placement where supported. |
| `--layout flow\|document` | HTML only. `flow` is the default. `document` emits paper-style pages for document viewers. |
| `--pages RANGE` | Select 1-based inclusive units, e.g. `1-3,5`. Aliases are `--slides`, `--sheets`, `--chapters`, and `--range`. |
| `--sheet NAME_OR_NUMBER` | Repeatable workbook sheet filter by title or 1-based number. Applies after the range. |
| `--json PATH` | Also save the structured document as JSON. Use a directory for a batch. |
| `--extract-images`, `--image-dir DIR` | Save separate images. `--image-dir` sets the destination and implies extraction. |
| `--include-images` | Keep images embedded in HTML when exporting copies. HTML embeds supported images by default. |
| `-r` / `--recursive` | Include subdirectories when scanning folders or globs. |
| `-j` / `--jobs N`, `--max-inflight-bytes SIZE` | Batch workers (1–64, default CPU count) and concurrent input-byte budget (default 1 GiB, `0` disables). |
| `--timeout SECONDS` | Optional per-file PDF parsing budget. Expiration can leave partial output with warnings. `0` disables it. |

By default, conversion writes beside the input and can overwrite existing results. For read-only work, use `--stdout` without file or image exports. Use `--format json --stdout` to see original range indexes in `sections`.

Pass files, folders, or quoted globs. `**` spans subdirectories. Use `--` before literal filenames that look like flags or globs. A batch deduplicates inputs and reports results in input order. Exit code `1` signals failed inputs, so check stderr and retain successful results. Exit code `2` signals invalid options.

## Search

`--search QUERY` matches literal text in paragraphs and table cells and writes JSON to stdout. Use `--case-sensitive` for exact case, `--limit N` for matches per file (default 50, range 1–200), and `--pages` to narrow the search. Search runs without output destinations, `--json`, or image options.

A batch writes one `{input, result}` JSON line per successful file. Read `matches`, `warnings`, and `truncated` before treating results as complete. Cite the file and available `source_page`, `source_section_index`, `block_id`, or spreadsheet `address`. Use `before`, `text`, and `after` for context.

The input limit is 2 GiB per file. Check warnings and `visibility_flags` when citing hidden or obscured text. A PDF page marked `ocr_needed_for_image_text` needs OCR to recover text from images.
