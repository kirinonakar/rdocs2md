---
name: rdocs2md
description: Convert and read local documents (PDF, HWP/HWPX, DOC/DOCX, PPT/PPTX, XLS/XLSX, ODT/ODS/ODP, RTF, EPUB) to UTF-8 Markdown, HTML, or JSON with the offline rdocs2md CLI, preserving reading order and document structure. Use when a task needs to extract, inspect, search, summarize, or re-render the content of a document file instead of guessing from the extension.
license: MIT
---

# rdocs2md

`rdocs2md.exe` is a single offline Windows x64 executable that converts 14 document formats to UTF-8 Markdown, HTML, or JSON. No OCR is involved.

The executable is registered on `PATH`, so call `rdocs2md` directly. If it is not resolvable, ask for the install directory and run `.\rdocs2md.exe` from there.

## When to use

- Read or summarize the body text and structure of a local document.
- Feed document content into an LLM-friendly form (Markdown/HTML) or a structured form (JSON IR).
- Extract only a few pages, slides, sheets, chapters, or sections.
- Export embedded raster images next to the converted output.

## Supported formats

PDF, HWP, HWPX, DOC, DOCX, PPT, PPTX, XLS, XLSX, ODT, ODS, ODP, RTF, EPUB. The format is identified by signature and internal structure, not by file extension.

`rdocs2md --list-formats` prints the per-format feature matrix as JSON.

## Quick start

```powershell
rdocs2md "report.pdf"
rdocs2md "meeting-notes.hwpx" -o "meeting-notes.md"
rdocs2md "report.pdf" --pages 2-4,7 --stdout
rdocs2md "presentation.pptx" --slides 2-3 --mode layout --json "layout.json"
rdocs2md "book.epub" --extract-images --image-dir "book images" -o "book.md"
rdocs2md "statistics.xlsx" --sheets 2 --format html -o "sheet.html"
rdocs2md "document.doc" "data.ods" "presentation.odp" --out-dir "converted"
rdocs2md "reports/**/*.pdf" -o "converted" --json "ir" --image-dir "images"
rdocs2md "report.pdf" --search "revenue" --pages 2-5 --limit 10
rdocs2md --help
```

Default output is a `.md` file with the same name beside the input. Outputs are written as UTF-8 and may overwrite existing result files; an input/output path collision is an error.

## Options

| Option | Behavior |
|---|---|
| `-o`, `--output PATH` | File for one explicit input, directory for multiple inputs or a scan |
| `--out-dir DIRECTORY` | Destination directory, created if missing |
| `--stdout` | Write one document or JSON Lines (`input` and `content` per successful file). Diagnostics go to stderr |
| `-r`, `--recursive` | Include subdirectories when scanning directories or wildcard inputs |
| `--format md\|html\|json` | Output format, default `md` (`markdown` is accepted) |
| `--mode semantic\|hybrid\|layout` | Rendering mode, default `hybrid` |
| `--pages RANGE` | 1-based inclusive range, e.g. `1-3,5` |
| `--slides`, `--sheets`, `--chapters`, `--range` | Aliases of `--pages` |
| `--json PATH` | Also save the selected IR to a file for a single explicit input or per-file JSON in a directory for a batch |
| `--extract-images` | Extract supported embedded raster images |
| `--image-dir DIRECTORY` | Image destination, with a subdirectory per document for a batch. Implies extraction |
| `--search QUERY`, `--case-sensitive`, `--limit N` | Search extracted text and return JSON with snippets, section indexes, and available cell addresses or coordinates. Case-insensitive with 50 matches per file unless a limit from 1 to 200 is given |
| `--list-formats`, `--version`, `--licenses`, `--help` | Support matrix (JSON), version, license notices, usage |

Exit codes: `0` success, `1` conversion or batch failure (each failed input is reported and the rest still run), `2` invalid option.

A batch accepts files, directories and quoted glob patterns such as `"reports/**/*.pdf"`. Directory scans include supported extensions, files from one input run in sorted order, duplicates run once, and repeated base names get suffixes such as `report-2`. `--` disables option and wildcard parsing for the remaining input paths.

## Search

`--search QUERY` matches literal text inside a paragraph or table cell, including text split across formatting runs, and writes JSON to stdout instead of a converted file. Combine it with `--pages` to narrow the searched units. Matching is case-insensitive unless `--case-sensitive` is given, and `--limit` caps matches per file (default 50, range 1-200). `--case-sensitive` and `--limit` are rejected without `--search`, and search cannot be combined with `-o`, `--out-dir`, `--json`, or image extraction.

Each match carries `section_index`, `section_kind`, `source`, a 1-based `path` through blocks and table cells, `before`/`text`/`after` with up to 60 characters of context per side, and zero-based `start_char`/`end_char` offsets within the paragraph or cell (exclusive end). Spreadsheet cells add `address`, and positioned PDF blocks add `x_pt` and `y_pt`. The result reports `format`, `warnings`, `matches`, and `truncated`, which is `true` when more matches exist than the limit allowed. A batch search writes one JSON line per input with `input` and `result`.

## Choosing a mode

- `semantic` — headings, lists, paragraphs, tables. Best for summarizing, searching, and Markdown-aware tooling.
- `hybrid` (default) — logical structure plus HTML for vertical text, RTL paragraphs, and similar cases. Best general choice.
- `layout` — HTML paragraphs that keep line breaks, whitespace, alignment, indentation, and character attributes. Best for fixed-layout fidelity; output is HTML-heavy.

Reach for `layout` only when spacing and placement matter.

## Ranges

Units follow the document model: PDF pages, presentation slides, workbook sheets, EPUB spine items, and stored sections or explicit page breaks for flow-based documents. Indexed formats skip conversion of units outside the range. Flow formats read the content needed to locate section boundaries. Inspect `sections` in the JSON IR to see the selectable units.

## Images

`--extract-images` writes supported raster images next to the output. File names are generated by the converter (original source names are never used as disk paths), and links in the output are relative to the output location.

## Warnings

Parsers record `warnings` in the JSON IR and print them to stderr during CLI conversion. Detected white, hidden, sub-3 pt, and out-of-bounds text stays in the IR with `visibility_flags` on its runs, and workbook sections mark hidden sheets plus 1-based `hidden_rows` and `hidden_columns` ranges. A PDF page with pictures but no extracted text warns `ocr_needed_for_image_text`, because text inside pictures needs OCR that this tool does not run.
