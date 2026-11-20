use serde::Serialize;
use std::collections::BTreeMap;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub type Properties = BTreeMap<String, String>;
pub const MIB: usize = 1024 * 1024;
pub const FILE_LIMIT: usize = 256 * MIB;
pub const PART_LIMIT: usize = 64 * MIB;
pub const CELL_LIMIT: usize = 250_000;

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
        for mut s in std::mem::take(&mut self.sections) {
            let blocks = std::mem::take(&mut s.blocks);
            for b in blocks {
                let Block::Paragraph {
                    heading,
                    list,
                    runs,
                    properties,
                } = b
                else {
                    s.blocks.push(b);
                    continue;
                };
                if properties.get("break-before").is_some_and(|v| v == "page")
                    && !s.blocks.is_empty()
                {
                    let next = Section::new("page", &s.source);
                    s.kind = "page".into();
                    pages.push(std::mem::replace(&mut s, next));
                }
                if !runs.iter().any(|r| r.page_break) {
                    s.blocks.push(Block::Paragraph {
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
                            s.blocks.push(Block::Paragraph {
                                heading,
                                list: list.clone(),
                                runs: std::mem::take(&mut segment),
                                properties: properties.clone(),
                            })
                        }
                        let next = Section::new("page", &s.source);
                        s.kind = "page".into();
                        pages.push(std::mem::replace(&mut s, next));
                    } else {
                        segment.push(r)
                    }
                }
                if !segment.is_empty() {
                    s.blocks.push(Block::Paragraph {
                        heading,
                        list,
                        runs: segment,
                        properties,
                    })
                }
            }
            pages.push(s)
        }
        self.sections = pages;
    }
    pub fn select(&mut self, range: &str) -> Result<()> {
        let mut slide_number = 0;
        for section in &mut self.sections {
            if section.kind == "slide" {
                slide_number += 1;
                section.slide_number.get_or_insert(slide_number);
            }
        }
        let mut keep = vec![false; self.sections.len()];
        for part in range.split(',') {
            let (a, b) = part
                .trim()
                .split_once('-')
                .unwrap_or((part.trim(), part.trim()));
            let (a, b) = (a.parse::<usize>()?, b.parse::<usize>()?);
            if a == 0 || a > b || b > keep.len() {
                return Err(format!(
                    "invalid section range {part} (document has {} sections)",
                    keep.len()
                )
                .into());
            }
            keep[a - 1..b].fill(true);
        }
        let mut flags = keep.into_iter();
        self.sections.retain(|_| flags.next().unwrap_or(false));
        Ok(())
    }
}
#[derive(Default, Serialize)]
pub struct Section {
    pub kind: String,
    pub source: String,
    #[serde(skip)]
    pub slide_number: Option<usize>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: Properties,
    pub blocks: Vec<Block>,
}
impl Section {
    pub fn new(kind: &str, source: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            source: source.into(),
            ..Self::default()
        }
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
    pub color: Option<String>,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageLayout {
    // Dimensions are in hundredths of a point.
    pub width: u32,
    pub height: u32,
    pub rotation_deg: u16,
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
