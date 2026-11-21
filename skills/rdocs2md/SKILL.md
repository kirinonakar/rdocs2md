---
name: rdocs2md
description: Convert and read local documents (PDF, HWP/HWPX, DOC/DOCX, PPT/PPTX, XLS/XLSX, ODT/ODS/ODP, RTF, EPUB) to UTF-8 Markdown, HTML, or JSON with the offline rdocs2md CLI, preserving reading order and document structure. Use when a task needs to extract, inspect, search, summarize, or re-render the content of a document file instead of guessing from the extension.
license: MIT
---

# rdocs2md

`rdocs2md.exe` is a single offline Windows x64 executable that converts 14 document formats — PDF, HWP, HWPX, DOC, DOCX, PPT, PPTX, XLS, XLSX, ODT, ODS, ODP, RTF, EPUB — to UTF-8 Markdown, HTML, or JSON. The format is identified by signature and internal structure, not by file extension. No OCR is involved.

The executable is registered on `PATH`, so call `rdocs2md` directly. If it is not resolvable, ask for the install directory and run `.\rdocs2md.exe` from there. `--list-formats` prints the per-format feature matrix as JSON and `--help` prints usage. The MCP server exposes the same options as structured tool fields; `README.md` covers MCP setup.

Use it to read, summarize, search, or re-render the body text and structure of a local document, to produce an LLM-friendly (`md`, `html`) or structured (`json`) form, to extract only a few pages, slides, sheets, or sections, or to export embedded raster images.

## Quick start

```powershell
rdocs2md "report.pdf"                                    # writes report.md beside the input
rdocs2md "meeting-notes.hwpx" --stdout
rdocs2md "presentation.pptx" --slides 2-3 --mode layout --json "layout.json"
rdocs2md "book.epub" --extract-images --image-dir "book images" -o "book.md"
rdocs2md "reports/**/*.pdf" --recursive --out-dir "converted" --format json
rdocs2md "report.pdf" --search "revenue" --pages 2-5 --limit 10
```

Default output is a `.md` file with the same name beside the input, written as UTF-8. Existing result files may be overwritten; a destination that resolves to an input file is an error.

## Options

| Option | Behavior |
|---|---|
| `-o`, `--output PATH` | File for one explicit input, directory for multiple inputs or a scan |
| `--out-dir DIRECTORY` | Destination directory, created if missing |
| `--stdout` | Write one document, or JSON Lines (`input` and `content`) for a batch; diagnostics go to stderr |
| `-r`, `--recursive` | Include subdirectories when scanning directories or wildcard inputs |
| `-j`, `--jobs N` | Parallel workers for a batch, 1 to 64, default the detected CPU count |
| `--max-inflight-bytes SIZE` | Cap on the input bytes converted at once, e.g. `512`, `4MiB`, `1.5GiB`; default 1 GiB, `0` removes the cap |
| `--format md\|html\|json` | Output format, default `md` (`markdown` is accepted) |
| `--mode semantic\|hybrid\|layout` | Rendering mode, default `hybrid` |
| `--pages RANGE` | 1-based inclusive range, e.g. `1-3,5` |
| `--slides`, `--sheets`, `--chapters`, `--range` | Aliases of `--pages` |
| `--json PATH` | Also save the selected IR to a file, or per-file JSON in a directory for a batch |
| `--extract-images` | Extract supported embedded raster images |
| `--image-dir DIRECTORY` | Image destination, with a subdirectory per document for a batch; implies extraction |
| `--search QUERY`, `--case-sensitive`, `--limit N` | Search extracted text and return JSON; see [Search](#search) |
| `--list-formats`, `--version`, `--licenses`, `--help` | Support matrix (JSON), version, license notices, usage |

Exit codes: `0` success, `1` conversion or batch failure (each failed input is reported and the rest still run), `2` invalid option.

A batch accepts files, directories and quoted glob patterns such as `"reports/**/*.pdf"`; `--recursive` adds subdirectories and directory scans pick up supported extensions. Files from one input run in sorted order, duplicates run once, repeated base names get suffixes such as `report-2`, and `--` stops option and wildcard parsing. A batch with more than one file runs on several workers (`--jobs 1` forces sequential), `--max-inflight-bytes` lowers the worker count when large inputs would otherwise be parsed at once, and per-file results keep the expanded input order.

## Search

`--search QUERY` matches literal text inside a paragraph or table cell, including text split across formatting runs, and writes JSON to stdout instead of a converted file. Narrow the searched units with `--pages`. Matching is case-insensitive unless `--case-sensitive` is given, and `--limit` caps matches per file (default 50, range 1-200). `--case-sensitive` and `--limit` require `--search`, which cannot be combined with `-o`, `--out-dir`, `--json`, or image extraction.

Each match carries `section_index` and `source_section_index`, `section_kind`, `source`, a 1-based `path` through blocks and table cells, a stable `block_id` such as `s2-b1` (`-r2c3` for a table cell), `before`/`text`/`after` with up to 60 characters of context per side, and zero-based `start_char`/`end_char` offsets within the paragraph or cell (exclusive end). PDF pages and slides add `source_page`, spreadsheet cells add `address`, and positioned PDF blocks add `x_pt`, `y_pt`, plus `bounding_box` when the parser recorded a size. Use these fields to cite a page, block, or rectangle. The result reports `format`, `warnings`, `matches`, and `truncated`, which is `true` when matches exist beyond the limit. A batch search writes one JSON line per input with `input` and `result`.

## Choosing a mode

- `semantic` — headings, lists, paragraphs, tables. Best for summarizing, searching, and Markdown-aware tooling.
- `hybrid` (default) — logical structure plus HTML for vertical text, RTL paragraphs, and similar cases. Best general choice.
- `layout` — HTML paragraphs that keep line breaks, whitespace, alignment, indentation, and character attributes. Use it only when spacing and placement matter.

## Ranges

Units follow the document model: PDF pages, presentation slides, workbook sheets, EPUB spine items, and stored sections or explicit page breaks for flow-based documents; in some Word files footnote and endnote groups are separate sections. Indexed formats skip conversion of units outside the range, while flow formats read the content needed to locate section boundaries. Each section in the JSON IR keeps its 1-based `index` for its original position, so numbering stays valid when a range selected a subset. Inspect `sections` to see the selectable units.

## Images

`--extract-images` writes supported raster images next to the output, with names generated by the converter (original source names are never used as disk paths) and links relative to the output location.

## Warnings

Parsers record `warnings` in the JSON IR and print them to stderr during CLI conversion. Detected white, hidden, sub-3 pt, and out-of-bounds text stays in the IR with `visibility_flags` on its runs, and workbook sections mark hidden sheets plus 1-based `hidden_rows` and `hidden_columns` ranges. A PDF page with pictures but no extracted text warns `ocr_needed_for_image_text`, because text inside pictures needs OCR that this tool does not run.
