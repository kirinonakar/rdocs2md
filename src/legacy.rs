use crate::{formatting, model::*};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::{Cursor, Read},
};
type Ole<'a> = cfb::CompoundFile<Cursor<&'a [u8]>>;
pub fn u16le(b: &[u8], i: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        b.get(i..i + 2)
            .ok_or("truncated 16-bit field")?
            .try_into()?,
    ))
}
pub fn u32le(b: &[u8], i: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(i..i + 4)
            .ok_or("truncated 32-bit field")?
            .try_into()?,
    ))
}
pub fn utf16(b: &[u8]) -> Result<String> {
    if !b.len().is_multiple_of(2) {
        return Err("odd UTF-16 length".into());
    }
    Ok(String::from_utf16_lossy(
        &b.as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect::<Vec<_>>(),
    ))
}
fn stream(ole: &mut Ole, name: &str) -> Result<Vec<u8>> {
    let st = ole.open_stream(name)?;
    if st.len() > PART_LIMIT as u64 {
        return Err("OLE stream exceeds limit".into());
    }
    let mut v = Vec::new();
    st.take(PART_LIMIT as u64 + 1).read_to_end(&mut v)?;
    Ok(v)
}
pub fn parse(data: &[u8], images: bool, range: Option<&SectionRange>) -> Result<Document> {
    // The CFB reader validates allocation chains even in compatibility mode.
    // Some PowerPoint producers leave directory name terminators uninitialized.
    let mut ole = cfb::CompoundFile::open(Cursor::new(data))?;
    if ole.is_stream("/EncryptedPackage") || ole.is_stream("/EncryptionInfo") {
        return Err("encrypted Office package is unsupported".into());
    }
    if ole.is_stream("/FileHeader") {
        hwp(&mut ole, images, range)
    } else if ole.is_stream("/WordDocument") {
        word(&mut ole, range)
    } else if ole.is_stream("/PowerPoint Document") {
        ppt(&mut ole, range)
    } else if ole.is_stream("/Workbook") {
        crate::xls::parse(&stream(&mut ole, "/Workbook")?, range)
    } else {
        Err("unsupported OLE document streams".into())
    }
}
fn hwp_runs(data: &[u8], shapes: &[u8], styles: &[Style]) -> Result<Vec<Run>> {
    let mut runs = Vec::new();
    let mut units = Vec::new();
    let mut changes = shapes.as_chunks::<8>().0.iter().take(CELL_LIMIT).peekable();
    let mut style = None;
    let flush = |runs: &mut Vec<Run>, units: &mut Vec<u16>, style: Option<&Style>| {
        if !units.is_empty() {
            push_run(
                runs,
                Run {
                    text: String::from_utf16_lossy(units),
                    style: style.cloned().unwrap_or_default(),
                    ..Run::default()
                },
            );
            units.clear();
        }
    };
    let mut i = 0;
    while i < data.len() {
        let n = u16le(data, i)?;
        // Shape offsets count UTF-16 units, including the extended control bytes.
        if !(0xdc00..=0xdfff).contains(&n) {
            while let Some(shape) = changes.next_if(|v| u32le(*v, 0).unwrap() as usize <= i / 2) {
                let next = styles.get(u32le(shape, 4)? as usize);
                if next != style {
                    flush(&mut runs, &mut units, style);
                    style = next;
                }
            }
        }
        i += 2;
        match n {
            9 => {
                units.push(9);
                i += 14
            }
            10 => units.push(10),
            13 | 0 => {}
            1..=8 | 11..=12 | 14..=23 => i += 14,
            24 => units.push(45),
            30 | 31 => units.push(32),
            25..=29 => {}
            _ => units.push(n),
        }
        if i > data.len() {
            return Err("truncated HWP control".into());
        }
    }
    flush(&mut runs, &mut units, style);
    let Some(first) = runs.iter().position(|r| !r.text.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let last = runs
        .iter()
        .rposition(|r| !r.text.trim().is_empty())
        .unwrap();
    runs.truncate(last + 1);
    runs.drain(..first);
    let leading = runs[0].text.len() - runs[0].text.trim_start().len();
    runs[0].text.drain(..leading);
    let tail = &mut runs.last_mut().unwrap().text;
    tail.truncate(tail.trim_end().len());
    Ok(runs)
}

#[derive(Clone, Copy)]
struct HwpRecord<'a> {
    tag: u32,
    level: u32,
    data: &'a [u8],
}

fn hwp_record<'a>(raw: &mut &'a [u8]) -> Result<HwpRecord<'a>> {
    let header = u32le(raw, 0)?;
    let mut offset = 4;
    let mut size = (header >> 20) as usize;
    if size == 4095 {
        size = u32le(raw, offset)? as usize;
        offset += 4;
    }
    let (data, rest) = raw[offset..]
        .split_at_checked(size)
        .ok_or("truncated HWP record")?;
    *raw = rest;
    Ok(HwpRecord {
        tag: header & 1023,
        level: (header >> 10) & 1023,
        data,
    })
}

#[derive(Default)]
struct HwpFormatting {
    borders: Vec<Properties>,
    paragraphs: Vec<u32>,
    characters: Vec<Style>,
}

fn hwp_color(bytes: &[u8]) -> String {
    format!("#{:02X}{:02X}{:02X}", bytes[0], bytes[1], bytes[2])
}

impl HwpFormatting {
    fn read(mut info: &[u8]) -> Self {
        let mut styles = Self::default();
        let mut fonts = Vec::new();
        let mut hangul_fonts = 0;
        while !info.is_empty() {
            let Ok(record) = hwp_record(&mut info) else {
                break;
            };
            match record.tag {
                17 => {
                    hangul_fonts = u32le(record.data, 4)
                        .unwrap_or_default()
                        .min(u16::MAX as u32 + 1) as usize
                }
                19 if fonts.len() < hangul_fonts => {
                    let size = usize::from(u16le(record.data, 1).unwrap_or_default());
                    fonts.push(
                        (1..128)
                            .contains(&size)
                            .then(|| {
                                record
                                    .data
                                    .get(3..3 + size * 2)
                                    .and_then(|name| utf16(name).ok())
                            })
                            .flatten(),
                    );
                }
                20 if styles.borders.len() < u16::MAX as usize => {
                    let mut props = Properties::new();
                    if let Some(edges) = record.data.get(2..26) {
                        for (key, edge) in formatting::BORDERS
                            .into_iter()
                            .zip(edges.as_chunks::<6>().0)
                        {
                            let style = match edge[0] {
                                0 => "none",
                                2 | 4..=6 | 12 => "dashed",
                                3 | 7 => "dotted",
                                8..=11 | 13 => "double",
                                _ => "solid",
                            };
                            let widths = [
                                "0.1mm", "0.12mm", "0.15mm", "0.2mm", "0.25mm", "0.3mm", "0.4mm",
                                "0.5mm", "0.6mm", "0.7mm", "1mm", "1.5mm", "2mm", "3mm", "4mm",
                                "5mm",
                            ];
                            let width = widths.get(usize::from(edge[1])).unwrap_or(&"0.12mm");
                            props.insert(
                                key.into(),
                                formatting::border(style, width, Some(&hwp_color(&edge[2..]))),
                            );
                        }
                    }
                    if u32le(record.data, 32).is_ok_and(|flags| flags & 1 != 0)
                        && let Some(shade) = record.data.get(36..40)
                        && shade != [255; 4]
                    {
                        props.insert("background-color".into(), hwp_color(shade));
                    }
                    styles.borders.push(props);
                }
                21 if styles.characters.len() < CELL_LIMIT => {
                    let flags = u32le(record.data, 46).unwrap_or_default();
                    let size = u32le(record.data, 42)
                        .ok()
                        .filter(|n| (1..100_000).contains(n));
                    let font = u16le(record.data, 0)
                        .ok()
                        .and_then(|id| fonts.get(usize::from(id)))
                        .cloned()
                        .flatten();
                    styles.characters.push(Style {
                        font,
                        size: size.map(|n| format!("{}pt", f64::from(n) / 100.)),
                        color: record.data.get(52..56).map(hwp_color),
                        bold: flags & 2 != 0,
                        italic: flags & 1 != 0,
                        underline: (flags >> 2) & 3 == 1,
                        script: if flags & (1 << 15) != 0 {
                            Some("sup".into())
                        } else if flags & (1 << 16) != 0 {
                            Some("sub".into())
                        } else {
                            None
                        },
                        ..Style::default()
                    });
                }
                25 if styles.paragraphs.len() <= u16::MAX as usize => {
                    styles.paragraphs.push(
                        u32le(record.data, 0).unwrap_or_else(|_| {
                            u32::from(record.data.first().copied().unwrap_or(0))
                        }),
                    );
                }
                _ => {}
            }
        }
        styles
    }

