use crate::{
    legacy::{u16le, u32le, utf16},
    model::*,
    sheets::Grid,
};
fn record(b: &[u8], i: usize) -> Result<(u16, &[u8])> {
    let n = u16le(b, i + 2)? as usize;
    Ok((
        u16le(b, i)?,
        b.get(i + 4..i + 4 + n).ok_or("truncated BIFF record")?,
    ))
}
// BIFF8 changes compression flags at CONTINUE boundaries inside character data only.
struct Segments<'a> {
    parts: Vec<&'a [u8]>,
    segment: usize,
    offset: usize,
}
impl Segments<'_> {
    fn next(&mut self) -> Result<u8> {
        while self
            .parts
            .get(self.segment)
            .is_some_and(|p| self.offset == p.len())
        {
            self.segment += 1;
            self.offset = 0
        }
        let b = *self
            .parts
            .get(self.segment)
            .and_then(|p| p.get(self.offset))
            .ok_or("truncated BIFF string")?;
        self.offset += 1;
        Ok(b)
    }
    fn word(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes([self.next()?, self.next()?]))
    }
    fn long(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes([
            self.next()?,
            self.next()?,
            self.next()?,
            self.next()?,
        ]))
    }
    fn string(&mut self) -> Result<String> {
        let len = self.word()? as usize;
        let flags = self.next()?;
        let mut wide = flags & 1 != 0;
        let runs = if flags & 8 != 0 {
            self.word()? as usize
        } else {
            0
        };
        let ext = if flags & 4 != 0 {
            self.long()? as usize
        } else {
            0
        };
        let mut v = Vec::new();
        for _ in 0..len {
            if self
                .parts
                .get(self.segment)
                .is_some_and(|p| self.offset == p.len())
            {
                wide = self.next()? & 1 != 0
            }
            v.push(if wide {
                self.word()?
            } else {
                self.next()? as u16
            })
        }
        for _ in 0..runs * 4 + ext {
            self.next()?;
        }
        Ok(String::from_utf16_lossy(&v))
    }
}
fn rk(v: i32) -> String {
    let n = if v & 2 != 0 {
        (v >> 2) as f64
    } else {
        f64::from_bits(((v as u32 & !3) as u64) << 32)
    };
    if v & 1 != 0 {
        (n / 100.).to_string()
    } else {
        n.to_string()
    }
}
fn short_string(b: &[u8], n: usize, wide: bool) -> Result<String> {
    let bytes = b
        .get(..n * if wide { 2 } else { 1 })
        .ok_or("truncated BIFF sheet name")?;
    if wide {
        utf16(bytes)
    } else {
        Ok(bytes.iter().map(|c| char::from(*c)).collect())
    }
}
pub fn parse(b: &[u8]) -> Result<Document> {
    let (typ, bof) = record(b, 0)?;
    if typ != 0x809 || u16le(bof, 0)? != 0x600 {
        return Err("only BIFF8 Excel 97–2003 workbooks are supported".into());
    }
    let mut sheets = Vec::new();
    let mut shared = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let (k, v) = record(b, i)?;
        i += v.len() + 4;
        match k {
            0xa => break,
            0x2f => return Err("encrypted XLS workbook is unsupported".into()),
            0x85 if v.get(5) == Some(&0) => {
                let name = short_string(
                    v.get(8..).ok_or("short BoundSheet")?,
                    v[6] as usize,
                    v[7] & 1 != 0,
                )?;
                sheets.push((u32le(v, 0)? as usize, name, v[4]));
            }
            0xfc => {
                let count = u32le(v, 4)? as usize;
                if count > CELL_LIMIT {
                    return Err("BIFF shared string limit exceeded".into());
                }
                let mut parts = vec![&v[8..]];
                while i + 4 <= b.len() {
                    let (k, c) = record(b, i)?;
                    if k != 0x3c {
                        break;
                    }
                    parts.push(c);
                    i += 4 + c.len()
                }
                let mut chunks = Segments {
                    parts,
                    segment: 0,
                    offset: 0,
                };
                for _ in 0..count {
                    shared.push(chunks.string()?);
                }
            }
            _ => {}
        }
    }
    let mut doc = Document::new("xls");
    let mut bytes = 0usize;
    for (mut i, name, state) in sheets {
        let mut s = Section::new("sheet", format!("Workbook:{i}"));
        s.title = name;
        if state != 0 {
            s.properties.insert("hidden".into(), "true".into());
        }
        let mut grid = Grid::new();
        let mut pending = None;
        while i < b.len() {
            let (k, v) = record(b, i)?;
            i += v.len() + 4;
            if k == 0xa {
                break;
            }
            if k == 0x207 {
                if let Some((r, c)) = pending.take() {
                    let mut seg = Segments {
                        parts: vec![v],
                        segment: 0,
                        offset: 0,
                    };
                    let mut cell = Cell::text(seg.string()?);
                    cell.properties
                        .insert("formula".into(), "BIFF expression".into());
                    grid.add(r, c, cell, &mut s)?
                }
                continue;
            }
            if !matches!(k, 0xfd | 0x203 | 0x27e | 0xbd | 0x205 | 0x204 | 6) {
                continue;
            }
            let r = u16le(v, 0)? as usize;
            let c = u16le(v, 2)? as usize;
            let mut props = Properties::new();
            let value = match k {
                0xfd => {
                    let t = shared
                        .get(u32le(v, 6)? as usize)
                        .ok_or("invalid BIFF shared string index")?;
                    bytes += t.len();
                    if bytes > PART_LIMIT {
                        return Err("BIFF shared text expansion limit exceeded".into());
                    }
                    t.clone()
                }
                0x203 => {
                    f64::from_le_bytes(v.get(6..14).ok_or("truncated BIFF number")?.try_into()?)
                        .to_string()
                }
                0x27e => rk(u32le(v, 6)? as i32),
                0xbd => {
                    if v.len() < 12 || (v.len() - 6) % 6 != 0 {
                        return Err("invalid MULRK".into());
                    }
                    for (n, entry) in v[4..v.len() - 2].as_chunks::<6>().0.iter().enumerate() {
                        grid.add(r, c + n, Cell::text(rk(u32le(entry, 2)? as i32)), &mut s)?
                    }
                    continue;
                }
                0x205 => {
                    if *v.get(7).ok_or("truncated BOOLERR")? != 0 {
                        format!("#ERR({})", v[6])
                    } else {
                        (v[6] != 0).to_string().to_uppercase()
                    }
                }
                0x204 => {
                    let mut seg = Segments {
                        parts: vec![v.get(6..).ok_or("truncated BIFF label")?],
                        segment: 0,
                        offset: 0,
                    };
                    seg.string()?
                }
                _ => {
                    let v = v.get(6..14).ok_or("truncated formula cache")?;
                    props.insert("formula".into(), "BIFF expression".into());
                    if v[6..8] == [255, 255] {
                        match v[0] {
                            0 => {
                                pending = Some((r, c));
                                continue;
                            }
                            1 => (v[2] != 0).to_string().to_uppercase(),
                            2 => format!("#ERR({})", v[2]),
                            _ => String::new(),
                        }
                    } else {
                        f64::from_le_bytes(v.try_into()?).to_string()
                    }
                }
            };
            let mut cell = Cell::text(value);
            cell.properties = props;
            grid.add(r, c, cell, &mut s)?;
        }
        grid.flush(&mut s);
        doc.sections.push(s)
    }
    Ok(doc)
}
