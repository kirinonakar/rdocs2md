use crate::{
    model::*,
    structure::{self, HeadingEvidence, Headings},
};
use flate2::read::DeflateDecoder;
use std::{borrow::Cow, io::Read};

mod drawing;
mod picture;
pub(crate) use drawing::SVG_START as GENERATED_SVG_START;

const SIGNATURE_2: &[u8; 30] = b"HWP Document File V2.00 \x1a\x01\x02\x03\x04\x05";
const SIGNATURE_3: &[u8; 30] = b"HWP Document File V3.00 \x1a\x01\x02\x03\x04\x05";
const DOCUMENT_INFO_SIZE: usize = 128;
const DOCUMENT_SUMMARY_SIZE: usize = 1008;
const FONT_NAME_SIZE: usize = 40;
const STYLE_NAME_SIZE: usize = 20;
const PARAGRAPH_SHAPE_SIZE: usize = 187;
const MAX_NESTING: usize = 32;
const NODE_LIMIT: usize = 1_500_000;

pub fn matches(data: &[u8]) -> bool {
    font_groups(data).is_some()
}

fn font_groups(data: &[u8]) -> Option<usize> {
    if data.starts_with(SIGNATURE_2) {
        Some(5)
    } else if data.starts_with(SIGNATURE_3) {
        Some(7)
    } else {
        None
    }
}

pub fn parse(data: &[u8], images: bool) -> Result<Document> {
    let font_groups = font_groups(data).ok_or("expected HWP 2.00 or 3.00 signature")?;
    // Font, width and spacing each use one byte per language group.
    let char_shape_size = 10 + 3 * font_groups;
    let style_size = STYLE_NAME_SIZE + char_shape_size + PARAGRAPH_SHAPE_SIZE;
    let signature_size = SIGNATURE_3.len();
    let fixed_end = signature_size + DOCUMENT_INFO_SIZE + DOCUMENT_SUMMARY_SIZE;
    let info = data
        .get(signature_size..signature_size + DOCUMENT_INFO_SIZE)
        .ok_or("truncated HWP document info")?;
    if read_u16_at(info, 96)? != 0 {
        return Err("password-protected HWP is unsupported".into());
    }
    let info_blocks_size = usize::from(read_u16_at(info, 126)?);
    let body_start = fixed_end
        .checked_add(info_blocks_size)
        .ok_or("HWP offset overflow")?;
    if body_start > data.len() {
        return Err("truncated HWP info blocks".into());
    }

    let body = if info[124] != 0 {
        let mut decoded = Vec::new();
        DeflateDecoder::new(&data[body_start..])
            .take(EXPANSION_LIMIT as u64 + 1)
            .read_to_end(&mut decoded)?;
        if decoded.len() > EXPANSION_LIMIT {
            return Err("HWP expansion limit exceeded".into());
        }
        Cow::Owned(decoded)
    } else {
        Cow::Borrowed(&data[body_start..])
    };

    let mut cursor = Cursor::new(&body);
    for _ in 0..font_groups {
        let count = usize::from(cursor.u16()?);
        cursor.skip(
            count
                .checked_mul(FONT_NAME_SIZE)
                .ok_or("HWP font table overflow")?,
        )?;
    }
    let style_count = usize::from(cursor.u16()?);
    cursor.skip(
        style_count
            .checked_mul(style_size)
            .ok_or("HWP style table overflow")?,
    )?;

    let mut doc = Document::new("hwp");
    if let Some(summary) = data.get(signature_size + DOCUMENT_INFO_SIZE..fixed_end)
        && let Some(title_bytes) = summary.get(..112)
    {
        let title = decode_hchar_array(title_bytes);
        if !title.trim().is_empty() {
            doc.metadata.insert("title".into(), title.trim().into());
        }
    }
    let paper = Paper::read(info);
    let mut section = Section::new("section", "document");
    section.document_paper = paper.as_ref().and_then(|paper| paper.document_paper(info));
    let mut state = ParseState {
        char_shape_size,
        version3: font_groups == 7,
        images,
        paper: paper.filter(|_| images),
        ..ParseState::default()
    };
    section.blocks = paragraph_list(&mut cursor, 0, &mut state)?;
    state.read_embedded_pictures(&mut cursor)?;
    classify_paragraphs(&mut section.blocks);
    section.hwp3_layout = state.layout(section.blocks.len());
    if section.hwp3_layout.is_none() {
        join_list_continuations(&mut section.blocks);
    }
    if state.partial_graphics {
        doc.warn(
            "Some HWP 3.0 graphics could only be restored partially or could not be rendered.",
        );
    }
    doc.assets = state.assets;
    doc.sections.push(section);
    Ok(doc)
}

struct Paper {
    size: [i32; 2],
    left: i32,
    top: i32,
}

impl Paper {
    fn read(info: &[u8]) -> Option<Self> {
        let get = |offset| read_u16_at(info, offset).ok().map(i32::from);
        let mut size = [get(8)?, get(6)?];
        if info[5] != 0 {
            size.swap(0, 1);
        }
        let left = get(14)?;
        let top = get(10)? + get(18)?;
        (size[0] > left && size[1] > top).then_some(Self { size, left, top })
    }