    fn cell(&self, data: &[u8]) -> Result<Properties> {
        let id = usize::from(u16le(data, 32)?);
        let mut props = if id == 0 && !self.borders.is_empty() {
            formatting::BORDERS
                .into_iter()
                .map(|key| (key.into(), "none".into()))
                .collect()
        } else {
            id.checked_sub(1)
                .and_then(|i| self.borders.get(i))
                .cloned()
                .unwrap_or_default()
        };
        for (key, offset) in [("width", 16), ("height", 20)] {
            let value = u32le(data, offset)?;
            formatting::points(
                &mut props,
                key,
                (value > 0).then_some(f64::from(value) / 100.),
            );
        }
        let align = match (u32le(data, 4)? >> 5) & 3 {
            0 => "top",
            1 => "middle",
            2 => "bottom",
            _ => "",
        };
        formatting::box_property(&mut props, "vertical-align", align);
        for (key, offset) in [
            ("padding-left", 24),
            ("padding-right", 26),
            ("padding-top", 28),
            ("padding-bottom", 30),
        ] {
            formatting::points(
                &mut props,
                key,
                Some(f64::from(u16le(data, offset)?) / 100.),
            );
        }
        Ok(props)
    }
}

fn hwp_table<'a>(
    records: &mut std::iter::Peekable<impl Iterator<Item = Result<HwpRecord<'a>>>>,
    level: u32,
    depth: usize,
    out: &mut Vec<Block>,
    styles: &HwpFormatting,
) -> Result<()> {
    if depth >= 32 {
        return Err("HWP table nesting limit exceeded".into());
    }
    let child_level = level + 1;
    let mut grid = Vec::new();
    let mut columns = 0;
    let mut current = None;
    let mut trailing = Vec::new();
    while let Some(record) = records.next_if(|r| r.as_ref().map_or(true, |r| r.level > level)) {
        let record = record?;
        if record.level == child_level {
            match record.tag {
                77 => {
                    current = None;
                    let rows = u16le(record.data, 4)? as usize;
                    columns = u16le(record.data, 6)? as usize;
                    if rows * columns > CELL_LIMIT {
                        return Err("HWP table too large".into());
                    }
                    grid = (0..rows * columns)
                        .map(|_| None)
                        .collect::<Vec<Option<Cell>>>();
                }
                // Cell bodies share this level with their list headers. Captions have a
                // shorter list header and can occur before the table definition.
                72 => {
                    current = None;
                    if columns == 0 || record.data.len() < 34 {
                        continue;
                    }
                    // HWP 5 cell lists store two 16-bit fields before the
                    // 32-bit list flags. The cell address starts at byte eight.
                    let column = u16le(record.data, 8)? as usize;
                    let row = u16le(record.data, 10)? as usize;
                    let colspan = u16le(record.data, 12)? as usize;
                    let rowspan = u16le(record.data, 14)? as usize;
                    if colspan == 0
                        || rowspan == 0
                        || column + colspan > columns
                        || row + rowspan > grid.len() / columns
                    {
                        return Err("invalid HWP table cell bounds".into());
                    }
                    let index = row * columns + column;
                    if grid[index].is_some() {
                        return Err("duplicate HWP table cell".into());
                    }
                    grid[index] = Some(Cell {
                        colspan: (colspan > 1).then_some(colspan),
                        rowspan: (rowspan > 1).then_some(rowspan),
                        properties: styles.cell(record.data)?,
                        ..Cell::default()
                    });
                    current = Some(index);
                }
                66 => {
                    if let Some(index) = current {
                        hwp_block(
                            records,
                            record,
                            depth + 1,
                            &mut grid[index].as_mut().unwrap().blocks,
                            styles,
                        )?;
                    }
                }
                _ => {}
            }
        } else if let Some(index) = current {
            let cell = grid[index].as_mut().unwrap();
            hwp_block(records, record, depth + 1, &mut cell.blocks, styles)?;
        } else {
            let blocks = if columns == 0 {
                &mut *out
            } else {
                &mut trailing
            };
            hwp_block(records, record, depth + 1, blocks, styles)?;
        }
    }
    if columns > 0 && !grid.is_empty() {
        let mut covered = vec![false; grid.len()];
        let mut rows = Vec::new();
        for (row, cells) in grid.chunks_mut(columns).enumerate() {
            let mut items = Vec::new();
            for (column, slot) in cells.iter_mut().enumerate() {
                if covered[row * columns + column] {
                    if slot.is_some() {
                        return Err("overlapping HWP table cells".into());
                    }
                    continue;
                }
                let mut cell = slot.take().unwrap_or_default();
                for r in row..row + cell.rowspan.unwrap_or(1) {
                    let start = r * columns + column;
                    let end = start + cell.colspan.unwrap_or(1);
                    if covered[start..end].iter().any(|&occupied| occupied) {
                        return Err("overlapping HWP table spans".into());
                    }
                    covered[start..end].fill(true);
                }
                // Unnested cells can use a Markdown table, with line breaks inside cells.
                if cell
                    .blocks
                    .iter()
                    .all(|b| matches!(b, Block::Paragraph { .. }))
                {
                    let mut alignments = cell.blocks.iter().map(|block| match block {
                        Block::Paragraph { properties, .. } => properties.get("align"),
                        _ => None,
                    });
                    if let Some(Some(align)) = alignments.next()
                        && alignments.all(|a| a == Some(align))
                    {
                        cell.properties.insert("text-align".into(), align.clone());
                    }
                    for block in std::mem::take(&mut cell.blocks) {
                        if let Block::Paragraph { runs, .. } = block {
                            if !cell.runs.is_empty() {
                                push_run(&mut cell.runs, Run::text("\n"));
                            }
                            for run in runs {
                                push_run(&mut cell.runs, run);
                            }
                        }
                    }
                }
                items.push(cell);
            }
            rows.push(items);
        }
        out.push(Block::Table { rows });
    }
    out.extend(trailing);
    Ok(())
}

