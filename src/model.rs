use serde::Serialize;
use std::collections::BTreeMap;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub type Properties = BTreeMap<String, String>;
pub const MIB: usize = 1024 * 1024;
pub const FILE_LIMIT: usize = 2 * 1024 * MIB;
pub(crate) const EXPANSION_LIMIT: usize = 256 * MIB;
pub const PART_LIMIT: usize = 64 * MIB;
pub const CELL_LIMIT: usize = 250_000;

#[derive(Default)]
pub(crate) struct SectionRange(pub Vec<(usize, usize)>);

impl SectionRange {
    pub fn parse(range: &str) -> Result<Self> {
        let mut spans = Vec::new();
        for part in range.split(',') {
            let part = part.trim();
            let (start, end) = part.split_once('-').unwrap_or((part, part));
            let (start, end) = (start.parse::<usize>()?, end.parse::<usize>()?);
            if start == 0 || start > end {
                return Err(format!("invalid section range {part}").into());
            }
            spans.push((start, end));
        }
        Ok(Self::from_spans(spans))
    }

    pub fn from_spans(mut spans: Vec<(usize, usize)>) -> Self {
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in spans {
            if let Some(last) = merged.last_mut()
                && start <= last.1.saturating_add(1)
            {
                last.1 = last.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        Self(merged)
    }

    pub fn contains(&self, index: usize) -> bool {
        self.0
            .get(self.0.partition_point(|&(_, end)| end < index))
            .is_some_and(|&(start, _)| start <= index)
    }

    pub fn ends_at(&self, index: usize) -> bool {
        self.0.last().is_some_and(|&(_, end)| end == index)
    }

    pub fn validate(&self, count: usize) -> Result<()> {
        if let Some(&(_, end)) = self.0.last().filter(|&&(_, end)| end > count) {
            return Err(format!(
                "invalid section range ending at {end} (document has {count} sections)"
            )
            .into());
        }
        Ok(())
    }
}

pub(crate) fn section_indices(
    range: Option<&SectionRange>,
    count: usize,
) -> Result<impl Iterator<Item = usize>> {
    if let Some(range) = range {
        range.validate(count)?;
    }
    Ok((0..count).filter(move |&i| range.is_none_or(|r| r.contains(i + 1))))
}

#[derive(Default, Serialize)]
pub struct Document {
    pub format: String,
    pub metadata: Properties,
    pub sections: Vec<Section>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub assets: BTreeMap<String, Vec<u8>>,
}
impl Document {
    pub fn new(format: &str) -> Self {
        Self {
            format: format.into(),
            ..Self::default()
        }
    }
    pub fn warn(&mut self, msg: &str) {
        if self.warnings.len() < 64 && !self.warnings.iter().any(|w| w == msg) {
            self.warnings.push(msg.into());
        }
    }
    /// Split only at recorded page breaks. Flow documents otherwise retain their logical sections.
    pub fn explicit_pages(&mut self) {
        let mut pages = Vec::new();
        for section in std::mem::take(&mut self.sections) {
            section.append_explicit_pages(&mut pages);
        }
        self.sections = pages;
    }
    pub fn select(&mut self, range: &str) -> Result<()> {
        self.retain_sections(Some(&SectionRange::parse(range)?))
    }

    /// Keep only the selected workbook sheets. Titles match ASCII
    /// case-insensitively, because workbook titles are unique that way, and take
    /// precedence; any other selector is read as a 1-based workbook sheet
    /// number, so `1` is the first sheet. The stored 1-based `index` stays valid
    /// for the sheets that remain.
    pub fn select_sheets(&mut self, selectors: &[String]) -> Result<()> {
        if selectors.is_empty() {
            return Ok(());
        }
        if !matches!(self.format.as_str(), "xls" | "xlsx" | "ods") {
            return Err(format!(
                "sheet selection supports xls, xlsx and ods workbooks, not {}",
                self.format
            )
            .into());
        }
        let titles: Vec<&str> = self.sections.iter().map(|s| s.title.as_str()).collect();
        let mut keep = vec![false; titles.len()];
        for selector in selectors {
            let selector = selector.trim();
            if selector.is_empty() {
                return Err("sheet selector must not be empty".into());
            }
            if let Some(pos) = titles.iter().position(|t| t.eq_ignore_ascii_case(selector)) {
                keep[pos] = true;
                continue;
            }
            if let Ok(number) = selector.parse::<usize>() {
                if !(1..=titles.len()).contains(&number) {
                    return Err(
                        format!("sheet number {number} is outside 1-{}", titles.len()).into(),
                    );
                }
                keep[number - 1] = true;
                continue;
            }
            let mut available = titles.clone();
            let more = available.len() > 20;
            available.truncate(20);
            return Err(format!(
                "sheet {selector:?} not found (sheets: {}{})",
                available.join(", "),
                if more { ", ..." } else { "" }
            )
            .into());
        }
        let mut pos = 0;
        self.sections.retain(|_| {
            let selected = keep[pos];
            pos += 1;
            selected
        });
        Ok(())
    }

    pub(crate) fn retain_sections(&mut self, range: Option<&SectionRange>) -> Result<()> {
        let Some(range) = range else { return Ok(()) };
        range.validate(self.sections.len())?;
        let mut slide_number = 0;
        let mut index = 0;
        self.sections.retain_mut(|section| {
            index += 1;
            section.index.get_or_insert(index);
            if section.kind == "slide" {
                slide_number += 1;
                section.slide_number.get_or_insert(slide_number);
            }
            range.contains(index)
        });
        Ok(())
    }
}
#[derive(Default, Serialize)]
pub struct Section {
    pub kind: String,
    pub source: String,
    /// 1-based position among the source sections, kept when a range selects a
    /// subset so that citations retain the original numbering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(skip)]
    pub slide_number: Option<usize>,
    #[serde(skip)]
    #[doc(hidden)]
    pub pdf_warnings: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: Properties,
    pub blocks: Vec<Block>,
    #[serde(skip)]
    #[doc(hidden)]
    pub hwp3_layout: Option<Hwp3Layout>,
    #[serde(skip)]
    #[doc(hidden)]
    pub document_paper: Option<DocumentPaper>,
    #[serde(skip)]
    #[doc(hidden)]
    pub hwp5_layout: Option<Box<Hwp5Layout>>,
    #[serde(skip)]
    #[doc(hidden)]
    pub slide_layout: Option<SlideLayout>,
}

#[derive(Default)]
pub struct Hwp5Layout {
    pub(crate) paragraph_css: BTreeMap<usize, std::sync::Arc<str>>,
    pub(crate) page_starts: std::collections::BTreeSet<usize>,
}

pub struct DocumentPaper {
    pub(crate) size: [u32; 2],
    // Top, right, bottom, left, in hundredths of a point.
    pub(crate) padding: [u32; 4],
}

impl DocumentPaper {
    pub(crate) fn new(size: [u32; 2], padding: [u32; 4]) -> Option<Self> {
        let [top, right, bottom, left] = padding;
        (size.iter().all(|n| (1_000..=1_000_000).contains(n))
            && left.saturating_add(right) < size[0]
            && top.saturating_add(bottom) < size[1])
            .then_some(Self { size, padding })
    }
}

pub struct SlideLayout {
    pub(crate) size: [f64; 2],
    pub(crate) background: Option<String>,
    pub(crate) frames: Vec<SlideFrame>,
}

pub(crate) struct SlideFrame {
    pub transform: [f64; 6],
    pub size: [f64; 2],
    pub blocks: std::ops::Range<usize>,
    pub shape: Option<SlideShape>,
    pub text: SlideText,
}

pub(crate) struct SlideShape {
    pub kind: String,
    pub fill: Option<String>,
    pub stroke: Option<(String, f64)>,
}

#[derive(Clone)]
pub(crate) struct SlideText {
    pub size: f64,
    pub padding: [f64; 4],
    pub vertical: &'static str,
}

pub struct Hwp3Layout {
    pub(crate) paper: [i32; 2],
    pub(crate) pages: Vec<Hwp3Page>,
}

pub(crate) struct Hwp3Page {
    pub number: usize,
    pub first_block: usize,
    pub graphics: Vec<Hwp3Graphic>,
    pub lines: Vec<Hwp3Line>,
    pub replaced_blocks: Vec<std::ops::Range<usize>>,
}

pub(crate) struct Hwp3Graphic {
    pub asset: String,
    pub rect: [i32; 4],
    pub order: i32,
}

pub(crate) struct Hwp3Line {
    pub x: i32,
    pub y: i32,
    pub size: u16,
    pub height: u16,
    pub word_spacing: i16,
    pub parts: Vec<Hwp3Text>,
}

pub(crate) enum Hwp3Text {
    Text(Box<Run>),
    Tab(u16),
}
impl Section {
    pub fn new(kind: &str, source: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            source: source.into(),
            ..Self::default()
        }
    }
    pub(crate) fn append_explicit_pages(mut self, pages: &mut Vec<Section>) {
        let blocks = std::mem::take(&mut self.blocks);
        for b in blocks {
            let Block::Paragraph {
                heading,
                list,
                runs,
                properties,
            } = b
            else {
                self.blocks.push(b);
                continue;
            };
            if properties.get("break-before").is_some_and(|v| v == "page")
                && !self.blocks.is_empty()
            {
                let next = Section::new("page", &self.source);
                self.kind = "page".into();
                pages.push(std::mem::replace(&mut self, next));
            }
            if !runs.iter().any(|r| r.page_break) {
                self.blocks.push(Block::Paragraph {
                    heading,
                    list,
                    runs,
                    properties,
                });
                continue;
            }
            let mut segment = Vec::new();
            for r in runs {
                if r.page_break {
                    if !segment.is_empty() {
                        self.blocks.push(Block::Paragraph {
                            heading,
                            list: list.clone(),
                            runs: std::mem::take(&mut segment),
                            properties: properties.clone(),
                        })
                    }
                    let next = Section::new("page", &self.source);
                    self.kind = "page".into();
                    pages.push(std::mem::replace(&mut self, next));
                } else {
                    segment.push(r)
                }
            }
            if !segment.is_empty() {
                self.blocks.push(Block::Paragraph {
                    heading,
                    list,
                    runs: segment,
                    properties,
                })
            }
        }
        pages.push(self);
    }
    pub(crate) fn indexed(mut self, index: usize) -> Self {
        self.index = Some(index + 1);
        if self.kind == "slide" {
            self.slide_number = self.index;
        }
        self
    }
    pub fn paragraph(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.blocks.push(Block::text(text));
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Paragraph {
        #[serde(skip_serializing_if = "is_zero")]
        heading: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        list: Option<List>,
        runs: Vec<Run>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        properties: Properties,
    },
    Table {
        rows: Vec<Vec<Cell>>,
    },
}
fn is_zero(n: &u8) -> bool {
    *n == 0
}
impl Block {
    pub fn flow(runs: Vec<Run>) -> Self {
        Self::Paragraph {
            heading: 0,
            list: None,
            runs,
            properties: Properties::new(),
        }
    }
    pub fn text(t: impl Into<String>) -> Self {
        Self::flow(vec![Run::text(t)])
    }

    pub(crate) fn page_break() -> Self {
        Self::flow(vec![Run {
            page_break: true,
            ..Run::default()
        }])
    }

    pub(crate) fn is_page_break(&self) -> bool {
        matches!(self, Self::Paragraph { runs, .. }
            if matches!(runs.as_slice(), [run] if run.page_break && run.text.is_empty()))
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct List {
    pub ordered: bool,
    pub depth: usize,
}
#[derive(Debug, Default, Clone, Serialize)]
pub struct Cell {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Block>,
    pub runs: Vec<Run>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colspan: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rowspan: Option<usize>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: Properties,
}
impl Cell {
    pub fn text(t: impl Into<String>) -> Self {
        Self {
            runs: vec![Run::text(t)],
            ..Self::default()
        }
    }
}
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Style {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}
#[derive(Debug, Default, Clone, Serialize)]
pub struct Run {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub visibility_flags: Vec<VisibilityFlag>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub page_break: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mathml: Option<String>,
    /// LaTeX for a formula, either supplied by the parser or reconstructed
    /// from page geometry. Rendered as `$...$` in Markdown and `\(...\)` in HTML.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latex: Option<String>,
    pub text: String,
    #[serde(skip_serializing_if = "is_plain")]
    pub style: Style,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_layout: Option<ImageLayout>,
    #[serde(skip)]
    #[doc(hidden)]
    pub image_geometry: Option<ImageGeometry>,
    #[serde(skip)]
    #[doc(hidden)]
    pub image_alt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityFlag {
    WhiteText,
    TinyText,
    OffPage,
    HiddenText,
}

impl VisibilityFlag {
    pub(crate) const ALL: [Self; 4] = [
        Self::WhiteText,
        Self::TinyText,
        Self::OffPage,
        Self::HiddenText,
    ];
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageLayout {
    // Dimensions are in hundredths of a point.
    pub width: u32,
    pub height: u32,
    pub rotation_deg: u16,
}

#[derive(Debug, Default, Clone)]
pub struct ImageGeometry {
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub rotation: f64,
    pub flip_x: bool,
    pub flip_y: bool,
    // Fractions removed at the left, top, right and bottom edges.
    pub crop: [f64; 4],
}
fn is_plain(s: &Style) -> bool {
    *s == Style::default()
}
impl Run {
    pub fn text(t: impl Into<String>) -> Self {
        Self {
            text: t.into(),
            ..Self::default()
        }
    }
}
pub fn push_run(out: &mut Vec<Run>, run: Run) {
    if !run.page_break && run.image.is_none() && run.mathml.is_none() {
        if run.text.is_empty() {
            return;
        }
        if let Some(prev) = out.last_mut()
            && !prev.page_break
            && prev.image.is_none()
            && prev.mathml.is_none()
            && prev.style == run.style
            && prev.link == run.link
            && prev.visibility_flags == run.visibility_flags
        {
            prev.text.push_str(&run.text);
            return;
        }
    }
    out.push(run);
}
pub fn image_ext(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if b.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if b.starts_with(b"GIF89a") || b.starts_with(b"GIF87a") {
        Some("gif")
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        Some("webp")
    } else if b.starts_with(b"BM") {
        Some("bmp")
    } else if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        Some("tif")
    } else {
        None
    }
}
pub fn image_sources(d: &Document) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    fn visit(blocks: &[Block], names: &mut std::collections::BTreeSet<String>) {
        for b in blocks {
            match b {
                Block::Paragraph { runs, .. } => {
                    names.extend(runs.iter().filter_map(|r| r.image.clone()))
                }
                Block::Table { rows } => {
                    for c in rows.iter().flatten() {
                        names.extend(c.runs.iter().filter_map(|r| r.image.clone()));
                        visit(&c.blocks, names)
                    }
                }
            }
        }
    }
    for s in &d.sections {
        visit(&s.blocks, &mut names)
    }
    names
}