    fn document_paper(&self, info: &[u8]) -> Option<DocumentPaper> {
        let right = i32::from(read_u16_at(info, 16).ok()?);
        let bottom =
            i32::from(read_u16_at(info, 12).ok()?) + i32::from(read_u16_at(info, 20).ok()?);
        // HWP 2/3 uses 1/1800 inch, or four hundredths of a point.
        let to_hundredths = |n: i32| n as u32 * 4;
        DocumentPaper::new(
            self.size.map(to_hundredths),
            [self.top, right, bottom, self.left].map(to_hundredths),
        )
    }
}

#[derive(Default)]
struct ParseState {
    char_shape_size: usize,
    version3: bool,
    nodes: usize,
    images: bool,
    asset_bytes: usize,
    assets: std::collections::BTreeMap<String, Vec<u8>>,
    embedded: std::collections::BTreeMap<Vec<u8>, String>,
    partial_graphics: bool,
    paper: Option<Paper>,
    page: usize,
    break_after: bool,
    previous_line_y: Option<u16>,
    layout_bytes: usize,
    paragraphs: Vec<PositionedParagraph>,
    graphics: std::collections::BTreeMap<usize, Vec<Hwp3Graphic>>,
}

struct PositionedParagraph {
    page: usize,
    blocks: std::ops::Range<usize>,
    lines: Vec<Hwp3Line>,
    replaces: bool,
}

impl ParseState {
    fn layout(&mut self, blocks: usize) -> Option<Hwp3Layout> {
        let paper = self.paper.as_ref()?.size;
        self.graphics.retain(|_, graphics| {
            graphics
                .iter()
                .any(|graphic| self.assets.contains_key(&graphic.asset))
        });
        if self.graphics.is_empty() {
            return None;
        }
        let mut pages: std::collections::BTreeMap<_, _> = std::mem::take(&mut self.graphics)
            .into_iter()
            .map(|(number, mut graphics)| {
                graphics.sort_by_key(|graphic| graphic.order);
                (
                    number,
                    Hwp3Page {
                        number,
                        first_block: blocks,
                        graphics,
                        lines: Vec::new(),
                        replaced_blocks: Vec::new(),
                    },
                )
            })
            .collect();
        for paragraph in std::mem::take(&mut self.paragraphs) {
            if let Some(page) = pages.get_mut(&paragraph.page) {
                page.first_block = page.first_block.min(paragraph.blocks.start);
                if paragraph.replaces {
                    page.lines.extend(paragraph.lines);
                    page.replaced_blocks.push(paragraph.blocks);
                }
            }
        }
        let mut pages: Vec<_> = pages.into_values().collect();
        pages.sort_by_key(|page| (page.first_block, page.number));
        Some(Hwp3Layout { paper, pages })
    }

    fn drawing(&mut self, data: &[u8]) -> Option<(String, [i32; 4], i32)> {
        let drawing = drawing::parse(data)?;
        let key = format!("hwp3/drawing/{}", self.assets.len() + 1);
        self.insert_asset(key.clone(), drawing.svg)?;
        self.partial_graphics |= drawing.partial;
        Some((key, drawing.bounds, drawing.order))
    }

    fn insert_asset(&mut self, key: String, bytes: Vec<u8>) -> Option<()> {
        let total = self.asset_bytes.checked_add(bytes.len())?;
        if total > PART_LIMIT || self.assets.len() >= 16_384 {
            return None;
        }
        self.asset_bytes = total;
        self.assets.insert(key, bytes);
        Some(())
    }

    fn embedded_picture(&mut self, header: &[u8]) -> Option<String> {
        let name = picture::name(&header[83..99]);
        if let Some(key) = self.embedded.get(name) {
            return Some(key.clone());
        }
        if name.is_empty() || self.embedded.len() >= 16_384 {
            return None;
        }
        let key = format!("hwp3/picture/{}", self.embedded.len() + 1);
        self.embedded.insert(name.to_vec(), key.clone());
        Some(key)
    }

    fn read_embedded_pictures(&mut self, cursor: &mut Cursor<'_>) -> Result<()> {
        if self.embedded.is_empty() {
            return Ok(());
        }
        // The end of the main paragraph list is followed by length-prefixed
        // info blocks. Pictures are looked up only within this document.
        while let Ok(kind) = cursor.u32() {
            if kind == 0 {
                break;
            }
            crate::limits::check_request()?;
            let Ok(bytes) = cursor.u32().and_then(|len| cursor.take(len as usize)) else {
                break;
            };
            if kind != 1 || bytes.len() <= 32 {
                continue;
            }
            let Some(key) = self.embedded.get(picture::name(&bytes[..16])) else {
                continue;
            };
            let data = &bytes[32..];
            if self.assets.contains_key(key) || data.len() > PART_LIMIT - self.asset_bytes {
                continue;
            }
            let raster = if image_ext(data).is_some() {
                Some(data.to_vec())
            } else {
                picture::wmf_raster(data)
            };
            if let Some(raster) = raster {
                self.insert_asset(key.clone(), raster);
            }
        }
        self.partial_graphics |= self
            .embedded
            .values()
            .any(|key| !self.assets.contains_key(key));
        Ok(())
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(len).ok_or("HWP offset overflow")?;
        let value = self.data.get(self.pos..end).ok_or("truncated HWP body")?;
        self.pos = end;
        Ok(value)
    }

    fn skip(&mut self, len: usize) -> Result<()> {
        self.take(len).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
}

fn read_u16_at(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        data.get(offset..offset + 2)
            .ok_or("truncated HWP field")?
            .try_into()?,
    ))
}