fn hwp_block<'a>(
    records: &mut std::iter::Peekable<impl Iterator<Item = Result<HwpRecord<'a>>>>,
    record: HwpRecord<'a>,
    depth: usize,
    out: &mut Vec<Block>,
    styles: &HwpFormatting,
) -> Result<()> {
    match record.tag {
        66 => {
            if depth == 0
                && record.level == 0
                && record.data.get(11).is_some_and(|flags| flags & 4 != 0)
                && !out.is_empty()
            {
                out.push(Block::page_break());
            }
            if let Some(text) = records.next_if(|r| {
                r.as_ref()
                    .is_ok_and(|p| p.tag == 67 && p.level == record.level + 1)
            }) {
                let shapes = records
                    .next_if(|r| {
                        r.as_ref()
                            .is_ok_and(|p| p.tag == 68 && p.level == record.level + 1)
                    })
                    .transpose()?
                    .map_or(&[][..], |p| p.data);
                let runs = hwp_runs(text?.data, shapes, &styles.characters)?;
                if !runs.is_empty() {
                    let mut block = Block::flow(runs);
                    if let Block::Paragraph {
                        heading,
                        list,
                        properties,
                        ..
                    } = &mut block
                        && let Ok(index) = u16le(record.data, 8)
                        && let Some(&flags) = styles.paragraphs.get(usize::from(index))
                    {
                        let align = match (flags >> 2) & 7 {
                            0 | 4 | 5 => Some("justify"),
                            1 => Some("left"),
                            2 => Some("right"),
                            3 => Some("center"),
                            _ => None,
                        };
                        if let Some(align) = align {
                            properties.insert("align".into(), align.into());
                        }
                        let level = ((flags >> 25) & 7) as u8;
                        match (flags >> 23) & 3 {
                            1 => *heading = (level + 1).min(6),
                            kind @ (2 | 3) => {
                                *list = Some(List {
                                    ordered: kind == 2,
                                    depth: usize::from(level),
                                });
                            }
                            _ => {}
                        }
                    }
                    out.push(block);
                }
            }
        }
        67 => {
            let runs = hwp_runs(record.data, &[], &[])?;
            if !runs.is_empty() {
                out.push(Block::flow(runs));
            }
        }
        71 if record.data.starts_with(b" lbt") => {
            hwp_table(records, record.level, depth, out, styles)?
        }
        76 if record.data.starts_with(b"cip$") => {
            if let Some(picture) = records.next_if(|r| {
                r.as_ref()
                    .is_ok_and(|p| p.tag == 85 && p.level == record.level + 1)
            }) && let Ok(id) = u16le(picture?.data, 71)
                && id != 0
            {
                // The common shape record repeats the control ID for a top-level object.
                let base = if record.data.starts_with(b"cip$cip$") {
                    8
                } else {
                    4
                };
                let dimensions = || -> Result<ImageLayout> {
                    let width = (u32le(record.data, base + 20)? as i32).unsigned_abs();
                    let height = (u32le(record.data, base + 24)? as i32).unsigned_abs();
                    let rotation_deg = u16le(record.data, base + 32)? % 360;
                    Ok(ImageLayout {
                        width,
                        height,
                        rotation_deg,
                    })
                };
                out.push(Block::flow(vec![Run {
                    image: Some(format!("hwp/{id}")),
                    image_layout: dimensions().ok().filter(|layout| {
                        (1..=200_000).contains(&layout.width)
                            && (1..=200_000).contains(&layout.height)
                    }),
                    ..Run::default()
                }]));
            }
        }
        _ => {}
    }
    Ok(())
}

fn hwp_stream(ole: &mut Ole, path: &str, compressed: bool) -> Result<Vec<u8>> {
    let bytes = stream(ole, path)?;
    if !compressed {
        return Ok(bytes);
    }
    let mut decoded = Vec::new();
    flate2::read::DeflateDecoder::new(&bytes[..])
        .take(PART_LIMIT as u64 + 1)
        .read_to_end(&mut decoded)?;
    if decoded.len() > PART_LIMIT {
        return Err("HWP stream expansion limit exceeded".into());
    }
    Ok(decoded)
}

fn hwp_images(
    ole: &mut Ole,
    doc: &mut Document,
    compressed: bool,
    mut total: usize,
    mut remaining: &[u8],
) -> Result<()> {
    let sources = image_sources(doc);
    if sources.is_empty() {
        return Ok(());
    }
    let mut id = 0;
    while !remaining.is_empty() {
        let record = hwp_record(&mut remaining)?;
        if record.tag != 18 {
            continue;
        }
        // Picture records use the one-based BinData record index, not the stream ID.
        id += 1;
        let key = format!("hwp/{id}");
        if !sources.contains(&key) {
            continue;
        }
        let attributes = u16le(record.data, 0)?;
        if attributes & 0xf != 1 {
            continue;
        }
        let stream_id = u16le(record.data, 2)?;
        let length = usize::from(u16le(record.data, 4)?) * 2;
        let ext = utf16(
            record
                .data
                .get(6..6 + length)
                .ok_or("truncated HWP image extension")?,
        )?;
        if ext.is_empty() || ext.len() > 8 || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
            continue;
        }
        let path = format!("/BinData/BIN{stream_id:04X}.{ext}");
        if !ole.is_stream(&path) {
            continue;
        }
        let packed = match attributes & 0x30 {
            0x10 => true,
            0x20 => false,
            _ => compressed,
        };
        let bytes = hwp_stream(ole, &path, packed)?;
        total += bytes.len();
        if total > EXPANSION_LIMIT {
            return Err("HWP expansion limit exceeded".into());
        }
        if image_ext(&bytes).is_some() {
            doc.assets.insert(key, bytes);
        }
    }
    Ok(())
}

