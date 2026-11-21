# rdocs2md

<p align="center">
  <img src="app.png" alt="Logo" width="100" height="100" />
</p>

<h3 align="center">rdocs2md</h3>

rdocs2md is a simple portable executable written in Rust, with AI-agent-friendly MCP and CLI interfaces. This single offline executable for Windows x64 converts a wide range of document formats such as PDF, Hangul (HWP/HWPX), Office, OpenDocument, RTF, and EPUB into UTF-8 Markdown, HTML, JSON, or CSV (workbook sheets), keeping reading order and document structure as readable as possible.

## Quick start

Open the folder that contains the executable in PowerShell and run it.

```powershell
.\rdocs2md.exe "meeting-notes.hwpx" -o "meeting-notes.md"
.\rdocs2md.exe "report.pdf" --pages 2-4,7 --stdout
.\rdocs2md.exe "data.epub" --format html --extract-images -o "data.html"
.\rdocs2md.exe "document.doc" "data.ods" "reports/**/*.pdf" --recursive --out-dir "converted"
.\rdocs2md.exe "report.pdf" --search "revenue" --pages 2-5 --limit 10
.\rdocs2md.exe "book.xlsx" --format csv --sheet "Summary" -o "Summary.csv"
.\rdocs2md.exe --help
```

The default output is a `.md` file with the same name next to the input. The CLI may replace an existing result file, and a destination that resolves to an input file is rejected. `--stdout` writes one document, or one JSON line per successful file for a batch, while diagnostics go to stderr. Exit codes are `0` for success, `1` when a conversion or batch fails (each failed input is reported and the rest still run), and `2` for an invalid option.

`rdocs2md --help` lists every option. The [agent skill](skills/rdocs2md/SKILL.md) is the CLI reference: option defaults, batch and wildcard rules, range units, mode selection, image extraction, and search result fields.

## MCP setup

Set `command` to the actual EXE path and `--root` to the local folder where reading and writing results is allowed. The default root is the current folder of the process that starts the server.

```json
{
  "mcpServers": {
    "rdocs2md": {
      "command": "C:\\rdocs2md_path\\rdocs2md.exe",
      "args": ["mcp", "--root", "C:\\docu_path"]
    }
  }
}
```

To process local documents outside the root as well, set `args` in the MCP configuration as shown below and restart the server. `--allow-outside-root` applies to input documents and scans, output files and directories, and image folders. Relative paths are resolved against `--root`.

```json
"args": ["mcp", "--root", "C:\\docu_path", "--allow-outside-root"]
```

The server discloses the resolved root instead of only naming a scope: every path argument in `tools/list` starts with `MCP root: <path>`, and `initialize` returns the same text in `instructions` for clients that forward it to the model. An agent can therefore send root-relative paths without discovering the working directory of the server process, and only `--allow-outside-root` widens which paths are accepted. When the agent workspace root is not the MCP root, those descriptions and `instructions` tell the agent to send absolute paths under `MCP root`, so root-relative paths are used only when the two coincide.

| Tool | Example input |
|---|---|
| `list_supported_formats` | `{}` |
| `inspect_document` | `{"path":"presentation.pptx"}` |
| `convert_document` | `{"path":"data.epub","format":"html","mode":"hybrid","extract_images":true,"output_path":"data.html"}` |
| `extract_document_range` | `{"path":"report.pdf","range":"2-5,8","format":"json"}` |
| `search_document` | `{"path":"report.pdf","query":"revenue","range":"2-5","limit":10}` |
| `convert_documents` | `{"paths":["reports/*.pdf","data"],"recursive":true,"output_dir":"converted","format":"json"}` |
| `convert_document` (CSV) | `{"path":"book.xlsx","format":"csv","sheet":"Summary","output_path":"Summary.csv"}` |

All conversion tools accept `format` (`md`, `html`, `json`, or `csv`, default `md`), `mode`, `range`, `sheet`, `extract_images`, and `image_dir`. `convert_document` and `extract_document_range` accept an optional `output_path` with an extension that matches `format`. `convert_documents` requires `output_dir`, keeps existing result files, and reports `succeeded` and `failed` counts with each `output_path` or `error`; `jobs` and `max_inflight_bytes` behave as in the CLI. The parent of a new output or image directory must already exist, and extracted images go to a per-document folder next to the converted file or under `image_dir`.