fn paragraph_list<'a>(
    cursor: &mut Cursor<'a>,
    depth: usize,
    state: &mut ParseState,
) -> Result<Vec<Block>> {
    if depth > MAX_NESTING {
        return Err("HWP object nesting limit exceeded".into());
    }
    let mut blocks = Vec::new();
    let mut shape = ParagraphShape { data: None };
    loop {
        state.nodes += 1;
        if state.nodes > NODE_LIMIT {
            return Err("HWP paragraph limit exceeded".into());
        }
        let Some(mut paragraph) = parse_paragraph(cursor, depth, state, blocks.len(), &mut shape)?
        else {
            return Ok(blocks);
        };
        blocks.append(&mut paragraph);
    }
}

fn parse_paragraph<'a>(
    cursor: &mut Cursor<'a>,
    depth: usize,
    state: &mut ParseState,
    block_start: usize,
    shape: &mut ParagraphShape<'a>,
) -> Result<Option<Vec<Block>>> {
    let previous_shape = cursor.u8()?;
    let char_count = usize::from(cursor.u16()?);
    let line_count = usize::from(cursor.u16()?);
    let has_char_shapes = cursor.u8()? != 0;
    let flags = cursor.u8()?;
    cursor.skip(4 + 1)?;
    let default_shape = cursor.take(state.char_shape_size)?;
    if char_count == 0 {
        return Ok(None);
    }
    let positioned = if depth == 0 && state.paper.is_some() {
        state.layout_bytes = state
            .layout_bytes
            .saturating_add(line_count * 128 + char_count * 12);
        state.layout_bytes <= PART_LIMIT
    } else {
        false
    };
    if previous_shape == 0 {
        shape.data = Some(cursor.take(PARAGRAPH_SHAPE_SIZE)?);
    }
    let line_info = cursor.take(
        line_count
            .checked_mul(14)
            .ok_or("HWP line table overflow")?,
    )?;
    let line_info = line_info.as_chunks::<14>().0;
    let starts: Vec<_> = line_info
        .iter()
        .map(|line| usize::from(u16::from_le_bytes([line[0], line[1]])))
        .collect();
    let mut page_breaks = Vec::new();
    if depth == 0 {
        let first_y = line_info
            .first()
            .map(|line| u16::from_le_bytes([line[6], line[7]]));
        // HWP 3 can store a page break on the final paragraph of the old page.
        // A cached Y that is still on that page distinguishes it from the
        // boundary on the first line of the new page.
        let explicit_break = flags & 2 != 0;
        let break_after = state.version3
            && explicit_break
            && flags & 4 == 0
            && first_y.is_some_and(|y| y > 0 && state.previous_line_y.is_none_or(|prev| y >= prev));
        if state.break_after || explicit_break && !break_after {
            page_breaks.push(0);
        }
        state.break_after = break_after;
        if let Some(line) = line_info.last() {
            state.previous_line_y = Some(u16::from_le_bytes([line[6], line[7]]));
        }
        page_breaks.extend(line_info.iter().zip(&starts).filter_map(|(line, &start)| {
            let flags = u16::from_le_bytes([line[12], line[13]]);
            (flags & 0x8001 == 0x8001 && start < char_count).then_some(start)
        }));
        page_breaks.sort_unstable();
        page_breaks.dedup();
    }
    let paragraph_page = state.page + usize::from(page_breaks.first() == Some(&0));
    state.page += page_breaks.len();
    let mut page_breaks = page_breaks.into_iter().peekable();
    let mut changes = Vec::new();
    if has_char_shapes {
        for index in 0..char_count {
            if cursor.u8()? != 1 {
                changes.push((index, cursor.take(state.char_shape_size)?));
            }
        }
    }
    let mut changes = changes.into_iter().peekable();
    let initial_shape = changes
        .next_if(|(index, _)| *index == 0)
        .map_or(default_shape, |(_, s)| s);
    let default_size = read_u16_at(initial_shape, 0)?;
    let mut paragraph = ParagraphText {
        style: char_style(initial_shape),
        properties: shape.properties(default_size),
        ..ParagraphText::default()
    };
    let mut lines = Vec::new();
    if positioned {
        let paper = state.paper.as_ref().unwrap();
        for (i, line) in line_info.iter().enumerate() {
            let size = if (25..=10000).contains(&default_size) {
                default_size
            } else {
                250
            };
            let source_x = i32::from(read_u16_at(line, 8)? as i16);
            let x = if source_x != 0 {
                source_x
            } else {
                shape.left() + if i == 0 { shape.indent() } else { 0 }
            };
            lines.push(Hwp3Line {
                x: paper.left + x,
                y: paper.top + i32::from(read_u16_at(line, 6)?),
                size,
                height: read_u16_at(line, 4)?.max(size),
                word_spacing: read_u16_at(line, 2)? as i16,
                parts: Vec::new(),
            });
        }
    }
    let mut replaces = positioned
        && starts.first() == Some(&0)
        && paragraph_page == state.page
        && starts.windows(2).all(|pair| pair[0] <= pair[1])
        && lines.iter().all(|line| {
            state
                .paper
                .as_ref()
                .is_some_and(|paper| line.y < paper.size[1])
        });
    let mut out = Vec::new();
    let mut graphics = Vec::new();
    let mut line = 0;
    let mut consumed = 0usize;
    let mut spaces_end = 0;
    let mut wrap_padding_end = 0;
    while consumed < char_count {
        while page_breaks.next_if(|&start| start <= consumed).is_some() {
            paragraph.flush(&mut out);
            if block_start != 0 || !out.is_empty() {
                out.push(Block::page_break());
            }
        }
        let mut next_shape = None;
        while let Some((_, raw)) = changes.next_if(|(index, _)| *index <= consumed) {
            next_shape = Some(raw);
        }
        if let Some(raw) = next_shape {
            let style = char_style(raw);
            if style != paragraph.style {
                paragraph.flush_run();
                paragraph.style = style;
            }
        }
        while starts.get(line + 1).is_some_and(|&start| start <= consumed) {
            line += 1;
        }
        let text_start = paragraph.text.len();
        let code = cursor.u16()?;
        consumed += 1;
        match code {
            0..=5 | 12 | 27 | 29 => {
                skip_reserved(cursor, code)?;
                consumed += 3;
            }
            6 => {
                cursor.skip(40)?;
                consumed += 3;
            }
            7 => {
                cursor.skip(82)?;
                consumed += 41;
            }
            8 => {
                cursor.skip(94)?;
                consumed += 47;
            }
            9 => {
                let width = cursor.u16()?;
                cursor.skip(4)?;
                consumed += 3;
                paragraph.text.push('\t');
                if replaces {
                    lines[line].parts.push(Hwp3Text::Tab(width));
                }
            }
            10 => {
                replaces = false;
                paragraph.flush(&mut out);
                cursor.skip(6)?;
                consumed += 3;
                parse_box(cursor, depth + 1, state, &mut out)?;
            }
            11 => {
                cursor.skip(6)?;
                consumed += 3;
                let header = cursor.take(348)?;
                let size = u32::from_le_bytes(header[..4].try_into()?) as usize;
                let extra = cursor.take(size)?;
                let box_size = [
                    i32::from(read_u16_at(header, 42)?),
                    i32::from(read_u16_at(header, 44)?),
                ];
                let graphic = if state.images {
                    match header[74] {
                        2 => state.embedded_picture(header).map(|key| {
                            let [width, height] = box_size;
                            let order =
                                i32::MIN + i32::from(u16::from_le_bytes([header[72], header[73]]));
                            (key, [0, 0, width, height], order)
                        }),
                        3 => state.drawing(extra),
                        _ => None,
                    }
                } else {
                    None
                };
                let captions = paragraph_list(cursor, depth + 1, state)?;
                if let Some((key, [dx, dy, width, height], order)) = graphic {
                    if replaces
                        && captions.is_empty()
                        && header[8] <= 3
                        && width > 0
                        && height > 0
                        && usize::from(read_u16_at(header, 66)?) == paragraph_page
                    {
                        let paper = state.paper.as_ref().unwrap();
                        let x = paper.left
                            + i32::from(read_u16_at(header, 62)? as i16)
                            + i32::from(read_u16_at(header, 18)? as i16)
                            + i32::from(read_u16_at(header, 26)? as i16)
                            + dx;
                        let y = paper.top
                            + i32::from(read_u16_at(header, 64)? as i16)
                            + i32::from(read_u16_at(header, 22)? as i16)
                            + i32::from(read_u16_at(header, 30)? as i16)
                            + dy;
                        if x < paper.size[0] && y < paper.size[1] && x + width > 0 && y + height > 0
                        {
                            graphics.push(Hwp3Graphic {
                                asset: key.clone(),
                                rect: [x, y, width, height],
                                order,
                            });
                            if header[8] == 0 {
                                lines[line]
                                    .parts
                                    .push(Hwp3Text::Tab(read_u16_at(header, 52)?));
                            }
                        } else {
                            replaces = false;
                        }
                    } else {
                        replaces = false;
                    }
                    paragraph.flush_run();
                    paragraph.runs.push(Run {
                        text: "[그림]".into(),
                        image: Some(key),
                        image_alt: Some(String::new()),
                        image_geometry: Some(ImageGeometry {
                            width: (width > 0).then(|| f64::from(width) / 25.),
                            height: (height > 0).then(|| f64::from(height) / 25.),
                            ..ImageGeometry::default()
                        }),
                        ..Run::default()
                    });
                } else {
                    replaces = false;
                    state.partial_graphics |= state.images;
                    paragraph.text.push_str("[그림]");
                }
                if !captions.is_empty() {
                    paragraph.flush(&mut out);
                    out.extend(captions);
                }
            }
            13 => {}
            14 => {
                cursor.skip(90)?;
                consumed += 3;
            }
            15 => {
                replaces = false;
                cursor.skip(14)?;
                consumed += 3;
                append_nested(cursor, depth, state, &mut out, &mut paragraph)?;
            }
            16 => {
                replaces = false;
                cursor.skip(16)?;
                consumed += 3;
                append_nested(cursor, depth, state, &mut out, &mut paragraph)?;
            }
            17 => {
                replaces = false;
                cursor.skip(20)?;
                consumed += 3;
                append_nested(cursor, depth, state, &mut out, &mut paragraph)?;
            }
            18..=21 => {
                cursor.skip(6)?;
                consumed += 3;
            }
            22 => {
                cursor.skip(22)?;
                consumed += 11;
            }
            23 => {
                cursor.skip(8)?;
                consumed += 4;
            }
            24 => {
                cursor.skip(4)?;
                consumed += 2;
                paragraph.text.push('-');
            }
            25 => {
                cursor.skip(4)?;
                consumed += 2;
            }
            26 => {
                cursor.skip(244)?;
                consumed += 122;
            }
            28 => {
                cursor.skip(62)?;
                consumed += 31;
            }
            30 => {
                cursor.skip(2)?;
                consumed += 1;
                paragraph.text.push('\u{00a0}');
            }
            31 => {
                cursor.skip(2)?;
                consumed += 1;
                paragraph.text.push(' ');
            }
            32 => {
                if consumed > spaces_end {
                    spaces_end = consumed
                        + cursor.data[cursor.pos..]
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .take(char_count - consumed)
                            .take_while(|&&unit| unit == [32, 0])
                            .count();
                    // Padded continuations used literal spaces to lay out both
                    // ends of a line. Keep the source break instead of moving
                    // that padding into the middle of a word or guessing a separator.
                    if starts
                        .get(line + 1)
                        .is_some_and(|&start| consumed < start && start + 2 <= spaces_end)
                    {
                        wrap_padding_end = spaces_end;
                        if page_breaks.peek() != starts.get(line + 1) {
                            paragraph.text.push('\n');
                        }
                    }
                }
                if consumed > wrap_padding_end {
                    paragraph.text.push(' ');
                }
            }
            _ => paragraph.text.push(hnc_char(code).unwrap_or('\u{fffd}')),
        }
        if consumed > char_count {
            return Err("invalid HWP control character length".into());
        }
        if replaces && code != 9 && (code == 32 || paragraph.text.len() > text_start) {
            let value = if code == 32 {
                " "
            } else {
                &paragraph.text[text_start..]
            };
            if let Some(Hwp3Text::Text(last)) = lines[line].parts.last_mut()
                && last.style == paragraph.style
            {
                last.text.push_str(value);
            } else {
                lines[line].parts.push(Hwp3Text::Text(Box::new(Run {
                    text: value.into(),
                    style: paragraph.style.clone(),
                    ..Run::default()
                })));
            }
        }
    }
    paragraph.flush(&mut out);
    if depth == 0
        && line_count > 0
        && char_count == 1
        && out.last().is_none_or(Block::is_page_break)
    {
        // A paragraph containing only its terminator can still occupy a source
        // line. An empty fragment after a table or picture does not.
        out.push(Block::Paragraph {
            heading: 0,
            list: None,
            runs: Vec::new(),
            properties: paragraph.properties,
        });
    }
    if positioned {
        if replaces && !graphics.is_empty() {
            state
                .graphics
                .entry(paragraph_page)
                .or_default()
                .extend(graphics);
        }
        state.paragraphs.push(PositionedParagraph {
            page: paragraph_page,
            blocks: block_start..block_start + out.len(),
            lines,
            replaces,
        });
    }
    Ok(Some(out))
}