fn hwp(ole: &mut Ole, images: bool, range: Option<&SectionRange>) -> Result<Document> {
    let head = stream(ole, "/FileHeader")?;
    if !head.starts_with(b"HWP Document File") || head.get(35) != Some(&5) {
        return Err("expected HWP version 5".into());
    }
    let flags = u32le(&head, 36)?;
    if flags & 6 != 0 {
        return Err("encrypted or distribution-protected HWP is unsupported".into());
    }
    let mut files: Vec<_> = ole
        .walk()
        .filter(|e| e.is_stream())
        .filter_map(|e| {
            let path = e.path().to_str()?.replace('\\', "/");
            let index = path
                .strip_prefix("/BodyText/Section")?
                .parse::<u32>()
                .ok()?;
            Some((index, path))
        })
        .collect();
    files.sort();
    let mut doc = Document::new("hwp");
    let compressed = flags & 1 != 0;
    let info = if ole.is_stream("/DocInfo") {
        hwp_stream(ole, "/DocInfo", compressed)?
    } else {
        Vec::new()
    };
    let mut total = info.len();
    let styles = HwpFormatting::read(&info);
    for i in section_indices(range, files.len())? {
        let (_, path) = &files[i];
        let raw = hwp_stream(ole, path, compressed)?;
        total += raw.len();
        if total > EXPANSION_LIMIT {
            return Err("HWP expansion limit exceeded".into());
        }
        let mut sec = Section::new("section", path).indexed(i);
        let mut remaining = raw.as_slice();
        let mut records = std::iter::from_fn(|| {
            if remaining.is_empty() {
                return None;
            }
            Some(hwp_record(&mut remaining))
        })
        .peekable();
        while let Some(record) = records.next() {
            hwp_block(&mut records, record?, 0, &mut sec.blocks, &styles)?;
        }
        doc.sections.push(sec)
    }
    if images {
        hwp_images(ole, &mut doc, compressed, total, &info)?;
    }
    Ok(doc)
}
fn word(ole: &mut Ole, range: Option<&SectionRange>) -> Result<Document> {
    let data = stream(ole, "/WordDocument")?;
    if u16le(&data, 0)? != 0xa5ec || u16le(&data, 2)? < 0xc1 {
        return Err("expected Word 97–2003 FIB".into());
    }
    let flags = u16le(&data, 10)?;
    if flags & 0x8100 != 0 {
        return Err("encrypted DOC is unsupported".into());
    }
    let table = stream(
        ole,
        if flags & 0x200 != 0 {
            "/1Table"
        } else {
            "/0Table"
        },
    )?;
    let mut p = 34 + 2 * u16le(&data, 32)? as usize;
    let longs = u16le(&data, p)? as usize;
    p += 2;
    let main = u32le(&data, p + 12)? as usize;
    p += longs * 4;
    let pairs = u16le(&data, p)? as usize;
    if pairs < 34 {
        return Err("missing Word piece table".into());
    }
    let clx = p + 2 + 33 * 8;
    // Table structure lives in paragraph properties, outside the text stream.
    let paps = word_paps(&data, &table, p + 2, pairs);
    let ttp = paps.iter().any(|p| p.ttp || p.inner_ttp);
    let off = u32le(&data, clx)? as usize;
    let len = u32le(&data, clx + 4)? as usize;
    let clx = table.get(off..off + len).ok_or("invalid Word CLX range")?;
    let mut i = 0;
    while clx.get(i) == Some(&1) {
        i += 3 + u16le(clx, i + 1)? as usize
    }
    if clx.get(i) != Some(&2) {
        return Err("Word Pcdt missing".into());
    }
    let len = u32le(clx, i + 1)? as usize;
    let plc = clx.get(i + 5..i + 5 + len).ok_or("invalid Pcdt size")?;
    if len < 4 || !(len - 4).is_multiple_of(12) {
        return Err("malformed Word piece table".into());
    }
    let pieces = (len - 4) / 12;
    let mut doc = Document::new("doc");
    let mut sec = Section::new("section", "WordDocument");
    let mut line = String::new();
    let mut line_pap = None;
    let mut fields = Vec::new();
    let mut index = 1;
    let mut selected = range.is_none_or(|r| r.contains(index));
    let mut text_bytes = 0;
    let mut tables = WordTables {
        ttp,
        ..WordTables::default()
    };
    for n in 0..pieces {
        let cp = u32le(plc, n * 4)? as usize;
        let end = (u32le(plc, (n + 1) * 4)? as usize).min(main);
        if cp >= main {
            break;
        }
        let chars = end.checked_sub(cp).ok_or("unsorted Word piece table")?;
        let fc = u32le(plc, (pieces + 1) * 4 + n * 8 + 2)?;
        let compressed = fc & 0x40000000 != 0;
        let pos = (fc & 0x3fffffff) as usize;
        let text = if compressed {
            let b = data
                .get(pos / 2..pos / 2 + chars)
                .ok_or("Word piece out of bounds")?;
            encoding_rs::WINDOWS_1252.decode(b).0
        } else {
            Cow::Owned(utf16(
                data.get(pos..pos + 2 * chars)
                    .ok_or("Word Unicode piece out of bounds")?,
            )?)
        };
        text_bytes += text.len();
        if text_bytes > PART_LIMIT {
            return Err("Word text exceeds limit".into());
        }
        // Paragraph properties are keyed by file offset, so the offset has to
        // follow every character of the piece.
        let mut offset = if compressed { pos / 2 } else { pos };
        for c in text.chars() {
            let pap = word_pap_at(&paps, offset);
            offset += if compressed { 1 } else { c.len_utf16() * 2 };
            match c {
                '\u{13}' => {
                    if fields.len() > 64 {
                        return Err("excessive Word field depth".into());
                    }
                    fields.push(false)
                }
                '\u{14}' => {
                    if let Some(s) = fields.last_mut() {
                        *s = true
                    }
                }
                '\u{15}' => {
                    fields.pop();
                }
                _ if fields.contains(&false) => {}
                '\u{c}' => {
                    for block in tables.finish() {
                        sec.blocks.push(block);
                    }
                    if selected {
                        word_paragraph(&mut sec, std::mem::take(&mut line), line_pap.take());
                        doc.sections.push(sec.indexed(index - 1));
                        sec = Section::new("section", "WordDocument");
                    }
                    if range.is_some_and(|r| r.ends_at(index)) {
                        return Ok(doc);
                    }
                    index += 1;
                    selected = range.is_none_or(|r| r.contains(index));
                }
                _ if !selected => {}
                _ if tables.enter_table(pap) => {
                    word_paragraph(&mut sec, std::mem::take(&mut line), line_pap.take());
                    tables.push(c, pap);
                }
                _ => {
                    for block in tables.finish() {
                        sec.blocks.push(block);
                    }
                    line_pap = pap.or(line_pap);
                    match c {
                        '\r' | '\u{7}' => {
                            word_paragraph(&mut sec, std::mem::take(&mut line), line_pap.take())
                        }
                        '\u{b}' => line.push('\n'),
                        '\t' => line.push('\t'),
                        _ if !c.is_control() => line.push(c),
                        _ => {}
                    }
                }
            }
        }
    }
    if let Some(range) = range {
        range.validate(index)?;
    }
    if selected {
        for block in tables.finish() {
            sec.blocks.push(block);
        }
        word_paragraph(&mut sec, line, line_pap);
        doc.sections.push(sec.indexed(index - 1));
    }
    Ok(doc)
}

fn word_paragraph(section: &mut Section, text: String, pap: Option<&WordPap>) {
    if !text.is_empty() {
        let heading = pap.map_or(0, |pap| match pap.style {
            1..=9 => pap.style.min(6) as u8,
            _ => pap.outline,
        });
        section.blocks.push(Block::Paragraph {
            heading,
            list: None,
            runs: vec![Run::text(text)],
            properties: Properties::new(),
        });
    }
}

/// Outline and table properties from Word 97–2003 paragraph property pages.
#[derive(Default)]
struct WordPap {
    /// File offset where the paragraph described by this entry ends.
    fc_end: usize,
    style: u16,
    outline: u8,
    in_table: bool,
    ttp: bool,
    inner_ttp: bool,
    itap: usize,
    tap: Option<WordTap>,
    edges: Option<[Option<String>; 6]>,
}

/// Cell definitions of one table row: the `rgdxaCenter` column boundaries in
/// points followed by one descriptor per grid column.
#[derive(Clone, Default)]
struct WordTap {
    bounds: Vec<f32>,
    cells: Vec<WordTc>,
}

#[derive(Clone, Default)]
struct WordTc {
    /// The cell continues the horizontally merged cell before it.
    merged: bool,
    vert_merge: bool,
    vert_restart: bool,
    borders: [Option<String>; 4],
}

impl WordTap {
    /// Grid column of every text cell, as `(start, span)`. A merged cell keeps
    /// its own descriptor and the columns it swallowed follow as descriptors
    /// that only set the merge flag, so they belong to the cell before them.
    fn groups(&self, cells: usize) -> Option<Vec<(usize, usize)>> {
        let mut starts = vec![0usize];
        for (i, tc) in self.cells.iter().enumerate().skip(1) {
            if !tc.merged {
                starts.push(i);
            }
        }
        (starts.len() == cells).then(|| {
            starts
                .iter()
                .enumerate()
                .map(|(k, &start)| {
                    let end = starts.get(k + 1).copied().unwrap_or(self.cells.len());
                    (start, end - start)
                })
                .collect()
        })
    }
}

fn word_pap_at(paps: &[WordPap], fc: usize) -> Option<&WordPap> {
    paps.get(paps.partition_point(|p| p.fc_end <= fc))
}

fn word_paps(data: &[u8], table: &[u8], base: usize, pairs: usize) -> Vec<WordPap> {
    let mut paps = Vec::new();
    if pairs >= 14 {
        word_papx(data, table, base, &mut paps);
    }
    paps.sort_unstable_by_key(|p| p.fc_end);
    paps
}

/// Read every property page named by the `PlcfBtePapx` bin table. Property data
/// for a page's paragraphs sits behind their forward links, at the page offset
/// recorded for each of them.
fn word_papx(data: &[u8], table: &[u8], base: usize, paps: &mut Vec<WordPap>) {
    let Ok(off) = u32le(table, base + 13 * 8) else {
        return;
    };
    let Ok(len) = u32le(table, base + 13 * 8 + 4) else {
        return;
    };
    let (off, len) = (off as usize, len as usize);
    let Some(plc) = table
        .get(off..off + len)
        .filter(|_| len >= 4 && (len - 4).is_multiple_of(8))
    else {
        return;
    };
    let pages = (len - 4) / 8;
    for n in 0..pages {
        let Ok(page) = u32le(plc, (pages + 1) * 4 + n * 4) else {
            return;
        };
        let start = (page & 0x003f_ffff) as usize * 512;
        let Some(page) = data.get(start..start + 512) else {
            return;
        };
        let count = page[511] as usize;
        let rgbx = 4 * (count + 1);
        if count > 0x1d || rgbx + 13 * count > 511 {
            return;
        }
        for k in 0..count {
            let Ok(end) = u32le(page, (k + 1) * 4) else {
                return;
            };
            let mut pap = WordPap {
                fc_end: end as usize,
                ..WordPap::default()
            };
            let offset = page[rgbx + 13 * k] as usize;
            if offset != 0 && 2 * offset < 511 {
                word_pap(&page[2 * offset..], table, &mut pap);
            }
            paps.push(pap);
        }
    }
}

