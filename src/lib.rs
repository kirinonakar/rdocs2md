pub mod batch;
mod epub;
mod flow;
mod formatting;
mod hwp3;
mod latex;
mod legacy;
pub mod limits;
mod math;
pub mod mcp;
pub mod model;
mod office;
pub mod output;
mod package;
pub mod parallel;
mod pdf;
pub mod render;
mod rtf;
pub mod search;
mod sheets;
mod structure;
mod visibility;
mod xls;
use model::*;
use std::{fs::File, io::Read, path::Path};

pub fn recover_document<T>(
    action: impl FnOnce() -> Result<T> + std::panic::UnwindSafe,
) -> Result<T> {
    std::panic::catch_unwind(action).unwrap_or_else(|_| Err("document processing panicked".into()))
}
pub fn load(path: &Path, images: bool) -> Result<Document> {
    load_sheets(path, images, None, &[])
}

pub fn load_range(path: &Path, images: bool, range: Option<&str>) -> Result<Document> {
    load_sheets(path, images, range, &[])
}

/// Load a document and keep only the named workbook sheets, if any.
pub fn load_sheets(
    path: &Path,
    images: bool,
    range: Option<&str>,
    sheets: &[String],
) -> Result<Document> {
    limits::check_request()?;
    let range = range.map(SectionRange::parse).transpose()?;
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m = f.metadata()?;
    if !m.is_file() || m.len() > FILE_LIMIT as u64 {
        return Err("input must be a regular file of at most 2 GiB".into());
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(m.len() as usize)?;
    (&mut f).take(FILE_LIMIT as u64).read_to_end(&mut bytes)?;
    limits::check_request()?;
    if bytes.len() == FILE_LIMIT && f.read(&mut [0])? != 0 {
        return Err("file limit exceeded".into());
    }
    convert_selected(&bytes, images, range.as_ref(), sheets)
}
pub fn convert(b: &[u8], images: bool) -> Result<Document> {
    convert_sheets(b, images, None, &[])
}

pub fn convert_range(b: &[u8], images: bool, range: Option<&str>) -> Result<Document> {
    convert_sheets(b, images, range, &[])
}

/// Convert in-memory bytes and keep only the named workbook sheets, if any.
pub fn convert_sheets(
    b: &[u8],
    images: bool,
    range: Option<&str>,
    sheets: &[String],
) -> Result<Document> {
    let range = range.map(SectionRange::parse).transpose()?;
    convert_selected(b, images, range.as_ref(), sheets)
}

fn convert_selected(
    b: &[u8],
    images: bool,
    range: Option<&SectionRange>,
    sheets: &[String],
) -> Result<Document> {
    if b.len() > FILE_LIMIT {
        return Err("document input exceeds 2 GiB".into());
    }
    let mut d = if b.starts_with(b"%PDF-") {
        pdf::parse(b, images, range)?
    } else if b.starts_with(b"{\\rtf") {
        rtf::parse(b, images, range)?
    } else if hwp3::matches(b) {
        if let Some(range) = range {
            range.validate(1)?;
        }
        hwp3::parse(b, images)?
    } else if b.starts_with(b"PK\x03\x04") {
        office::package(b, images, range)?
    } else if b.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        legacy::parse(b, images, range)?
    } else {
        return Err("unrecognized document signature".into());
    };
    limits::check_request()?;
    d.select_sheets(sheets)?;
    if d.sections.is_empty() {
        return Err(format!("{} contains no supported document sections", d.format).into());
    }
    visibility::annotate(&mut d);
    Ok(d)
}
pub const FORMATS: &[(&str, &str, &str)] = &[
    (
        "pdf",
        "Pages, font mappings, text origins, Form XObjects, simple columns and grids, outline titles as headings, JPEG and raster images as PNG",
        "No OCR, full vector math or irregular table recognition",
    ),
    (
        "hwp",
        "HWP 3.x KSSM text, tables, vector drawings as SVG, and positioned HTML pages; HWP 5 BodyText, Unicode paragraphs, tables and embedded raster pictures",
        "HWP 3.x legacy picture encodings, advanced drawing effects and typography are partial; HWP 5 distribution-protected files and general vector drawings are unsupported",
    ),
    (
        "hwpx",
        "Spine sections, paragraphs, character properties, tables, image references",
        "Advanced equations, page masters and nested layout are partial",
    ),
    (
        "doc",
        "Word 97–2003 FIB, CLX pieces, main story, field results, explicit breaks and table rows with merged cells, column widths and borders",
        "Character and paragraph formatting, floating objects and ancillary stories are partial",
    ),
    (
        "docx",
        "Sections, headings, styled runs, numbering, links, tables, notes, comments, embedded images",
        "Tracked deletions are omitted with a warning; floating layout, vertical merges, headers and equations are partial",
    ),
    (
        "ppt",
        "Active edit chain, persisted slides, outline and shape text",
        "No full graphical layout, master styles, pictures or notes",
    ),
    (
        "pptx",
        "Presentation order, text boxes, tables, links, images, notes, and HTML slide geometry with group transforms and basic preset shapes",
        "Charts, SmartArt, custom shapes, master styling and animations are partial",
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