fn skip_reserved(cursor: &mut Cursor<'_>, code: u16) -> Result<()> {
    let size = usize::try_from(cursor.u32()?)?;
    if cursor.u16()? != code {
        return Err("invalid HWP control character marker".into());
    }
    cursor.skip(size)
}

fn append_nested(
    cursor: &mut Cursor<'_>,
    depth: usize,
    state: &mut ParseState,
    out: &mut Vec<Block>,
    paragraph: &mut ParagraphText,
) -> Result<()> {
    let nested = paragraph_list(cursor, depth + 1, state)?;
    if !nested.is_empty() {
        paragraph.flush(out);
        out.extend(nested);
    }
    Ok(())
}

fn parse_box(
    cursor: &mut Cursor<'_>,
    depth: usize,
    state: &mut ParseState,
    out: &mut Vec<Block>,
) -> Result<()> {
    if depth > MAX_NESTING {
        return Err("HWP object nesting limit exceeded".into());
    }
    let info = cursor.take(84)?;
    let kind = info[78];
    let cell_count = usize::from(read_u16_at(info, 80)?);
    if cell_count > CELL_LIMIT {
        return Err("HWP table cell limit exceeded".into());
    }
    let mut cells = Vec::with_capacity(cell_count);
    for _ in 0..cell_count {
        cells.push(cursor.take(27)?);
    }
    let mut contents = Vec::with_capacity(cell_count);
    for _ in 0..cell_count {
        contents.push(paragraph_list(cursor, depth + 1, state)?);
    }
    let captions = paragraph_list(cursor, depth + 1, state)?;

    if kind == 0 && cell_count > 0 {
        let table = table_from_cells(&cells, contents)?;
        out.push(table);
    } else {
        for blocks in contents {
            out.extend(blocks);
        }
    }
    out.extend(captions);
    Ok(())
}