/// Apply one `PapxInFkp` record. The record holds the style index of the
/// paragraph followed by the property modifiers that override it.
fn word_pap(record: &[u8], table: &[u8], pap: &mut WordPap) {
    let Some(&cb) = record.first() else {
        return;
    };
    let (start, len) = if cb == 0 {
        (2, 2 * record.get(1).copied().unwrap_or(0) as usize)
    } else {
        (1, 2 * cb as usize - 1)
    };
    let grpprl = record
        .get(start..)
        .map(|b| &b[..len.min(b.len())])
        .unwrap_or_default();
    pap.style = u16le(grpprl, 0).unwrap_or_default();
    word_sprms(grpprl.get(2..).unwrap_or_default(), table, pap, 0);
}

/// Read property modifiers. `depth` bounds the `sprmPHugePapx` indirection that
/// moves an oversized modifier list out of its page and into the table stream.
fn word_sprms(mut b: &[u8], table: &[u8], pap: &mut WordPap, depth: usize) {
    while b.len() > 2 {
        let sprm = u16::from_le_bytes([b[0], b[1]]);
        b = &b[2..];
        if matches!(sprm, 0xd606 | 0xd608) {
            let Some((tap, used)) = word_tap(b) else {
                return;
            };
            pap.tap = Some(tap);
            b = &b[used..];
            continue;
        }
        let (size, skip) = match sprm >> 13 {
            0 | 1 => (1, 0),
            2 | 4 | 5 => (2, 0),
            3 => (4, 0),
            7 => (3, 0),
            // A variable modifier records its size in the first byte, or in the
            // two bytes behind a 0xFF marker.
            _ => match b.first().copied().unwrap_or(0) {
                0xff => (
                    b.get(1..3)
                        .map_or(0, |v| usize::from(u16::from_le_bytes([v[0], v[1]]))),
                    2,
                ),
                n => (usize::from(n), 1),
            },
        };
        let total = size.saturating_add(skip);
        if total == 0 || b.len() < total {
            return;
        }
        let data = &b[skip..total];
        match sprm {
            0x2416 => pap.in_table = data.first().is_some_and(|v| *v != 0),
            0x2417 => pap.ttp = data.first().is_some_and(|v| *v != 0),
            0x244c => pap.inner_ttp = data.first().is_some_and(|v| *v != 0),
            0x2640 => {
                pap.outline = data
                    .first()
                    .filter(|&&level| level < 9)
                    .map_or(0, |&level| (level + 1).min(6));
            }
            0x6646 => word_huge_papx(data, table, pap, depth),
            0x6649 => {
                if let Some(v) = data.get(..4) {
                    pap.itap = u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as usize;
                }
            }
            0xd605 => pap.edges = word_table_borders(data),
            _ => {}
        }
        b = &b[total..];
    }
}

fn word_huge_papx(data: &[u8], table: &[u8], pap: &mut WordPap, depth: usize) {
    if depth >= 4 {
        return;
    }
    let Some(v) = data.get(..4) else {
        return;
    };
    let offset = u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as usize;
    let Some(size) = table.get(offset..offset + 2) else {
        return;
    };
    let size = u16::from_le_bytes([size[0], size[1]]) as usize;
    let Some(grpprl) = table.get(offset + 2..offset + 2 + size) else {
        return;
    };
    word_sprms(grpprl, table, pap, depth + 1);
}

/// Cell definitions of a row: the column boundaries followed by one descriptor
/// per grid column. Word records the operand size in two bytes and counts the
/// column byte in it, so the neighbouring spellings are tried as well.
fn word_tap(b: &[u8]) -> Option<(WordTap, usize)> {
    for (field, extra) in [(2usize, 1usize), (2, 0), (1, 0)] {
        let Some(head) = b.get(..field + 1) else {
            continue;
        };
        let size = match field {
            1 => usize::from(head[0]),
            _ => usize::from(u16::from_le_bytes([head[0], head[1]])),
        };
        let Some(size) = size.checked_sub(extra) else {
            continue;
        };
        let columns = usize::from(head[field]);
        if columns == 0 || columns > 63 || size != 1 + 2 * (columns + 1) + 20 * columns {
            continue;
        }
        let Some(operand) = b.get(field..field + size) else {
            continue;
        };
        let mut bounds = Vec::with_capacity(columns + 1);
        for i in 0..=columns {
            let at = 1 + 2 * i;
            bounds.push(f32::from(i16::from_le_bytes([operand[at], operand[at + 1]])) / 20.);
        }
        if bounds.windows(2).any(|w| w[1] < w[0]) {
            continue;
        }
        let mut cells = Vec::with_capacity(columns);
        for i in 0..columns {
            let at = 1 + 2 * (columns + 1) + 20 * i;
            let flags = u16::from_le_bytes([operand[at], operand[at + 1]]);
            let mut borders = [None, None, None, None];
            for (side, border) in borders.iter_mut().enumerate() {
                *border = word_brc(&operand[at + 4 + 4 * side..at + 8 + 4 * side]);
            }
            cells.push(WordTc {
                // 1 continues a merged set; 2 and 3 start one.
                merged: flags & 3 == 1,
                vert_merge: flags & 0x20 != 0,
                vert_restart: flags & 0x40 != 0,
                borders,
            });
        }
        return Some((WordTap { bounds, cells }, field + size));
    }
    None
}

/// One cell side. Word stores the width in eighths of a point, the line style
/// and an index into a fixed palette.
fn word_brc(b: &[u8]) -> Option<String> {
    const PALETTE: [&str; 17] = [
        "currentColor",
        "#000000",
        "#0000ff",
        "#00ffff",
        "#00ff00",
        "#ff00ff",
        "#ff0000",
        "#ffff00",
        "#ffffff",
        "#000080",
        "#008080",
        "#008000",
        "#800080",
        "#800000",
        "#808000",
        "#808080",
        "#c0c0c0",
    ];
    let style = match *b.get(1)? {
        0 => return None,
        3 | 9..=18 => "double",
        4 => "dotted",
        5 | 7 | 8 | 21 | 22 => "dashed",
        6 => "solid",
        _ => "solid",
    };
    let width = f32::from(*b.first()?) / 8.;
    let width = if width < 0.25 {
        "0.5pt".to_string()
    } else {
        format!("{width}pt")
    };
    let shade = PALETTE
        .get(*b.get(2).unwrap_or(&0) as usize)
        .copied()
        .unwrap_or("#000000");
    Some(format!("{width} {style} {shade}"))
}

/// The borders of a whole table: top, left, bottom, right, then the borders
/// between rows and between columns.
fn word_table_borders(b: &[u8]) -> Option<[Option<String>; 6]> {
    if b.len() < 24 {
        return None;
    }
    let mut edges: [Option<String>; 6] = Default::default();
    for (slot, edge) in edges.iter_mut().zip(b.as_chunks::<4>().0) {
        *slot = word_brc(edge);
    }
    Some(edges)
}

/// Build the cell for a run of text collected from the text stream. Cell marks
/// also end the paragraph around them, so a trailing break is dropped.
fn table_cell(text: String) -> Cell {
    let text = text.trim_end_matches('\n');
    if text.is_empty() {
        Cell::default()
    } else {
        Cell::text(text)
    }
}