`search_document` and `--search` match literal text within a paragraph or table cell, including text divided across formatting runs, and return snippets with up to 60 characters of context per side plus the source section, page, block id, and an Excel cell address or PDF coordinates for citation. The [skill](skills/rdocs2md/SKILL.md#search) documents every field.

The server handles line-delimited JSON-RPC 2.0: `initialize`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call`, negotiating 2024-11-05, 2025-03-26, 2025-06-18, or 2025-11-25. The inline result limit is 512 KiB for the whole reply, including metadata and JSON escaping, so large conversions should use `output_path` or `range`; `omitted_results` counts batch entries dropped from a report. No tool returns `structuredContent`: reports, search hits, and summaries arrive once in the text block, and warnings are added as a second text block for clients that read only text. The server caches the most recently parsed documents for `inspect_document`, `search_document`, and conversions without images (8 entries, 128 MiB), dropping a cached version as soon as its size or modification time changes. Closing the input stream ends the session. [MCP lifecycle specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)

## Visibility and PDF image warnings

The JSON IR keeps detected white, sub-3 pt, hidden, and out-of-bounds text and marks its runs with `visibility_flags`. Document warnings summarize detections. Bounds checks cover PDF page rectangles and top-level PPTX shape coordinates. Workbook sections mark hidden sheets and 1-based `hidden_rows` and `hidden_columns` ranges, and affected parsed cells carry `hidden_row` or `hidden_column` properties. Detection uses the formatting and geometry available to each parser. White text can be readable on a dark background.

A PDF page with pictures and no extracted text is marked `ocr_needed_for_image_text`. Text visible inside those pictures requires OCR. Pages drawn with vector outlines can also contain visible text that text extraction cannot read.

## Agent skill

[`skills/rdocs2md/SKILL.md`](skills/rdocs2md/SKILL.md) is the CLI reference written for coding agents and assumes `rdocs2md` is on `PATH`. Copy the `skills/rdocs2md` folder into your agent's skill directory and restart the agent. The skill stays CLI-only and does not duplicate the MCP tool schemas; connect the MCP server as described above when structured tool calls are preferred.

## Per-format implementation status

All 14 formats implement body text extraction and conversion. The supported range of formatting and layout is as follows.

| Format | Implemented | Known limitations |
|---|---|---|
| PDF | Objects and streams, pages and Form XObjects, font character mapping, text origins, simple multi-column and regular cell-grid heuristics, link and underline annotations, JPEG and some RGB and grayscale images | No OCR for scanned pages. Irregular tables, formulas, rotated or vertical text, and multi-column layouts spanning full pages may restore with lower accuracy |
| HWP | 3.x single-stream files with raw DEFLATE, KSSM text, paragraphs, tables and object captions; 5.x CFB with compressed BodyText, UTF-16 paragraphs, merged and nested table cells, embedded raster pictures with `--extract-images` | HWP 3.x image extraction and detailed layout/style restoration are partial. HWP 5 distribution-protected files and general vector shapes are unsupported |
| HWPX | HPF ordering, sections and paragraphs, character attributes, table and cell merges, BinData images, equation script text | Headers, page layout, and placement of composite objects are partially supported |
| DOC | Word 97–2003 FIB and CLX piece table, main text, field results, explicit breaks | Binary table structures, per-character styles, other stories, and images are unsupported |
| DOCX | Sections and explicit breaks, headings, numbering and styles, links, footnotes and endnotes, referenced headers and footers, nested and merged tables, images, and basic equations | Partial support for floating layout, multi-level numbering formats, per-page header inheritance, and advanced equations |
| PPT | Persist directory of the current edit, slide list, outline and shape text | Masters, notes, images, and precise shape placement are unsupported |
| PPTX | Slide order from relationship files, titles, text boxes, tables, pictures, links, partial shape positions, speaker notes | Partial support for group transforms, charts and SmartArt, master inheritance, and animations |
| XLS | BIFF8 sheet names and order, continued SST, RK, number, string and boolean values, formula caches | Earlier BIFF versions, recovering the original text of binary formulas, pictures, and custom display formats are unsupported |
| XLSX | Sheet order, sparse cells, shared strings, separate formulas and caches, some dates and percentages, merges, links, and hidden metadata | Formula recalculation, complete custom number formats, charts, and images are unsupported |
| ODT | Headings, character style inheritance, lists, links, annotation bodies, explicit breaks, tables, images, and basic MathML | Partial support for master pages, footnote number placement, and complex column and page styles |
| ODS | Sheets, repeated rows and columns, display text, original date, time and currency values, formulas, merges, separated data ranges | Repeat expansion is capped. Partial support for full display-format calculation and visual restoration of hidden columns |
| ODP | Slides, ordering by frame coordinates, titles, tables, picture and group bodies, notes | Rotated group coordinates, master formatting, and animations are unsupported |
| RTF | Groups and control words, code pages, Unicode fallback characters and surrogates, fonts and colors, line and paragraph, basic lists and tables, link fields, and pictures | Partial support for multi-level list definitions, nested tables, and compound fields |
| EPUB | EPUB 2/3 OPF and spine, NCX and nav tables of contents, XHTML, basic CSS, links, images, ruby readings, vertical text and RTL, MathML, SVG text | DRM and encrypted resources are unsupported. Full CSS, ruby typesetting, and SVG graphics are unsupported |

Image file names are generated by the converter. Original names are never used as disk paths. If a different image already uses the same name, a new name is chosen. Source resource references are kept in the JSON. No external OCR, formula evaluator, browser engine, or office application is run.

## Implementation and input limits

The shared model in `src/model.rs` stores documents, sections, paragraphs, text runs, tables, styles, and source positions. `package.rs` handles ZIP/XML-based formats, `hwp3.rs` handles single-stream HWP 3.x files, and `legacy.rs` handles legacy CFB-based formats. OOXML, ODF, HWPX, and EPUB share the text and table conversion in `flow.rs`. The CLI and MCP use the same parsers and renderers.

| Item | Limit or behavior |
|---|---|
| Input file | Regular files up to 2 GiB (2,147,483,648 bytes). Format identified by signature and internal structure |
| ZIP | Up to 32,768 entries, 512 MiB each for the declared total uncompressed size and for cumulative reads, up to 64 MiB per entry read |
| XML and RTF | XML tree depth and RTF group depth of 128, 1.5 million nodes per XML part. No external entity resolution and no macro execution |
| Tables | A 250,000-cell threshold applies to XML tables, spreadsheet expansion, and recovered PDF grids. Repeated data has separate byte limits |
| PDF | Stream and page decoding limits, Form nesting and cycle checks, total content budget |
| MCP | Request frames of 1 MiB. The default access scope is the configured root and can be widened at server start with `--allow-outside-root`. Parsed documents kept for read-only tools are limited to 8 entries and 128 MiB |

Parsers process files and major parts in memory. Reaching a limit can end the run with an error. Decrypting encrypted documents and verifying digital signatures are not supported.

## Build and verification

```powershell
cargo build --release --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings

# Download the public samples once at pinned revisions, then include the real-document tests for all 14 formats
.\tests\fetch-samples.ps1
cargo test --offline --locked -- --include-ignored

# Test environment with the official Python MCP SDK 1.x installed
python tests/mcp_client.py

# Re-measure run times of the Windows release build
python tests/benchmark.py
```

The Cargo release output is `target/release/rdocs2md.exe`. Windows MSVC targets link the CRT statically. Once the dependencies have been fetched, you can rebuild with `cargo build --release --offline --locked`. Python and PowerShell scripts are used only for verification and notice generation.

Measured on 2026-11-20 on Windows x64 with Rust 1.99.1 Release. The times below include file reading, process startup, Markdown rendering, and writing to stdout, with output redirected to NUL. Median of 7 runs after 1 warm-up run.

| Target | Result |
|---|---:|
| Single EXE | 3,318,784 bytes (3.17 MiB) |
| 14 public samples, 287,102 bytes total, 14 process runs | 130.0 ms (129.2–145.8 ms) |
| Generated 16 MiB RTF, 1 process run | 102.5 ms (101.5–104.0 ms) |

Real-document regression tests check known source phrases across **14 formats × 3 modes**. Separate tests verify Unicode, stdio calls from the official SDK, ranges and batching, image re-extraction, table merges and nesting, formulas, multi-column order, corrupt files, and expansion limits. `dumpbin /dependents` listed only the four standard Windows DLLs. This verification is sample-based and does not measure full-page visual fidelity. Sample sources and pinned revisions are in the download script. [Apache Tika test data](https://github.com/apache/tika/tree/2487f2b125d3e908808a1f450ed8e83e5869fc87/tika-parsers) · [python-hwpx examples](https://github.com/airmang/python-hwpx/tree/679225782cc07c2fe0c265bc1208fa679a266c8e/examples)

## Dependencies and licenses

There are 9 direct dependencies. Versions are pinned in `Cargo.lock`.

| Crate | Purpose |
|---|---|
| zip | Reading the ZIP central directory and compressed entries |
| flate2 | Rust-backed DEFLATE, HWP streams, and decompression of embedded notices |
| roxmltree | XML structure and namespace traversal |
| cfb | Traversing sectors, directories, and streams of OLE compound files |
| glob | Matching wildcard input paths |
| encoding_rs | RTF code pages and character encodings |
| lopdf | PDF objects, streams, resources, and font character mapping |
| serde | Serialization of the shared document model |
| serde_json | JSON output and MCP messages |

The project source is under the [MIT license](LICENSE). For the 76 external packages (including build tools) in the Windows target Cargo metadata, the declared licenses and notice files were verified. None of these packages require GPL or AGPL. The original texts, including BSD and Unicode data notices, are in [third-party notices](THIRD-PARTY-NOTICES.txt). The distributed EXE prints the project license, Cargo dependency notices, and Rust standard library notices with `--licenses`. After updating versions, regenerate the notices with `python tools/notices.py`.

This product was developed with reference to Hancom's published documentation for the HWP document file format (.hwp).