fn table_from_cells(metadata: &[&[u8]], contents: Vec<Vec<Block>>) -> Result<Block> {
    if let Some((columns, rows, locations)) = table_grid(metadata) {
        let mut grid = vec![vec![None; columns]; rows];
        for (((x, y, w, h), blocks), record) in locations.into_iter().zip(contents).zip(metadata) {
            let cols = w.end - w.start;
            let rows = h.end - h.start;
            let mut cell = Cell {
                blocks,
                colspan: (cols > 1).then_some(cols),
                rowspan: (rows > 1).then_some(rows),
                ..Cell::default()
            };
            let mut points = |key, offset| {
                crate::formatting::points(
                    &mut cell.properties,
                    key,
                    read_u16_at(record, offset).ok().map(|v| f64::from(v) / 25.),
                );
            };
            points("width", 8);
            points("height", 10);
            for (side, &kind) in crate::formatting::BORDERS.into_iter().zip(&record[20..24]) {
                let border = match kind {
                    0 => "none",
                    1 => "0.02mm solid #000000",
                    2 => "0.35mm solid #000000",
                    3 => "0.02mm dotted #000000",
                    4 => "0.39mm double #000000",
                    _ => continue,
                };
                cell.properties.insert(side.into(), border.into());
            }
            for row in &mut grid[h] {
                for slot in &mut row[w.clone()] {
                    *slot = Some(Cell::default());
                }
            }
            grid[y][x] = Some(cell);
        }
        // Covered cells are accounted for by spans. A gap in the geometric
        // grid still needs an empty HTML cell to preserve following columns.
        let rows = grid
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .filter_map(|value| match value {
                        None => Some(Cell::default()),
                        Some(cell)
                            if cell.blocks.is_empty()
                                && cell.properties.is_empty()
                                && cell.colspan.is_none()
                                && cell.rowspan.is_none() =>
                        {
                            None
                        }
                        Some(cell) => Some(cell),
                    })
                    .collect()
            })
            .collect();
        return Ok(Block::Table { rows });
    }
    let positions: Vec<_> = metadata
        .iter()
        .map(|m| (usize::from(m[0]), usize::from(m[1])))
        .collect();
    let mut seen = std::collections::HashSet::with_capacity(positions.len());
    let mut duplicate = false;
    for position in &positions {
        if !seen.insert(*position) {
            duplicate = true;
        }
    }
    let max_row = positions.iter().map(|(row, _)| *row).max().unwrap_or(0);
    let max_col = positions.iter().map(|(_, col)| *col).max().unwrap_or(0);
    let row_count = max_row.saturating_add(1);
    let col_count = max_col.saturating_add(1);
    let grid_size = row_count.saturating_mul(col_count);
    let sequential = duplicate || grid_size > CELL_LIMIT || grid_size < contents.len();
    let (rows, columns) = if sequential {
        (1, contents.len())
    } else {
        (row_count, col_count)
    };
    if rows.saturating_mul(columns) > CELL_LIMIT {
        return Err("HWP table cell limit exceeded".into());
    }
    let mut grid = (0..rows * columns)
        .map(|_| Cell::default())
        .collect::<Vec<_>>();
    for (index, blocks) in contents.into_iter().enumerate() {
        let slot = if sequential {
            index
        } else {
            positions[index].0 * columns + positions[index].1
        };
        grid[slot].blocks = blocks;
    }
    let rows = grid
        .chunks(columns)
        .map(|row| row.to_vec())
        .collect::<Vec<_>>();
    Ok(Block::Table { rows })
}

