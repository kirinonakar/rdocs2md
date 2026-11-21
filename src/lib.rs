pub mod batch;
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
pub mod parallel;
mod pdf;
pub mod render;
mod rtf;
pub mod search;
mod sheets;
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
    load_range(path, images, None)
}

pub fn load_range(path: &Path, images: bool, range: Option<&str>) -> Result<Document> {
    let range = range.map(SectionRange::parse).transpose()?;
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m = f.metadata()?;
    if !m.is_file() || m.len() > FILE_LIMIT as u64 {
        return Err("input must be a regular file of at most 2 GiB".into());
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(m.len() as usize)?;
    (&mut f).take(FILE_LIMIT as u64).read_to_end(&mut bytes)?;
    if bytes.len() == FILE_LIMIT && f.read(&mut [0])? != 0 {
        return Err("file limit exceeded".into());
    }
    convert_selected(&bytes, images, range.as_ref())
}
pub fn convert(b: &[u8], images: bool) -> Result<Document> {
    convert_range(b, images, None)
}

pub fn convert_range(b: &[u8], images: bool, range: Option<&str>) -> Result<Document> {
    let range = range.map(SectionRange::parse).transpose()?;
    convert_selected(b, images, range.as_ref())
}

fn convert_selected(b: &[u8], images: bool, range: Option<&SectionRange>) -> Result<Document> {
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
        hwp3::parse(b)?
    } else if b.starts_with(b"PK\x03\x04") {
        office::package(b, images, range)?
    } else if b.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        legacy::parse(b, images, range)?
    } else {
        return Err("unrecognized document signature".into());
    };
    if d.sections.is_empty() {
        return Err(format!("{} contains no supported document sections", d.format).into());
    }
    visibility::annotate(&mut d);
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
