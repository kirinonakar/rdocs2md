# rdocs2md

<p align="center">
  <img src="app.png" alt="Logo" width="100" height="100" />
</p>

<h3 align="center">rdocs2md</h3>

rdocs2md is a simple portable executable written in Rust, with AI-agent-friendly MCP and CLI interfaces. This single offline executable for Windows x64 converts a wide range of document formats such as PDF, Hangul (HWP/HWPX), Office, OpenDocument, RTF, and EPUB into UTF-8 Markdown, HTML, or JSON, keeping reading order and document structure as readable as possible.

## Quick start

Open the folder that contains the executable in PowerShell and run it.

```powershell
.\rdocs2md.exe "meeting-notes.hwpx" -o "meeting-notes.md"
.\rdocs2md.exe "report.pdf" --pages 2-4,7 --stdout
.\rdocs2md.exe "presentation.pptx" --slides 2-3 --mode layout --json "layout.json"
.\rdocs2md.exe "book.epub" --extract-images --image-dir "book images" -o "book.md"
.\rdocs2md.exe "statistics.xlsx" --sheets 2 --format html -o "sheet.html"
.\rdocs2md.exe "document.doc" "data.ods" "presentation.odp" --out-dir "converted"
.\rdocs2md.exe "reports" --recursive -o "converted" --json "ir" --image-dir "images"
.\rdocs2md.exe "reports/**/*.pdf" --stdout
.\rdocs2md.exe "report.pdf" --search "revenue" --pages 2-5 --limit 10
.\rdocs2md.exe --help
```

The default output is a `.md` file with the same name next to the input file. The CLI may replace an existing result file. A destination that resolves to an input file is rejected. With a single explicit file, `--stdout` writes its rendered content. With multiple inputs or a directory or wildcard scan, `--stdout` writes one JSON line per successful file with `input` and `content` fields. `content` holds the rendered text, including serialized JSON when `--format json` is selected. Diagnostics go to stderr. Batch mode continues when a file fails or its parser panics and returns exit code 1 if any input failed. Invalid command-line options return exit code 2.

Pass files, directories and quoted patterns together. A directory scan discovers files with supported document extensions. A pattern matches filenames using `*`, `?`, or bracket sets such as `[ab]`. `**` matches across directory levels. `--recursive` scans subdirectories for a directory or a filename pattern such as `"reports/*.pdf"`. Discovered files are handled in sorted order within each input and duplicates are processed once. For batch output, repeated base names receive suffixes such as `report-2`. When a batch has more than one file, conversion runs on several workers by default; `--jobs 1` forces sequential processing, and stdout, stderr, JSON lines and the per-file report still follow the expanded input order.

| Option | Behavior |
|---|---|
| `--mode semantic` | Markdown built around headings, lists, paragraphs, and tables |
| `--mode hybrid` | Default mode. Keeps logical structure while using HTML for vertical text, RTL paragraphs, and similar cases |
| `--mode layout` | HTML paragraphs that preserve line breaks and whitespace, supported character attributes, alignment, indentation, and HTML tables |
| `--format md\|html\|json` | Renders the same document model in the specified format |
| `-o PATH`, `--out-dir DIR` | `-o` takes a filename for one explicit file and a directory for multiple inputs or a scan. `--out-dir` always takes a directory |
| `-j N`, `--jobs N` | Parallel conversion workers for batches, from 1 to 64. The default is the detected CPU count. stdout, stderr, JSON lines and the per-file report keep the expanded input order |
| `--max-inflight-bytes SIZE` | Caps the input bytes converted at once so that a large batch does not parse on every core. Accepts `512`, `4MiB`, or `1.5GiB`. The default is 1 GiB, and `0` removes the cap |
| `--json PATH` | Saves a separate JSON IR file for one explicit file or one JSON IR file per input in the given directory for a batch |
| `--pages`, `--slides`, `--sheets`, `--chapters`, `--range` | Aliases of the same range option. Comma-separated 1-based ranges, inclusive at both ends |
| `--extract-images`, `--image-dir DIR` | Extracts supported raster images and writes result links relative to the output location. Batch mode creates a separate image directory for each document |
| `--search QUERY`, `--case-sensitive`, `--limit N` | Searches extracted text and writes JSON. Matching is case-insensitive by default, with 50 results per file unless a limit from 1 to 200 is specified |
| `--list-formats`, `--version`, `--licenses` | Per-format support information (JSON), version, and built-in license notices |

