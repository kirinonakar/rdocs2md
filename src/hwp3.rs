use crate::model::*;
use flate2::read::DeflateDecoder;
use std::{borrow::Cow, io::Read};

mod drawing;
pub(crate) use drawing::SVG_START as GENERATED_SVG_START;

const SIGNATURE: &[u8; 30] = b"HWP Document File V3.00 \x1a\x01\x02\x03\x04\x05";
const DOCUMENT_INFO_SIZE: usize = 128;
const DOCUMENT_SUMMARY_SIZE: usize = 1008;
const FONT_GROUPS: usize = 7;
const FONT_NAME_SIZE: usize = 40;
const STYLE_SIZE: usize = 238;
const MAX_NESTING: usize = 32;
const NODE_LIMIT: usize = 1_500_000;

pub fn matches(data: &[u8]) -> bool {
    data.starts_with(SIGNATURE)
}

pub fn parse(data: &[u8], images: bool) -> Result<Document> {
    if !matches(data) {
        return Err("expected HWP 3.0 signature".into());
    }
    let fixed_end = SIGNATURE.len() + DOCUMENT_INFO_SIZE + DOCUMENT_SUMMARY_SIZE;
    let info = data
        .get(SIGNATURE.len()..SIGNATURE.len() + DOCUMENT_INFO_SIZE)
        .ok_or("truncated HWP 3.0 document info")?;
    if read_u16_at(info, 96)? != 0 {
        return Err("password-protected HWP 3.0 is unsupported".into());
    }
    let info_blocks_size = usize::from(read_u16_at(info, 126)?);
    let body_start = fixed_end
        .checked_add(info_blocks_size)
        .ok_or("HWP 3.0 offset overflow")?;
    if body_start > data.len() {
        return Err("truncated HWP 3.0 info blocks".into());
    }

    let body = if info[124] != 0 {
        let mut decoded = Vec::new();
        DeflateDecoder::new(&data[body_start..])
            .take(EXPANSION_LIMIT as u64 + 1)
            .read_to_end(&mut decoded)?;
        if decoded.len() > EXPANSION_LIMIT {
            return Err("HWP 3.0 expansion limit exceeded".into());
        }
        Cow::Owned(decoded)
    } else {
        Cow::Borrowed(&data[body_start..])
    };

    let mut cursor = Cursor::new(&body);
    for _ in 0..FONT_GROUPS {
        let count = usize::from(cursor.u16()?);
        cursor.skip(
            count
                .checked_mul(FONT_NAME_SIZE)
                .ok_or("HWP 3.0 font table overflow")?,
        )?;
    }
    let style_count = usize::from(cursor.u16()?);
    cursor.skip(
        style_count
            .checked_mul(STYLE_SIZE)
            .ok_or("HWP 3.0 style table overflow")?,
    )?;

    let mut doc = Document::new("hwp");
    if let Some(summary) = data.get(SIGNATURE.len() + DOCUMENT_INFO_SIZE..fixed_end)
        && let Some(title_bytes) = summary.get(..112)
    {
        let title = decode_hchar_array(title_bytes);
        if !title.trim().is_empty() {
            doc.metadata.insert("title".into(), title.trim().into());
        }
    }
    let mut state = ParseState {
        images,
        paper: if images { Paper::read(info) } else { None },
        ..ParseState::default()
    };
    let mut section = Section::new("section", "document");
    section.blocks = paragraph_list(&mut cursor, 0, &mut state)?;
    section.hwp3_layout = state.layout(section.blocks.len());
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
}

#[derive(Default)]
struct ParseState {
    nodes: usize,
    images: bool,
    asset_bytes: usize,
    assets: std::collections::BTreeMap<String, Vec<u8>>,
    partial_graphics: bool,
    paper: Option<Paper>,
    page: usize,
    paragraph_indent: i32,
    first_indent: i32,
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
        if self.asset_bytes.saturating_add(drawing.svg.len()) > PART_LIMIT
            || self.assets.len() >= 16_384
        {
            return None;
        }
        self.partial_graphics |= drawing.partial;
        self.asset_bytes += drawing.svg.len();
        let key = format!("hwp3/drawing/{}", self.assets.len() + 1);
        self.assets.insert(key.clone(), drawing.svg);
        Some((key, drawing.bounds, drawing.order))
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
        let end = self.pos.checked_add(len).ok_or("HWP 3.0 offset overflow")?;
        let value = self
            .data
            .get(self.pos..end)
            .ok_or("truncated HWP 3.0 body")?;
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
            .ok_or("truncated HWP 3.0 field")?
            .try_into()?,
    ))
}