type CellLocation = (usize, usize, std::ops::Range<usize>, std::ops::Range<usize>);

fn table_grid(metadata: &[&[u8]]) -> Option<(usize, usize, Vec<CellLocation>)> {
    let mut x_edges = Vec::new();
    let mut y_edges = Vec::new();
    let mut rects = Vec::new();
    for record in metadata {
        let get = |offset| read_u16_at(record, offset).ok().map(i32::from);
        let (x, y, w, h) = (get(4)?, get(6)?, get(8)?, get(10)?);
        if w == 0 || h == 0 {
            return None;
        }
        x_edges.extend([x, x + w]);
        y_edges.extend([y, y + h]);
        rects.push((x, y, x + w, y + h));
    }
    for edges in [&mut x_edges, &mut y_edges] {
        edges.sort_unstable();
        edges.dedup();
    }
    let (columns, rows) = (x_edges.len() - 1, y_edges.len() - 1);
    if columns.saturating_mul(rows) > CELL_LIMIT {
        return None;
    }
    let mut covered = vec![false; columns * rows];
    let mut locations = Vec::new();
    for (left, top, right, bottom) in rects {
        let x = x_edges.binary_search(&left).ok()?;
        let y = y_edges.binary_search(&top).ok()?;
        let end_x = x_edges.binary_search(&right).ok()?;
        let end_y = y_edges.binary_search(&bottom).ok()?;
        for row in y..end_y {
            for slot in &mut covered[row * columns + x..row * columns + end_x] {
                if std::mem::replace(slot, true) {
                    return None;
                }
            }
        }
        locations.push((x, y, x..end_x, y..end_y));
    }
    Some((columns, rows, locations))
}

struct ParagraphShape<'a> {
    data: Option<&'a [u8]>,
}

impl ParagraphShape<'_> {
    fn value(&self, offset: usize) -> u16 {
        self.data.map_or(0, |data| {
            u16::from_le_bytes([data[offset], data[offset + 1]])
        })
    }

    fn left(&self) -> i32 {
        i32::from(self.value(0))
    }

    fn indent(&self) -> i32 {
        i32::from(self.value(4) as i16)
    }

    fn properties(&self, size: u16) -> Properties {
        let mut props = Properties::new();
        for (key, offset) in [("margin-top", 183), ("margin-bottom", 8)] {
            crate::formatting::points(&mut props, key, Some(f64::from(self.value(offset)) / 25.));
        }
        if let Some(size) = font_size(size) {
            props.insert("font-size".into(), size);
        }
        let leading = self.value(6);
        if leading > 0 {
            let height = if leading & 0x8000 != 0 {
                format!("{}pt", f64::from(leading & 0x7fff) / 25.)
            } else {
                format!("{leading}%")
            };
            props.insert("line-height".into(), height);
        }
        props
    }
}

