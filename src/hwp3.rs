use crate::model::*;
use flate2::read::DeflateDecoder;
use std::{borrow::Cow, io::Read};

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

pub fn parse(data: &[u8]) -> Result<Document> {
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
    let mut nodes = 0;
    let blocks = paragraph_list(&mut cursor, 0, &mut nodes)?;
    let mut section = Section::new("section", "document");
    section.blocks = blocks;
    doc.sections.push(section);
    Ok(doc)
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

fn paragraph_list(cursor: &mut Cursor<'_>, depth: usize, nodes: &mut usize) -> Result<Vec<Block>> {
    if depth > MAX_NESTING {
        return Err("HWP 3.0 object nesting limit exceeded".into());
    }
    let mut blocks = Vec::new();
    loop {
        *nodes = (*nodes).saturating_add(1);
        if *nodes > NODE_LIMIT {
            return Err("HWP 3.0 paragraph limit exceeded".into());
        }
        let Some(mut paragraph) = parse_paragraph(cursor, depth, nodes)? else {
            return Ok(blocks);
        };
        blocks.append(&mut paragraph);
    }
}

fn parse_paragraph(
    cursor: &mut Cursor<'_>,
    depth: usize,
    nodes: &mut usize,
) -> Result<Option<Vec<Block>>> {
    let previous_shape = cursor.u8()?;
    let char_count = usize::from(cursor.u16()?);
    let line_count = usize::from(cursor.u16()?);
    let has_char_shapes = cursor.u8()? != 0;
    cursor.skip(1 + 4 + 1 + 31)?;
    if previous_shape == 0 && char_count > 0 {
        cursor.skip(187)?;
    }
    if char_count == 0 {
        return Ok(None);
    }
    cursor.skip(
        line_count
            .checked_mul(14)
            .ok_or("HWP 3.0 line table overflow")?,
    )?;
    if has_char_shapes {
        for _ in 0..char_count {
            if cursor.u8()? != 1 {
                cursor.skip(31)?;
            }
        }
    }

    let mut out = Vec::new();
    let mut text = String::new();
    let mut consumed = 0usize;
    while consumed < char_count {
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
                cursor.skip(6)?;
                consumed += 3;
                text.push('\t');
            }
            10 => {
                flush_text(&mut out, &mut text);
                cursor.skip(6)?;
                consumed += 3;
                parse_box(cursor, depth + 1, nodes, &mut out)?;
            }
            11 => {
                cursor.skip(6)?;
                consumed += 3;
                let extra = usize::try_from(cursor.u32()?)?;
                cursor.skip(
                    344usize
                        .checked_add(extra)
                        .ok_or("HWP 3.0 picture size overflow")?,
                )?;
                let captions = paragraph_list(cursor, depth + 1, nodes)?;
                text.push_str("[그림]");
                if !captions.is_empty() {
                    flush_text(&mut out, &mut text);
                    out.extend(captions);
                }
            }
            13 => {}
            14 => {
                cursor.skip(90)?;
                consumed += 3;
            }
            15 => {
                cursor.skip(14)?;
                consumed += 3;
                append_nested(cursor, depth, nodes, &mut out, &mut text)?;
            }
            16 => {
                cursor.skip(16)?;
                consumed += 3;
                append_nested(cursor, depth, nodes, &mut out, &mut text)?;
            }
            17 => {
                cursor.skip(20)?;
                consumed += 3;
                append_nested(cursor, depth, nodes, &mut out, &mut text)?;
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
    }
    flush_text(&mut out, &mut text);
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
    nodes: &mut usize,
    out: &mut Vec<Block>,
    text: &mut String,
) -> Result<()> {
    let nested = paragraph_list(cursor, depth + 1, nodes)?;
    if !nested.is_empty() {
        flush_text(out, text);
        out.extend(nested);
    }
    Ok(())
}

fn parse_box(
    cursor: &mut Cursor<'_>,
    depth: usize,
    nodes: &mut usize,
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
    let mut positions = Vec::with_capacity(cell_count);
    for _ in 0..cell_count {
        let metadata = cursor.take(27)?;
        positions.push((usize::from(metadata[0]), usize::from(metadata[1])));
    }
    let mut cells = Vec::with_capacity(cell_count);
    for _ in 0..cell_count {
        cells.push(paragraph_list(cursor, depth + 1, nodes)?);
    }
    let captions = paragraph_list(cursor, depth + 1, nodes)?;

    if kind == 0 && cell_count > 0 {
        let table = table_from_cells(positions, cells)?;
        out.push(table);
    } else {
        for blocks in cells {
            out.extend(blocks);
        }
    }
    out.extend(captions);
    Ok(())
}

fn table_from_cells(positions: Vec<(usize, usize)>, cells: Vec<Vec<Block>>) -> Result<Block> {
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
    let sequential = duplicate || grid_size > CELL_LIMIT || grid_size < cells.len();
    let (rows, columns) = if sequential {
        (1, cells.len())
    } else {
        (row_count, col_count)
    };
    if rows.saturating_mul(columns) > CELL_LIMIT {
        return Err("HWP 3.0 table cell limit exceeded".into());
    }
    let mut grid = (0..rows * columns)
        .map(|_| Cell::default())
        .collect::<Vec<_>>();
    for (index, blocks) in cells.into_iter().enumerate() {
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

fn flush_text(out: &mut Vec<Block>, text: &mut String) {
    if !text.is_empty() {
        out.push(Block::text(std::mem::take(text)));
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