fn paragraph_list(
    cursor: &mut Cursor<'_>,
    depth: usize,
    state: &mut ParseState,
) -> Result<Vec<Block>> {
    if depth > MAX_NESTING {
        return Err("HWP 3.0 object nesting limit exceeded".into());
    }
    let mut blocks = Vec::new();
    loop {
        state.nodes += 1;
        if state.nodes > NODE_LIMIT {
            return Err("HWP 3.0 paragraph limit exceeded".into());
        }
        let Some(mut paragraph) = parse_paragraph(cursor, depth, state, blocks.len())? else {
            return Ok(blocks);
        };
        blocks.append(&mut paragraph);
    }
}

fn parse_paragraph(
    cursor: &mut Cursor<'_>,
    depth: usize,
    state: &mut ParseState,
    block_start: usize,
) -> Result<Option<Vec<Block>>> {
    let previous_shape = cursor.u8()?;
    let char_count = usize::from(cursor.u16()?);
    let line_count = usize::from(cursor.u16()?);
    let has_char_shapes = cursor.u8()? != 0;
    cursor.skip(1 + 4 + 1)?;
    let default_size = read_u16_at(cursor.take(31)?, 0)?;
    let positioned = if depth == 0 && state.paper.is_some() {
        state.layout_bytes = state
            .layout_bytes
            .saturating_add(line_count * 128 + char_count * 12);
        state.layout_bytes <= PART_LIMIT
    } else {
        false
    };
    if previous_shape == 0 && char_count > 0 {
        let shape = cursor.take(187)?;
        if positioned {
            state.paragraph_indent = i32::from(read_u16_at(shape, 0)?);
            state.first_indent = i32::from(read_u16_at(shape, 4)? as i16);
        }
    }
    if char_count == 0 {
        return Ok(None);
    }
    let line_info = cursor.take(
        line_count
            .checked_mul(14)
            .ok_or("HWP 3.0 line table overflow")?,
    )?;
    let (mut lines, mut starts) = (Vec::new(), Vec::new());
    let mut paragraph_page = state.page;
    if positioned {
        let paper = state.paper.as_ref().unwrap();
        for (i, line) in line_info.as_chunks::<14>().0.iter().enumerate() {
            if read_u16_at(line, 12)? & 0x8001 == 0x8001 {
                state.page += 1;
            }
            if i == 0 {
                paragraph_page = state.page;
            }
            starts.push(usize::from(read_u16_at(line, 0)?));
            lines.push(Hwp3Line {
                x: paper.left
                    + state.paragraph_indent
                    + if i == 0 { state.first_indent } else { 0 },
                y: paper.top + i32::from(read_u16_at(line, 6)?),
                size: if (25..=10000).contains(&default_size) {
                    default_size
                } else {
                    250
                },
                parts: Vec::new(),
            });
        }
    }
    let mut replaces = starts.first() == Some(&0)
        && paragraph_page == state.page
        && starts.windows(2).all(|pair| pair[0] <= pair[1])
        && lines.iter().all(|line| {
            state
                .paper
                .as_ref()
                .is_some_and(|paper| line.y < paper.size[1])
        });
    if has_char_shapes {
        for _ in 0..char_count {
            if cursor.u8()? != 1 {
                cursor.skip(31)?;
            }
        }
    }

    let mut out = Vec::new();
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut graphics = Vec::new();
    let mut line = 0;
    let mut consumed = 0usize;
    while consumed < char_count {
        while starts.get(line + 1).is_some_and(|&start| start <= consumed) {
            line += 1;
        }
        let text_start = text.len();
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
                text.push('\t');
                if replaces {
                    lines[line].parts.push(Hwp3Text::Tab(width));
                }
            }
            10 => {
                replaces = false;
                flush_paragraph(&mut out, &mut runs, &mut text);
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
                let graphic = if state.images && header[74] == 3 {
                    state.drawing(extra)
                } else {
                    None
                };
                let captions = paragraph_list(cursor, depth + 1, state)?;
                if let Some((key, [dx, dy, width, height], order)) = graphic {
                    if replaces
                        && captions.is_empty()
                        && matches!(header[8], 2 | 3)
                        && usize::from(read_u16_at(header, 66)?) == paragraph_page
                    {
                        let paper = state.paper.as_ref().unwrap();
                        let x = paper.left + i32::from(read_u16_at(header, 62)? as i16) + dx;
                        let y = paper.top + i32::from(read_u16_at(header, 64)? as i16) + dy;
                        graphics.push(Hwp3Graphic {
                            asset: key.clone(),
                            rect: [x, y, width, height],
                            order,
                        });
                    } else {
                        replaces = false;
                    }
                    flush_run(&mut runs, &mut text);
                    runs.push(Run {
                        text: "[그림]".into(),
                        image: Some(key),
                        image_alt: Some(String::new()),
                        image_geometry: Some(ImageGeometry {
                            width: Some(f64::from(width) / 25.),
                            height: Some(f64::from(height) / 25.),
                            ..ImageGeometry::default()
                        }),
                        ..Run::default()
                    });
                } else {
                    replaces = false;
                    state.partial_graphics |= state.images;
                    text.push_str("[그림]");
                }
                if !captions.is_empty() {
                    flush_paragraph(&mut out, &mut runs, &mut text);
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
                append_nested(cursor, depth, state, &mut out, &mut runs, &mut text)?;
            }
            16 => {
                replaces = false;
                cursor.skip(16)?;
                consumed += 3;
                append_nested(cursor, depth, state, &mut out, &mut runs, &mut text)?;
            }
            17 => {
                replaces = false;
                cursor.skip(20)?;
                consumed += 3;
                append_nested(cursor, depth, state, &mut out, &mut runs, &mut text)?;
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
                text.push('-');
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
                text.push('\u{00a0}');
            }
            31 => {
                cursor.skip(2)?;
                consumed += 1;
                text.push(' ');
            }
            _ => text.push(hnc_char(code).unwrap_or('\u{fffd}')),
        }
        if consumed > char_count {
            return Err("invalid HWP 3.0 control character length".into());
        }
        if replaces && code != 9 && text.len() > text_start {
            let value = &text[text_start..];
            if let Some(Hwp3Text::Text(last)) = lines[line].parts.last_mut() {
                last.push_str(value);
            } else {
                lines[line].parts.push(Hwp3Text::Text(value.into()));
            }
        }
    }
    flush_paragraph(&mut out, &mut runs, &mut text);
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
        return Err("invalid HWP 3.0 control character marker".into());
    }
    cursor.skip(size)
}