/// Rebuild the tables of the Word text stream. A row mark carries the cell
/// definitions of its row, so text is collected per cell and folded into the
/// model once the row ends.
#[derive(Default)]
struct WordTables {
    open: Vec<WordTable>,
    text: String,
    /// Whether the document records row marks as paragraph properties.
    ttp: bool,
    /// The previous character closed a cell.
    mark: bool,
    cells: usize,
    disabled: bool,
}

#[derive(Default)]
struct WordTable {
    rows: Vec<Vec<Cell>>,
    row: Vec<Cell>,
    tap: Option<WordTap>,
    edges: Option<[Option<String>; 6]>,
    merges: HashMap<usize, (usize, usize)>,
    /// A nested table was placed in the cell that is currently open.
    nested: bool,
}

impl WordTables {
    fn enter_table(&mut self, pap: Option<&WordPap>) -> bool {
        match pap {
            Some(p) if p.in_table => self.enter(p),
            _ => false,
        }
    }

    /// Follow the table depth of the paragraph that is about to be read. A
    /// deeper paragraph starts a table inside the cell that is open.
    fn enter(&mut self, pap: &WordPap) -> bool {
        if self.disabled {
            return false;
        }
        if self.cells > CELL_LIMIT {
            self.disabled = true;
            self.open.clear();
            self.text.clear();
            return false;
        }
        let depth = pap.itap.clamp(1, 8);
        while self.open.len() > depth {
            self.close_level();
        }
        while self.open.len() < depth {
            self.reserve_cell();
            if let Some(parent) = self.open.last_mut() {
                parent.nested = true;
            }
            self.open.push(WordTable::default());
        }
        true
    }

    /// Reserve the cell that a nested table is placed in.
    fn reserve_cell(&mut self) {
        if let Some(t) = self.open.last_mut()
            && !t.nested
        {
            let text = std::mem::take(&mut self.text);
            t.row.push(table_cell(text));
        }
    }

    fn push(&mut self, c: char, pap: Option<&WordPap>) {
        if self.open.is_empty() {
            return;
        }
        let cell_mark = c == '\u{7}';
        // A row ends at its row mark: a cell mark that also carries the table
        // properties on depth one, or a paragraph mark inside a nested table.
        // Text without any row mark falls back to two cell marks in a row.
        let row = if cell_mark {
            pap.is_some_and(|p| p.ttp) || (!self.ttp && self.mark && self.text.is_empty())
        } else {
            pap.is_some_and(|p| p.inner_ttp)
        };
        if row {
            if !self.text.is_empty() {
                self.cell_mark();
            }
            if let Some(p) = pap
                && let Some(t) = self.open.last_mut()
            {
                t.tap = p.tap.clone();
                if t.edges.is_none() {
                    t.edges = p.edges.clone();
                }
            }
            self.end_row();
            return;
        }
        match c {
            '\u{7}' => {
                self.mark = true;
                self.cell_mark();
            }
            '\r' | '\u{c}' | '\u{b}' => {
                self.mark = false;
                self.text.push('\n');
            }
            '\t' => {
                self.mark = false;
                self.text.push('\t');
            }
            c if !c.is_control() => {
                self.mark = false;
                self.text.push(c);
            }
            _ => {}
        }
    }

    /// The cell that is collecting text ends at a cell mark.
    fn cell_mark(&mut self) {
        if self.open.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        if let Some(t) = self.open.last_mut() {
            if t.nested {
                t.nested = false;
                if !text.is_empty()
                    && let Some(cell) = t.row.last_mut()
                {
                    push_run(&mut cell.runs, Run::text(text));
                }
                return;
            }
            t.row.push(table_cell(text));
        }
        self.cells += 1;
    }

    fn flush_cell(&mut self) {
        if !self.text.is_empty() {
            self.cell_mark();
        }
    }

    fn end_row(&mut self) {
        self.mark = false;
        let Some(t) = self.open.last_mut() else {
            return;
        };
        if t.row.is_empty() {
            t.tap = None;
            return;
        }
        let row = std::mem::take(&mut t.row);
        t.row_done(row);
    }

    /// Finish the innermost table and hand it to the cell around it.
    fn close_level(&mut self) {
        if self.open.is_empty() {
            return;
        }
        self.flush_cell();
        self.end_row();
        let Some(table) = self.open.pop() else {
            return;
        };
        if self.open.is_empty() {
            return;
        }
        if let Some(block) = table.finish()
            && let Some(cell) = self.open.last_mut().and_then(|t| t.row.last_mut())
        {
            cell.blocks.push(block);
        }
    }

    /// Finish every open table, outermost last.
    fn finish(&mut self) -> Vec<Block> {
        let mut blocks = Vec::new();
        while !self.open.is_empty() {
            if self.open.len() > 1 {
                self.close_level();
                continue;
            }
            self.flush_cell();
            self.end_row();
            if let Some(table) = self.open.pop()
                && let Some(block) = table.finish()
            {
                blocks.push(block);
            }
        }
        self.text.clear();
        self.mark = false;
        blocks
    }
}

impl WordTable {
    /// Fold the collected cells into the row, applying the cell definitions
    /// recorded on the row mark: column widths, spans and borders.
    fn row_done(&mut self, row: Vec<Cell>) {
        let tap = self.tap.take();
        let groups = tap.as_ref().and_then(|t| t.groups(row.len()));
        let y = self.rows.len();
        let mut out: Vec<Cell> = Vec::with_capacity(row.len());
        for (index, cell) in row.into_iter().enumerate() {
            let (column, span) = groups.as_ref().map_or((index, 1), |g| g[index]);
            let Some(tc) = tap.as_ref().and_then(|t| t.cells.get(column)) else {
                out.push(cell);
                continue;
            };
            let mut cell = cell;
            if let Some(tap) = tap.as_ref() {
                let left = tap.bounds.get(column).copied().unwrap_or(0.);
                let right = tap.bounds.get(column + span).copied().unwrap_or(left);
                formatting::points(
                    &mut cell.properties,
                    "width",
                    Some(f64::from(right - left)).filter(|w| *w > 0.),
                );
            }
            if span > 1 {
                cell.colspan = Some(span);
            }
            for (key, border) in formatting::BORDERS.iter().zip(tc.borders.iter()) {
                if let Some(border) = border {
                    cell.properties.insert((*key).into(), border.clone());
                }
            }
            if tc.vert_restart {
                self.merges.insert(column, (y, out.len()));
            } else if tc.vert_merge {
                if let Some(&(first, at)) = self.merges.get(&column)
                    && let Some(origin) = self.rows.get_mut(first).and_then(|r| r.get_mut(at))
                {
                    origin.rowspan = Some(origin.rowspan.unwrap_or(1) + 1);
                    if let Some(bottom) = cell.properties.remove("border-bottom") {
                        origin.properties.insert("border-bottom".into(), bottom);
                    }
                    continue;
                }
            } else {
                self.merges.remove(&column);
            }
            out.push(cell);
        }
        // A row whose cells were all folded into the row above leaves nothing.
        if !out.is_empty() {
            self.rows.push(out);
        }
    }

    /// Close the table, filling in the sides its borders do not name.
    fn finish(mut self) -> Option<Block> {
        self.apply_edges();
        let bordered = self.rows.iter().flatten().any(|c| {
            formatting::BORDERS
                .iter()
                .any(|key| c.properties.contains_key(*key))
        });
        if bordered {
            for cell in self.rows.iter_mut().flatten() {
                for key in formatting::BORDERS {
                    cell.properties
                        .entry(key.into())
                        .or_insert_with(|| "none".into());
                }
            }
        }
        (!self.rows.is_empty()).then_some(Block::Table { rows: self.rows })
    }

