mod epub;
mod flow;
mod formatting;
mod hwp3;
mod latex;
mod legacy;
mod math;
pub mod mcp;
pub mod model;
mod office;
pub mod output;
mod package;
mod pdf;
pub mod render;
mod rtf;
mod sheets;
mod xls;
use model::*;
use std::{fs::File, io::Read, path::Path};
pub fn load(path: &Path, images: bool) -> Result<Document> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m = f.metadata()?;
    if !m.is_file() || m.len() > FILE_LIMIT as u64 {
        return Err("input must be a regular file of at most 256 MiB".into());
    }
    let mut bytes = Vec::new();
    f.take(FILE_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > FILE_LIMIT {
        return Err("file limit exceeded".into());
    }
    convert(&bytes, images)
}
pub fn convert(b: &[u8], images: bool) -> Result<Document> {
    if b.len() > FILE_LIMIT {
        return Err("document input exceeds 256 MiB".into());
    }
    let d = if b.starts_with(b"%PDF-") {
        pdf::parse(b, images)?
    } else if b.starts_with(b"{\\rtf") {
        rtf::parse(b, images)?
    } else if hwp3::matches(b) {
        hwp3::parse(b)?
    } else if b.starts_with(b"PK\x03\x04") {
        office::package(b, images)?
    } else if b.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        legacy::parse(b, images)?
    } else {
        return Err("unrecognized document signature".into());
    };
    if d.sections.is_empty() {
        return Err(format!("{} contains no supported document sections", d.format).into());
    }
    Ok(d)
}
pub const FORMATS: &[(&str, &str, &str)] = &[
    (
        "pdf",
        "Pages, font mappings, text origins, Form XObjects, simple columns and grids, embedded JPEG",
        "No OCR, full vector math or irregular table recognition",
    ),
    (
        "hwp",
        "HWP 3.x raw DEFLATE, KSSM text and controls; HWP 5 BodyText, Unicode paragraphs, tables and embedded raster pictures",
        "HWP 3.x image extraction and detailed layout/style restoration are partial; HWP 5 distribution-protected files and general vector drawings are unsupported",
    ),
    (
        "hwpx",
        "Spine sections, paragraphs, character properties, tables, image references",
        "Advanced equations, page masters and nested layout are partial",
    ),
    (
        "doc",
        "Word 97–2003 FIB, CLX pieces, main story, field results and explicit breaks",
        "Table structure, style runs, floating objects and ancillary stories are partial",
    ),
    (
        "docx",
        "Sections, headings, styled runs, numbering, links, tables, notes, embedded images",
        "Floating layout, vertical merges, headers and equations are partial",
    ),
    (
        "ppt",
        "Active edit chain, persisted slides, outline and shape text",
        "No full graphical layout, master styles, pictures or notes",
    ),
    (
        "pptx",
        "Presentation order, title placeholders, text boxes, tables, links, images, notes",
        "Group transforms, charts, master inheritance and animations are partial",
    ),
    (
        "xls",
        "BIFF8 sheets, continued shared strings, numeric and cached formula cells",
        "No older BIFF, expression reconstruction, drawing or full display formatting",
    ),
    (
        "xlsx",
        "Workbook order, sparse cells, shared strings, formulas with cached values, merge spans",
        "No calculation engine, charts or complete numeric display formatting",
    ),
    (
        "odt",
        "Headings, inherited text styles, lists, links, tables, images and note text",
        "Page masters, complex tables and MathML typography are partial",
    ),
    (
        "ods",
        "Sheets, repeated rows and cells, spans, displayed text, typed values and formulas",
        "Repeat budgets apply, hidden layout and custom formats are partial",
    ),
    (
        "odp",
        "Slides, coordinate order, title frames, geometry, tables, images and notes",
        "No animations, rotated group transforms or full master styling",
    ),
    (
        "rtf",
        "Nested groups, code pages, Unicode, inline styles, fields, tables, raster pictures",
        "List definitions, color and font rendering and nested tables are partial",
    ),
    (
        "epub",
        "Spine order, NCX/nav metadata, XHTML, basic CSS, ruby, writing direction and images",
        "No DRM, browser layout, complex CSS or graphical SVG rendering",
    ),
];
pub fn support() -> serde_json::Value {
    serde_json::Value::Array(FORMATS.iter().map(|(f,s,l)|serde_json::json!({"format":f,"status":"partial","features":s,"limitations":l})).collect())
}