fn append_nested(
    cursor: &mut Cursor<'_>,
    depth: usize,
    state: &mut ParseState,
    out: &mut Vec<Block>,
    runs: &mut Vec<Run>,
    text: &mut String,
) -> Result<()> {
    let nested = paragraph_list(cursor, depth + 1, state)?;
    if !nested.is_empty() {
        flush_paragraph(out, runs, text);
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
        return Err("HWP 3.0 object nesting limit exceeded".into());
    }
    let info = cursor.take(84)?;
    let kind = info[78];
    let cell_count = usize::from(read_u16_at(info, 80)?);
    if cell_count > CELL_LIMIT {
        return Err("HWP 3.0 table cell limit exceeded".into());
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
        return Err("HWP 3.0 table cell limit exceeded".into());
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

fn flush_run(runs: &mut Vec<Run>, text: &mut String) {
    if !text.is_empty() {
        runs.push(Run::text(std::mem::take(text)));
    }
}

fn flush_paragraph(out: &mut Vec<Block>, runs: &mut Vec<Run>, text: &mut String) {
    flush_run(runs, text);
    if !runs.is_empty() {
        out.push(Block::flow(std::mem::take(runs)));
    }
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
        0x420f => Some('軍'),
        0x4216 => Some('弓'),
        0x4584 => Some('名'),
        0x4872 => Some('聖'),
        0x4ad5 => Some('領'),
        _ if code & 0x8000 != 0 => kssm_char(code),
        _ => char::from_u32(u32::from(code)).filter(|ch| !ch.is_control()),
    }
}

fn kssm_char(code: u16) -> Option<char> {
    let initial = ((code >> 10) & 0x1f) as usize;
    let medial = ((code >> 5) & 0x1f) as usize;
    let final_code = (code & 0x1f) as usize;
    let initial_index = initial.checked_sub(2).filter(|&n| n < 19)?;
    let medial_index = match medial {
        3..=7 => medial - 3,
        10..=15 => medial - 5,
        18..=23 => medial - 7,
        26..=29 => medial - 9,
        _ => return None,
    };
    let final_index = match final_code {
        1 => 0,
        2..=17 => final_code - 1,
        19..=29 => final_code - 2,
        _ => return None,
    };
    let offset = (initial_index * 21 + medial_index) * 28 + final_index;
    char::from_u32(0xac00 + offset as u32)
}