    /// Apply the table-wide borders to the sides the cells do not set
    /// themselves: the outer edges of the table, otherwise the borders between
    /// its rows and columns.
    fn apply_edges(&mut self) {
        let Some(edges) = self.edges.take() else {
            return;
        };
        let height = self.rows.len();
        let columns = self
            .rows
            .iter()
            .map(|row| row.iter().map(|c| c.colspan.unwrap_or(1)).sum::<usize>())
            .max()
            .unwrap_or(0);
        for (y, row) in self.rows.iter_mut().enumerate() {
            let mut x = 0;
            for cell in row.iter_mut() {
                let span = cell.colspan.unwrap_or(1);
                let outer = [
                    x == 0,
                    x + span >= columns,
                    y == 0,
                    y + cell.rowspan.unwrap_or(1) >= height,
                ];
                for ((key, edge), inside) in formatting::BORDERS
                    .into_iter()
                    .zip(outer)
                    .zip([5usize, 5, 4, 4])
                {
                    let index = if edge {
                        match key {
                            "border-top" => 0,
                            "border-left" => 1,
                            "border-bottom" => 2,
                            _ => 3,
                        }
                    } else {
                        inside
                    };
                    if let Some(value) = &edges[index] {
                        cell.properties
                            .entry(key.into())
                            .or_insert_with(|| value.clone());
                    }
                }
                x += span;
            }
        }
    }
}
struct Record<'a> {
    kind: u16,
    instance: u16,
    container: bool,
    body: &'a [u8],
}
fn record(b: &[u8], i: usize) -> Result<Record<'_>> {
    let ver = u16le(b, i)?;
    let kind = u16le(b, i + 2)?;
    let len = u32le(b, i + 4)? as usize;
    Ok(Record {
        kind,
        instance: ver >> 4,
        container: ver & 15 == 15,
        body: b
            .get(i + 8..i + 8 + len)
            .ok_or("truncated PowerPoint record")?,
    })
}
fn children(mut b: &[u8]) -> impl Iterator<Item = Result<Record<'_>>> {
    std::iter::from_fn(move || {
        if b.is_empty() {
            return None;
        }
        let res = record(b, 0);
        if let Ok(r) = &res {
            b = &b[8 + r.body.len()..]
        } else {
            b = &[]
        }
        Some(res)
    })
}
fn ppt_title(r: &Record) -> bool {
    matches!(u32le(r.body, 0), Ok(0 | 6))
}

