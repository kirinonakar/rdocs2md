use crate::{formatting, model::*};
use std::{
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
pub fn parse(data: &[u8], images: bool) -> Result<Document> {
    // The CFB reader validates allocation chains even in compatibility mode.
    // Some PowerPoint producers leave directory name terminators uninitialized.
    let mut ole = cfb::CompoundFile::open(Cursor::new(data))?;
    if ole.is_stream("/EncryptedPackage") || ole.is_stream("/EncryptionInfo") {
        return Err("encrypted Office package is unsupported".into());
    }
    if ole.is_stream("/FileHeader") {
        hwp(&mut ole, images)
    } else if ole.is_stream("/WordDocument") {
        word(&mut ole)
    } else if ole.is_stream("/PowerPoint Document") {
        ppt(&mut ole)
    } else if ole.is_stream("/Workbook") {
        crate::xls::parse(&stream(&mut ole, "/Workbook")?)
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
    alignments: Vec<Option<&'static str>>,
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
                25 if styles.alignments.len() <= u16::MAX as usize => {
                    styles
                        .alignments
                        .push(
                            record
                                .data
                                .first()
                                .and_then(|flags| match (flags >> 2) & 7 {
                                    0 | 4 | 5 => Some("justify"),
                                    1 => Some("left"),
                                    2 => Some("right"),
                                    3 => Some("center"),
                                    _ => None,
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
                    if let Block::Paragraph { properties, .. } = &mut block
                        && let Ok(index) = u16le(record.data, 8)
                        && let Some(Some(align)) = styles.alignments.get(usize::from(index))
                    {
                        properties.insert("align".into(), (*align).into());
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
        if total > FILE_LIMIT {
            return Err("HWP expansion limit exceeded".into());
        }
        if image_ext(&bytes).is_some() {
            doc.assets.insert(key, bytes);
        }
    }
    Ok(())
}

fn hwp(ole: &mut Ole, images: bool) -> Result<Document> {
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
    for (_, path) in files {
        let raw = hwp_stream(ole, &path, compressed)?;
        total += raw.len();
        if total > FILE_LIMIT {
            return Err("HWP expansion limit exceeded".into());
        }
        let mut sec = Section::new("section", &path);
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
fn word(ole: &mut Ole) -> Result<Document> {
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
    if u16le(&data, p)? < 34 {
        return Err("missing Word piece table".into());
    }
    let clx = p + 2 + 33 * 8;
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
    let mut text = String::new();
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
        if compressed {
            let b = data
                .get(pos / 2..pos / 2 + chars)
                .ok_or("Word piece out of bounds")?;
            text.push_str(&encoding_rs::WINDOWS_1252.decode(b).0)
        } else {
            text.push_str(&utf16(
                data.get(pos..pos + 2 * chars)
                    .ok_or("Word Unicode piece out of bounds")?,
            )?)
        }
        if text.len() > PART_LIMIT {
            return Err("Word text exceeds limit".into());
        }
    }
    let mut doc = Document::new("doc");
    let mut sec = Section::new("section", "WordDocument");
    let mut line = String::new();
    let mut fields = Vec::new();
    for c in text.chars() {
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
            '\r' | '\u{7}' => sec.paragraph(std::mem::take(&mut line)),
            '\u{c}' => {
                sec.paragraph(std::mem::take(&mut line));
                doc.sections.push(sec);
                sec = Section::new("section", "WordDocument")
            }
            '\u{b}' => line.push('\n'),
            '\t' => line.push('\t'),
            _ if !c.is_control() => line.push(c),
            _ => {}
        }
    }
    sec.paragraph(line);
    doc.sections.push(sec);
    Ok(doc)
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
fn ppt_text(r: &Record, sec: &mut Section) -> Result<()> {
    let s = if r.kind == 4000 {
        utf16(r.body)?
    } else {
        r.body.iter().map(|b| char::from(*b)).collect()
    };
    for line in s.split('\r') {
        sec.paragraph(line.replace('\u{b}', "\n"))
    }
    Ok(())
}
fn shape_text(b: &[u8], sec: &mut Section, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err("PPT record depth limit exceeded".into());
    }
    for r in children(b) {
        let r = r?;
        if matches!(r.kind, 4000 | 4008) {
            ppt_text(&r, sec)?
        } else if r.container || r.kind == 0xf00d {
            shape_text(r.body, sec, depth + 1)?
        }
    }
    Ok(())
}
fn ppt(ole: &mut Ole) -> Result<Document> {
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
            for item in children(r.body) {
                let item = item?;
                if item.kind == 1011 {
                    let id = u32le(item.body, 0)?;
                    slides.push((id, Section::new("slide", format!("persist:{id}"))))
                } else if matches!(item.kind, 4000 | 4008)
                    && let Some((_, s)) = slides.last_mut()
                {
                    ppt_text(&item, s)?
                }
            }
        }
    }
    for (id, mut sec) in slides {
        if let Some(p) = ids.get(&id) {
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
