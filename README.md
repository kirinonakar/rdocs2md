# rdocs2md

<p align="center">
  <img src="app.png" alt="Logo" width="100" height="100" />
</p>

<h3 align="center">rdocs2md</h3>

An offline document converter for Windows x64; its single executable contains both the CLI and the MCP stdio server, extracting reading order and document structure from PDF, Hangul (HWP/HWPX), Office, OpenDocument, RTF, and EPUB files and saving the result as UTF-8 Markdown, HTML, or JSON.

## Quick start

Open the folder that contains the executable in PowerShell and run it.

```powershell
.\rdocs2md.exe "meeting-notes.hwpx" -o "meeting-notes.md"
.\rdocs2md.exe "report.pdf" --pages 2-4,7 --stdout
.\rdocs2md.exe "presentation.pptx" --slides 2-3 --mode layout --json "layout.json"
.\rdocs2md.exe "book.epub" --extract-images --image-dir "book images" -o "book.md"
.\rdocs2md.exe "statistics.xlsx" --sheets 2 --format html -o "sheet.html"
.\rdocs2md.exe "document.doc" "data.ods" "presentation.odp" --out-dir "converted"
.\rdocs2md.exe --help
```

The default output is a `.md` file with the same name next to the input file. Both explicitly specified and default outputs can overwrite existing result files. A path collision between an input file and an output file is treated as an error. `--stdout` writes only the document and sends diagnostics to stderr. Batch mode records a result per input and returns exit code 1 if any of them fail. An invalid conversion option returns exit code 2.

| Option | Behavior |
|---|---|
| `--mode semantic` | Markdown built around headings, lists, paragraphs, and tables |
| `--mode hybrid` | Default mode. Keeps logical structure while using HTML for vertical text, RTL paragraphs, and similar cases |
| `--mode layout` | HTML paragraphs that preserve line breaks and whitespace, supported character attributes, alignment, indentation, and HTML tables |
| `--format md\|html\|json` | Renders the same document model in the specified format |
| `--json FILE` | Saves the IR of the selected range to a separate file. Includes source references, formulas and caches, actual coordinates, and warnings |
| `--pages`, `--slides`, `--sheets`, `--chapters`, `--range` | Aliases of the same range option. Comma-separated 1-based ranges, inclusive at both ends |
| `--extract-images`, `--image-dir DIR` | Extracts supported raster images and writes result links relative to the output location |
| `--list-formats`, `--version`, `--licenses` | Per-format support information (JSON), version, and built-in license notices |

Ranges follow the order of the document model. PDF uses pages, presentations use slides, workbooks use sheets, and EPUB uses spine items. Flow-based documents use stored sections or explicit page breaks. In some Word documents, footnote and endnote groups are separate items as well. You can check the selectable units with MCP `inspect_document` or with `sections` in the JSON. Range selection is applied after the full parse.

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

To process local documents outside the root as well, set `args` in the MCP configuration as shown below and restart the server. `--allow-outside-root` applies to input documents, new output files (`output_path`), and image folders (`image_dir`). Relative paths are resolved against `--root`.

```json
"args": ["mcp", "--root", "C:\\docu_path", "--allow-outside-root"]
```

| Tool | Example input |
|---|---|
| `list_supported_formats` | `{}` |
| `inspect_document` | `{"path":"presentation.pptx"}` |
| `convert_document` | `{"path":"data.epub","format":"html","mode":"hybrid","extract_images":true,"output_path":"data.html"}` |
| `extract_document_range` | `{"path":"report.pdf","range":"2-5,8","format":"json"}` |

Both conversion tools accept the optional `format` argument (`md`, `html`, or `json`). The default is `md`. When provided, `output_path` must name a new file with the corresponding `.md`, `.html`, or `.json` extension.

The server handles line-delimited JSON-RPC 2.0, `initialize`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call`. The negotiable versions are 2024-11-05, 2025-03-26, 2025-06-18, and 2025-11-25. The inline result text limit for MCP tools is 512 KiB. Large conversion results can be saved with `output_path` or retrieved in pieces with `range`. [MCP lifecycle specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)

## Per-format implementation status

All 14 formats implement body text extraction and conversion. The supported range of formatting and layout is as follows.

| Format | Implemented | Known limitations |
|---|---|---|
| PDF | Objects and streams, pages and Form XObjects, font character mapping, text origins, simple multi-column and regular cell-grid heuristics, link and underline annotations, JPEG and some RGB and grayscale images | No OCR for scanned pages. Irregular tables, formulas, rotated or vertical text, and multi-column layouts spanning full pages may restore with lower accuracy |
| HWP | 5.x CFB, compressed BodyText, UTF-16 paragraphs and control characters, merged and nested table cells, embedded raster pictures with `--extract-images` | Distribution (DRM) and encrypted documents, general shapes, and styles are unsupported |
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

The shared model in `src/model.rs` stores documents, sections, paragraphs, text runs, tables, styles, and source positions. `package.rs` handles ZIP/XML-based formats and `legacy.rs` handles legacy CFB-based formats. OOXML, ODF, HWPX, and EPUB share the text and table conversion in `flow.rs`. The CLI and MCP use the same parsers and renderers.

| Item | Limit or behavior |
|---|---|
| Input file | Regular files up to 256 MiB. Format identified by signature and internal structure |
| ZIP | Up to 32,768 entries, 512 MiB each for the declared total uncompressed size and for cumulative reads, up to 64 MiB per entry read |
| XML and RTF | XML tree depth and RTF group depth of 128, 1.5 million nodes per XML part. No external entity resolution and no macro execution |
| Tables | A 250,000-cell threshold applies to XML tables or spreadsheet expansion. Repeated data has separate byte limits |
| PDF | Stream and page decoding limits, Form nesting and cycle checks, total content budget |
| MCP | Request frames of 1 MiB. The default access scope is the configured root and can be widened at server start with `--allow-outside-root` |

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

There are 8 direct dependencies. Versions are pinned in `Cargo.lock`.

| Crate | Purpose |
|---|---|
| zip | Reading the ZIP central directory and compressed entries |
| flate2 | Rust-backed DEFLATE, HWP streams, and decompression of embedded notices |
| roxmltree | XML structure and namespace traversal |
| cfb | Traversing sectors, directories, and streams of OLE compound files |
| encoding_rs | RTF code pages and character encodings |
| lopdf | PDF objects, streams, resources, and font character mapping |
| serde | Serialization of the shared document model |
| serde_json | JSON output and MCP messages |

The project source is under the [MIT license](LICENSE). For the 76 external packages (including build tools) in the Windows target Cargo metadata, the declared licenses and notice files were verified. None of these packages require GPL or AGPL. The original texts, including BSD and Unicode data notices, are in [third-party notices](THIRD-PARTY-NOTICES.txt). The distributed EXE prints the project license, Cargo dependency notices, and Rust standard library notices with `--licenses`. After updating versions, regenerate the notices with `python tools/notices.py`.

This product was developed with reference to Hancom's published documentation for the HWP document file format (.hwp).