fn ppt_text(r: &Record, sec: &mut Section, title: bool) -> Result<()> {
    let s = if r.kind == 4000 {
        utf16(r.body)?
    } else {
        r.body.iter().map(|b| char::from(*b)).collect()
    };
    let first = sec.blocks.len();
    for line in s.split('\r') {
        sec.paragraph(line.replace('\u{b}', "\n"))
    }
    if title && let Some(Block::Paragraph { heading, .. }) = sec.blocks.get_mut(first) {
        *heading = 1;
    }
    Ok(())
}
fn shape_text(b: &[u8], sec: &mut Section, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err("PPT record depth limit exceeded".into());
    }
    let mut title = false;
    for r in children(b) {
        let r = r?;
        if r.kind == 3999 {
            title = ppt_title(&r);
        } else if matches!(r.kind, 4000 | 4008) {
            ppt_text(&r, sec, std::mem::take(&mut title))?
        } else if r.container || r.kind == 0xf00d {
            shape_text(r.body, sec, depth + 1)?
        }
    }
    Ok(())
}
fn ppt(ole: &mut Ole, range: Option<&SectionRange>) -> Result<Document> {
    let data = stream(ole, "/PowerPoint Document")?;
    let user = stream(ole, "/Current User")?;
    let cu = record(&user, 0)?;
    if cu.kind != 4086 {
        return Err("invalid PowerPoint Current User atom".into());
    }
    if u32le(cu.body, 4)? == 0xf3d1c4df {
        return Err("encrypted PPT is unsupported".into());
    }
    let mut edit = u32le(cu.body, 8)? as usize;
    let mut ids = HashMap::new();
    let mut seen = HashSet::new();
    let mut document = None;
    loop {
        if !seen.insert(edit) || seen.len() > 4096 {
            return Err("invalid PPT edit history".into());
        }
        let e = record(&data, edit)?;
        if e.kind != 4085 {
            return Err("invalid PowerPoint user edit".into());
        }
        document.get_or_insert(u32le(e.body, 16)?);
        let dir = record(&data, u32le(e.body, 12)? as usize)?;
        if !matches!(dir.kind, 6001 | 6002) {
            return Err("invalid PPT persistence directory".into());
        }
        let mut i = 0;
        while i < dir.body.len() {
            let h = u32le(dir.body, i)?;
            i += 4;
            for n in 0..(h >> 20) {
                ids.entry((h & 0xfffff) + n)
                    .or_insert(u32le(dir.body, i)? as usize);
                i += 4
            }
        }
        edit = u32le(e.body, 8)? as usize;
        if edit == 0 {
            break;
        }
    }
    let d = record(
        &data,
        *ids.get(&document.unwrap_or(0))
            .ok_or("active PPT document missing")?,
    )?;
    if d.kind != 1000 {
        return Err("invalid PPT document record".into());
    }
    let mut doc = Document::new("ppt");
    let mut slides = Vec::new();
    for r in children(d.body) {
        let r = r?;
        if r.kind == 4080 && r.instance == 0 {
            let mut title = false;
            for item in children(r.body) {
                let item = item?;
                if item.kind == 1011 {
                    title = false;
                    let id = u32le(item.body, 0)?;
                    slides.push((id, Section::new("slide", format!("persist:{id}"))))
                } else if item.kind == 3999 {
                    title = ppt_title(&item);
                } else if matches!(item.kind, 4000 | 4008)
                    && range.is_none_or(|r| r.contains(slides.len()))
                    && let Some((_, s)) = slides.last_mut()
                {
                    ppt_text(&item, s, std::mem::take(&mut title))?
                }
            }
        }
    }
    for i in section_indices(range, slides.len())? {
        let (id, sec) = &mut slides[i];
        let mut sec = std::mem::take(sec).indexed(i);
        if let Some(p) = ids.get(id) {
            let r = record(&data, *p)?;
            let mut shape = Section::default();
            shape_text(r.body, &mut shape, 0)?;
            let mut text = HashSet::new();
            for b in &sec.blocks {
                if let Block::Paragraph { runs, .. } = b {
                    text.insert(runs.iter().map(|r| r.text.as_str()).collect::<String>());
                }
            }
            for b in shape.blocks {
                if let Block::Paragraph { runs, .. } = &b
                    && !text.contains(&runs.iter().map(|r| r.text.as_str()).collect::<String>())
                {
                    sec.blocks.push(b)
                }
            }
        }
        doc.sections.push(sec)
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_paragraphs_preserve_builtin_and_direct_outline_levels() {
        let mut section = Section::default();
        for (style, outline, title) in [(2, 0, "Built-in"), (12, 4, "Direct"), (12, 9, "Body")] {
            let mut pap = WordPap::default();
            word_pap(&record(&[style, 0, 0x40, 0x26, outline]), &[], &mut pap);
            word_paragraph(&mut section, title.into(), Some(&pap));
        }
        let mut doc = Document::new("doc");
        doc.sections.push(section);
        let md = crate::render::render(&doc, "md", "semantic", &Properties::new()).unwrap();
        assert_eq!(md, "## Built-in\n\n##### Direct\n\nBody\n\n");
    }

    #[test]
    fn powerpoint_title_text_records_become_headings() {
        let mut content = Vec::new();
        for (kind, data) in [
            (3999u16, &6u32.to_le_bytes()[..]),
            (4008, b"PPT title"),
            (3999, &1u32.to_le_bytes()[..]),
            (4008, b"Body"),
        ] {
            content.extend([0, 0]);
            content.extend(kind.to_le_bytes());
            content.extend((data.len() as u32).to_le_bytes());
            content.extend(data);
        }
        let mut section = Section::default();
        shape_text(&content, &mut section, 0).unwrap();
        let mut doc = Document::new("ppt");
        doc.sections.push(section);
        let md = crate::render::render(&doc, "md", "semantic", &Properties::new()).unwrap();
        assert_eq!(md, "# PPT title\n\nBody\n\n");
    }

    /// One property record with a style index and the given modifiers, as a
    /// property page stores it.
    fn record(grpprl: &[u8]) -> Vec<u8> {
        assert!(grpprl.len() % 2 == 1, "records hold 2×cb-1 bytes");
        let mut out = vec![grpprl.len().div_ceil(2) as u8];
        out.extend_from_slice(grpprl);
        out
    }

    /// `sprmTDefTable` with the column boundaries and the merge flags of a row.
    fn tdef(bounds: &[i16], flags: &[u16]) -> Vec<u8> {
        let mut operand = vec![flags.len() as u8];
        for b in bounds {
            operand.extend_from_slice(&b.to_le_bytes());
        }
        for f in flags {
            operand.extend_from_slice(&f.to_le_bytes());
            operand.extend_from_slice(&[0; 18]);
        }
        let mut sprm = vec![0x08, 0xd6];
        sprm.extend_from_slice(&((operand.len() + 1) as u16).to_le_bytes());
        sprm.extend_from_slice(&operand);
        sprm
    }

    fn table_row(bounds: &[i16], flags: &[u16]) -> Vec<u8> {
        let mut grpprl = vec![0, 0]; // style index
        grpprl.extend_from_slice(&[0x16, 0x24, 1]); // sprmPFInTable
        grpprl.extend_from_slice(&[0x17, 0x24, 1]); // sprmPFTtp
        grpprl.extend_from_slice(&[0x49, 0x66, 1, 0, 0, 0]); // sprmPItap
        grpprl.extend_from_slice(&tdef(bounds, flags));
        record(&grpprl)
    }

    #[test]
    fn word_papx_reads_the_cell_definition_of_a_row() {
        let mut pap = WordPap::default();
        word_pap(&table_row(&[0, 1000, 3000], &[0, 2]), &[], &mut pap);
        assert!(pap.in_table && pap.ttp);
        assert_eq!(pap.itap, 1);
        let tap = pap.tap.expect("row definition");
        assert_eq!(tap.bounds, vec![0., 50., 150.]);
        assert_eq!(tap.cells.len(), 2);
        assert!(tap.cells.iter().all(|c| !c.merged));
    }

    #[test]
    fn word_papx_reads_merged_cells_and_widths() {
        // The third column carries no cell of its own: it belongs to the cell
        // before it, so the row has two cells over three grid columns.
        let mut pap = WordPap::default();
        word_pap(
            &table_row(&[0, 1000, 2000, 4000], &[0, 3, 1]),
            &[],
            &mut pap,
        );
        let tap = pap.tap.expect("row definition");
        assert_eq!(tap.groups(2), Some(vec![(0, 1), (1, 2)]));
        assert_eq!(tap.groups(3), None);
    }

    #[test]
    fn word_tables_keep_merged_cells_and_column_widths() {
        let mut tables = WordTables {
            ttp: true,
            ..WordTables::default()
        };
        let cell = WordPap {
            in_table: true,
            itap: 1,
            ..WordPap::default()
        };
        let mut mark = WordPap {
            in_table: true,
            itap: 1,
            ttp: true,
            ..WordPap::default()
        };
        mark.tap = Some(WordTap {
            bounds: vec![0., 50., 100., 200.],
            cells: vec![
                WordTc::default(),
                WordTc::default(),
                WordTc {
                    merged: true,
                    ..WordTc::default()
                },
            ],
        });
        for c in "가\u{7}나\u{7}".chars() {
            assert!(tables.enter(&cell));
            tables.push(c, Some(&cell));
        }
        assert!(tables.enter(&mark));
        tables.push('\u{7}', Some(&mark));
        let blocks = tables.finish();
        let [Block::Table { rows }] = blocks.as_slice() else {
            panic!("expected one table: {blocks:?}");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].runs[0].text, "가");
        assert_eq!(rows[0][1].runs[0].text, "나");
        assert_eq!(rows[0][0].properties.get("width").unwrap(), "50pt");
        assert_eq!(rows[0][1].colspan, Some(2));
        // The merged cell covers the two columns after the first boundary.
        assert_eq!(rows[0][1].properties.get("width").unwrap(), "150pt");
    }

    #[test]
    fn word_tables_fold_vertically_merged_cells() {
        let mut tables = WordTables {
            ttp: true,
            ..WordTables::default()
        };
        let cell = WordPap {
            in_table: true,
            itap: 1,
            ..WordPap::default()
        };
        let row = |flags: u16| {
            let mut pap = WordPap {
                in_table: true,
                itap: 1,
                ttp: true,
                ..WordPap::default()
            };
            pap.tap = Some(WordTap {
                bounds: vec![0., 100.],
                cells: vec![WordTc {
                    vert_merge: flags & 1 != 0,
                    vert_restart: flags & 2 != 0,
                    ..WordTc::default()
                }],
            });
            pap
        };
        let (first, second) = (row(2), row(1));
        for c in "위\u{7}".chars() {
            tables.enter(&cell);
            tables.push(c, Some(&cell));
        }
        tables.enter(&first);
        tables.push('\u{7}', Some(&first));
        for c in "아래\u{7}".chars() {
            tables.enter(&cell);
            tables.push(c, Some(&cell));
        }
        tables.enter(&second);
        tables.push('\u{7}', Some(&second));
        let blocks = tables.finish();
        let [Block::Table { rows }] = blocks.as_slice() else {
            panic!("expected one table: {blocks:?}");
        };
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[0][0].runs[0].text, "위");
        assert_eq!(rows[0][0].rowspan, Some(2));
    }

    #[test]
    fn word_tables_place_a_nested_table_in_its_cell() {
        let mut tables = WordTables {
            ttp: true,
            ..WordTables::default()
        };
        let outer = WordPap {
            in_table: true,
            itap: 1,
            ..WordPap::default()
        };
        let inner = WordPap {
            in_table: true,
            itap: 2,
            ..WordPap::default()
        };
        let mut end = WordPap {
            in_table: true,
            itap: 2,
            inner_ttp: true,
            ..WordPap::default()
        };
        end.tap = Some(WordTap {
            bounds: vec![0., 100.],
            cells: vec![WordTc::default()],
        });
        let mut mark = WordPap {
            in_table: true,
            itap: 1,
            ttp: true,
            ..WordPap::default()
        };
        mark.tap = Some(WordTap {
            bounds: vec![0., 100.],
            cells: vec![WordTc::default()],
        });
        for c in "바깥".chars() {
            tables.enter(&outer);
            tables.push(c, Some(&outer));
        }
        for c in "안\u{7}".chars() {
            tables.enter(&inner);
            tables.push(c, Some(&inner));
        }
        tables.enter(&end);
        tables.push('\r', Some(&end));
        tables.enter(&outer);
        tables.push('\u{7}', Some(&outer));
        tables.enter(&mark);
        tables.push('\u{7}', Some(&mark));
        let blocks = tables.finish();
        let [Block::Table { rows }] = blocks.as_slice() else {
            panic!("expected one table: {blocks:?}");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[0][0].runs[0].text, "바깥");
        let [Block::Table { rows: nested }] = rows[0][0].blocks.as_slice() else {
            panic!("expected a nested table: {:?}", rows[0][0].blocks);
        };
        assert_eq!(nested[0][0].runs[0].text, "안");
    }

    #[test]
    fn word_without_row_marks_splits_at_a_repeated_cell_mark() {
        let mut tables = WordTables::default();
        let cell = WordPap {
            in_table: true,
            itap: 1,
            ..WordPap::default()
        };
        for c in "하나\u{7}둘\u{7}\u{7}".chars() {
            tables.enter(&cell);
            tables.push(c, Some(&cell));
        }
        let blocks = tables.finish();
        let [Block::Table { rows }] = blocks.as_slice() else {
            panic!("expected one table: {blocks:?}");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][1].runs[0].text, "둘");
    }
}