Ranges follow the order of the document model. PDF uses pages, presentations use slides, workbooks use sheets, and EPUB uses spine items. Flow-based documents use stored sections or explicit page breaks. In some Word documents, footnote and endnote groups are separate items as well. You can check the selectable units with MCP `inspect_document` or with `sections` in the JSON. The converter reads shared metadata and indexes and skips content outside the requested range for PDF, PPT/PPTX, XLS/XLSX, ODS/ODP, EPUB, and HWP 5. DOC, RTF and HWPX walk forward to find section boundaries and stop at the last requested section. DOCX and ODT resolve explicit breaks from the parsed body before selecting sections.

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

| Tool | Example input |
|---|---|
| `list_supported_formats` | `{}` |
| `inspect_document` | `{"path":"presentation.pptx"}` |
| `convert_document` | `{"path":"data.epub","format":"html","mode":"hybrid","extract_images":true,"output_path":"data.html"}` |
| `extract_document_range` | `{"path":"report.pdf","range":"2-5,8","format":"json"}` |
| `search_document` | `{"path":"report.pdf","query":"revenue","range":"2-5","limit":10}` |
| `convert_documents` | `{"paths":["reports/*.pdf","data"],"recursive":true,"output_dir":"converted","format":"json"}` |

All conversion tools accept `format` (`md`, `html`, or `json`, default `md`). `convert_document` and `extract_document_range` accept an optional `output_path` for a new file with the matching extension. `convert_documents` requires `output_dir` and preserves existing result files. The result reports the `succeeded` and `failed` counts and lists each processed file with its `output_path` or `error`. If the response reaches the inline size limit, `omitted_results` reports the number of entries left out. With `extract_images: true`, the tool writes per-document image folders next to the converted files, or under `image_dir` if supplied. The parent of a new directory must already exist. `convert_documents` accepts `jobs` (1 to 64, default the detected CPU count), converts independent files on several workers, and still lists results in the expanded input order. `max_inflight_bytes` caps the input bytes converted at the same time (default 1 GiB, `0` removes the cap).

`search_document` and `--search` match literal text within a paragraph or table cell, including text divided across formatting runs. Each hit gives up to 60 characters on each side, the original 1-based section index, the source part and a 1-based path through blocks and table cells. Search also returns an Excel cell address or PDF block coordinates when available. Each hit also carries `source_section_index`, `source_page` (the PDF page or slide number), a stable `block_id` such as `s2-b1`, and a `bounding_box` in points when the parser recorded a size. `start_char` and `end_char` are zero-based character offsets within the paragraph or cell, with an exclusive end. `truncated` indicates additional matches beyond the limit. For CLI batch searches each line has `input` and `result` fields.

The server handles line-delimited JSON-RPC 2.0, `initialize`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call`. The negotiable versions are 2024-11-05, 2025-03-26, 2025-06-18, and 2025-11-25. The inline result limit for MCP tools is 512 KiB for the whole reply, including metadata and JSON escaping. Large conversion results can be saved with `output_path` or retrieved in pieces with `range`. No tool returns `structuredContent`: every result is the text block, so a `convert_documents` report, the `search_document` hits and the `inspect_document` summary are delivered there once. Conversion warnings are added as a second text block, so clients that read only text still see them. The server caches the most recently parsed documents for `inspect_document`, `search_document`, and conversions without images, and drops the previous version of a file as soon as its size or modification time changes, so one path never keeps two versions. Closing the input stream ends the session: cached documents are released and the process exits. [MCP lifecycle specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)

## Visibility and PDF image warnings

The JSON IR keeps detected white, sub-3 pt, hidden, and out-of-bounds text and marks its runs with `visibility_flags`. Document warnings summarize detections. Bounds checks cover PDF page rectangles and top-level PPTX shape coordinates. Workbook sections mark hidden sheets and 1-based `hidden_rows` and `hidden_columns` ranges, and affected parsed cells carry `hidden_row` or `hidden_column` properties. Detection uses the formatting and geometry available to each parser. White text can be readable on a dark background.

A PDF page with pictures and no extracted text is marked `ocr_needed_for_image_text`. Text visible inside those pictures requires OCR. Pages drawn with vector outlines can also contain visible text that text extraction cannot read.

## Agent skill

[`skills/rdocs2md/SKILL.md`](skills/rdocs2md/SKILL.md) is an agent skill that teaches a coding agent how to drive the CLI: supported formats, options, mode selection, range units, image extraction, and exit codes. It assumes `rdocs2md` is on `PATH`.

Copy the `skills/rdocs2md` folder into your agent's skill directory and restart the agent. The skill stays CLI-only and does not duplicate the MCP tool schemas; connect the MCP server as described above when structured tool calls are preferred.

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