#[derive(Default)]
struct ParagraphText {
    runs: Vec<Run>,
    text: String,
    style: Style,
    properties: Properties,
}

impl ParagraphText {
    fn flush_run(&mut self) {
        if !self.text.is_empty() {
            self.runs.push(Run {
                text: std::mem::take(&mut self.text),
                style: self.style.clone(),
                ..Run::default()
            });
        }
    }

    fn flush(&mut self, out: &mut Vec<Block>) {
        self.flush_run();
        if !self.runs.is_empty() {
            out.push(Block::Paragraph {
                heading: 0,
                list: None,
                runs: std::mem::take(&mut self.runs),
                properties: self.properties.clone(),
            });
        }
    }
}

fn font_size(size: u16) -> Option<String> {
    (25..=10000)
        .contains(&size)
        .then(|| format!("{}pt", f64::from(size) / 25.))
}

fn char_style(data: &[u8]) -> Style {
    let attributes = data.len() - 8;
    let flags = data[attributes + 3];
    let palette = [
        "#0000ff", "#00ff00", "#00ffff", "#ff0000", "#ff00ff", "#ffff00", "#ffffff",
    ];
    Style {
        italic: flags & 1 != 0,
        bold: flags & 2 != 0,
        underline: flags & 4 != 0,
        script: match flags & 0x60 {
            0x20 => Some("sup".into()),
            0x40 => Some("sub".into()),
            _ => None,
        },
        color: data[attributes + 1]
            .checked_sub(1)
            .and_then(|index| palette.get(usize::from(index)))
            .map(|&color| color.into()),
        size: font_size(u16::from_le_bytes([data[0], data[1]])),
        ..Style::default()
    }
}

const BULLET_GROUPS: [(char, char); 7] = [
    ('□', '■'),
    ('○', '●'),
    ('◇', '◆'),
    ('△', '▲'),
    ('▽', '▼'),
    ('◁', '◀'),
    ('▷', '▶'),
];

fn classify_paragraphs(blocks: &mut [Block]) {
    for block in blocks.iter_mut() {
        if let Block::Paragraph {
            runs,
            list,
            properties,
            ..
        } = block
            && let Some((marker, prefix)) = structure::bullet_prefix(runs)
            && structure::is_bullet(marker)
        {
            structure::remove_prefix(runs, prefix);
            *list = Some(List {
                ordered: false,
                depth: 0,
            });
            properties.insert("list-marker".into(), marker.to_string());
        }
    }
    if let Some(headings) = Headings::infer(blocks.iter(), HeadingEvidence::Hwp3) {
        for block in blocks {
            if let Block::Paragraph {
                heading,
                runs,
                list: None,
                ..
            } = block
            {
                *heading = headings.level(runs).unwrap_or(0);
            }
        }
    }
}

fn join_list_continuations(blocks: &mut Vec<Block>) {
    if !blocks
        .iter()
        .any(|block| matches!(block, Block::Paragraph { list: Some(_), .. }))
    {
        return;
    }
    let mut joined = Vec::with_capacity(blocks.len());
    for mut block in std::mem::take(blocks) {
        if let (
            Some(Block::Paragraph {
                list: Some(_),
                runs: previous,
                ..
            }),
            Block::Paragraph {
                heading: 0,
                list: None,
                runs,
                ..
            },
        ) = (joined.last_mut(), &mut block)
            && let Some(last) = previous.last_mut()
            && !last
                .text
                .trim_end()
                .ends_with(['.', '!', '?', '。', '！', '？'])
            && let Some(first) = runs.first_mut()
            && first.text.starts_with("    ")
            && !first.text.trim().is_empty()
        {
            first
                .text
                .drain(..first.text.len() - first.text.trim_start_matches(' ').len());
            last.text.truncate(last.text.trim_end_matches(' ').len());
            previous.push(Run::text("\n"));
            previous.append(runs);
        } else {
            joined.push(block);
        }
    }
    *blocks = joined;
}

fn decode_hchar_array(data: &[u8]) -> String {
    let mut text = String::new();
    for pair in data.as_chunks::<2>().0 {
        let code = u16::from_le_bytes(*pair);
        if code == 0 {
            break;
        }
        if let Some(ch) = hnc_char(code) {
            text.push(ch);
        }
    }
    text
}

fn hnc_char(code: u16) -> Option<char> {
    if code <= 0x7e || (0xa0..=0xff).contains(&code) {
        return char::from_u32(u32::from(code));
    }
    match code {
        0x007f => Some('¡'),
        0x0080 => Some('€'),
        0x0081 => Some('“'),
        0x0082 => Some('”'),
        0x0083 => Some('‘'),
        0x0084 => Some('’'),
        0x0085 => Some('·'),
        0x0086 => Some('„'),
        0x0087 => Some('‟'),
        0x0088..=0x00a0 => Some('\u{3000}'),
        // HNC kana use the hiragana and katakana rows of KS X 1001.
        0x1f01..=0x1f53 => euc_kr_char([0xaa, (code - 0x1f00 + 0xa0) as u8]),
        0x1f61..=0x1fb6 => euc_kr_char([0xab, (code - 0x1f60 + 0xa0) as u8]),
        0x1f00..=0x1fff => None,
        0x2050 => Some('✡'),
        // These symbols use Hancom's supplementary private-use glyphs.
        0x25c0 => Some('\u{f0088}'),
        0x25c8 => Some('\u{f008e}'),
        0x25d2 => Some('☼'),
        0x25d3 => Some('\u{f0091}'),
        0x2f08 => Some('▪'),
        0x2f11 | 0x2f14 => Some('◦'),
        0x2f17 => Some('•'),
        0x2f67 => Some('▸'),
        0x2f00..=0x2f6f if code & 0xf < 9 => {
            let (open, filled) = BULLET_GROUPS[usize::from((code - 0x2f00) >> 4)];
            Some(if code & 0xf < 6 { open } else { filled })
        }
        0x3400 => Some('\u{3000}'),
        0x3404 => Some('․'),
        0x3401..=0x37bf => {
            // HNC symbols are arranged in 96-column KS X 1001 rows,
            // with the kana rows skipped in the last group.
            let offset = code - 0x3400;
            let row = offset / 96 + if offset / 96 == 9 { 0xa3 } else { 0xa1 };
            euc_kr_char([row as u8, (offset % 96 + 0xa0) as u8])
        }
        0x4000..=0x5317 => {
            // HNC level-1 Hanja flatten the 52 KS X 1001 rows of 94 characters.
            let offset = code - 0x4000;
            euc_kr_char([(0xca + offset / 94) as u8, (0xa1 + offset % 94) as u8])
        }
        _ if code & 0x8000 != 0 => kssm_char(code),
        _ => char::from_u32(u32::from(code)).filter(|ch| !ch.is_control()),
    }
}

fn euc_kr_char(bytes: [u8; 2]) -> Option<char> {
    encoding_rs::EUC_KR
        .decode_without_bom_handling_and_without_replacement(&bytes)?
        .chars()
        .next()
}

fn kssm_char(code: u16) -> Option<char> {
    const INITIAL_JAMO: [char; 19] = [
        'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ',
        'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
    ];
    const FINAL_JAMO: [char; 27] = [
        'ㄱ', 'ㄲ', 'ㄳ', 'ㄴ', 'ㄵ', 'ㄶ', 'ㄷ', 'ㄹ', 'ㄺ', 'ㄻ', 'ㄼ', 'ㄽ', 'ㄾ', 'ㄿ', 'ㅀ',
        'ㅁ', 'ㅂ', 'ㅄ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
    ];

    let initial = ((code >> 10) & 0x1f) as usize;
    let medial = ((code >> 5) & 0x1f) as usize;
    let final_code = (code & 0x1f) as usize;
    let final_index = match final_code {
        1 => 0,
        2..=17 => final_code - 1,
        19..=29 => final_code - 2,
        _ => return None,
    };
    // The empty medial field marks standalone consonants.
    if medial == 2 {
        return match (initial, final_index) {
            (2..=20, 0) => Some(INITIAL_JAMO[initial - 2]),
            (1, 1..=27) => Some(FINAL_JAMO[final_index - 1]),
            _ => None,
        };
    }

    let medial_index = match medial {
        3..=7 => medial - 3,
        10..=15 => medial - 5,
        18..=23 => medial - 7,
        26..=29 => medial - 9,
        _ => return None,
    };
    if initial == 1 && final_index == 0 {
        return char::from_u32('ㅏ' as u32 + medial_index as u32);
    }

    let initial_index = initial.checked_sub(2).filter(|&n| n < 19)?;
    let offset = (initial_index * 21 + medial_index) * 28 + final_index;
    char::from_u32(0xac00 + offset as u32)
}

#[cfg(test)]
mod tests {
    use super::hnc_char;

    #[test]
    fn decodes_kssm_standalone_modern_jamo() {
        for (code, expected) in [
            (0x8461, 'ㅏ'),
            (0x85c1, 'ㅘ'),
            (0x8741, 'ㅠ'),
            (0x87a1, 'ㅣ'),
            (0x8841, 'ㄱ'),
            (0xac41, 'ㅅ'),
            (0xd041, 'ㅎ'),
            (0x8444, 'ㄳ'),
            (0x8454, 'ㅄ'),
            (0x845d, 'ㅎ'),
            (0x8861, '가'),
            (0x8862, '각'),
        ] {
            assert_eq!(hnc_char(code), Some(expected), "KSSM {code:#06x}");
        }
        for code in [0x8501, 0x8742, 0x8842, 0x8452] {
            assert_eq!(hnc_char(code), None, "KSSM {code:#06x}");
        }
    }

    #[test]
    fn decodes_hnc_hanja_as_ks_x_1001_characters() {
        for (code, expected) in [
            (0x4000, '伽'),
            (0x405d, '龕'),
            (0x405e, '匣'),
            (0x420f, '軍'),
            (0x47c4, '殺'),
            (0x4ad5, '領'),
            (0x4cac, '人'),
            (0x4cf3, '者'),
            (0x5317, '詰'),
        ] {
            assert_eq!(hnc_char(code), Some(expected), "HNC {code:#06x}");
        }
    }
}
