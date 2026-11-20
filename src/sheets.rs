use crate::{model::*, package::*};
use std::collections::{HashMap, HashSet};
pub fn address(s: &str) -> Result<(usize, usize)> {
    let cut = s
        .find(|c: char| c.is_ascii_digit())
        .ok_or("invalid cell address")?;
    let mut col = 0usize;
    for c in s[..cut].bytes() {
        if !c.is_ascii_uppercase() {
            return Err("invalid cell column".into());
        }
        col = col
            .checked_mul(26)
            .and_then(|n| n.checked_add((c - b'A' + 1) as usize))
            .ok_or("invalid column")?;
    }
    let row = s[cut..].parse::<usize>()?;
    if !(1..=16384).contains(&col) || !(1..=1048576).contains(&row) {
        return Err("cell address out of range".into());
    }
    Ok((row - 1, col - 1))
}
pub struct Grid {
    covered: HashSet<(usize, usize)>,
    rows: Vec<Vec<Cell>>,
    start: usize,
    last: Option<usize>,
    cells: usize,
}
impl Grid {
    pub fn new() -> Self {
        Self {
            covered: HashSet::new(),
            rows: vec![],
            start: 0,
            last: None,
            cells: 0,
        }
    }
    pub fn add(&mut self, r: usize, c: usize, v: Cell, sec: &mut Section) -> Result<()> {
        if self.last.is_some_and(|prev| r > prev + 1) {
            self.flush(sec)
        }
        if self.rows.is_empty() {
            self.start = r
        }
        if r < self.start || c >= 16384 {
            return Err("invalid worksheet coordinates".into());
        }
        let idx = r - self.start;
        if idx >= CELL_LIMIT {
            return Err("row limit exceeded".into());
        }
        if self.rows.len() <= idx {
            self.rows.resize_with(idx + 1, Vec::new)
        }
        let row = &mut self.rows[idx];
        if row.len() <= c {
            self.cells += c + 1 - row.len();
            if self.cells > CELL_LIMIT {
                return Err("spreadsheet expansion limit exceeded".into());
            }
            row.resize_with(c + 1, Cell::default)
        }
        row[c] = v;
        self.last = Some(r);
        Ok(())
    }
    pub fn flush(&mut self, sec: &mut Section) {
        if !self.rows.is_empty() {
            if !self.covered.is_empty() {
                for (i, row) in self.rows.iter_mut().enumerate() {
                    let mut col = 0;
                    row.retain(|_| {
                        let keep = !self.covered.contains(&(self.start + i, col));
                        col += 1;
                        keep
                    })
                }
            }
            sec.blocks.push(Block::Table {
                rows: std::mem::take(&mut self.rows),
            })
        }
        self.last = None;
    }
}
pub fn xlsx(
    pkg: &mut Package,
    mut doc: Document,
    root: N,
    rels: &HashMap<String, String>,
    types: &HashMap<String, String>,
) -> Result<Document> {
    if tag(root) != "workbook" {
        return Err("invalid Excel workbook".into());
    }
    let mut shared = Vec::new();
    if let Some(path) = types.get("sharedStrings") {
        let s = pkg.text(path)?;
        let x = xml(&s)?;
        for si in desc(x.root(), "si") {
            shared.push(
                desc(si, "t")
                    .filter(|t| !t.ancestors().any(|n| tag(n) == "rPh"))
                    .map(all_text)
                    .collect::<String>(),
            )
        }
    }
    let mut fmts = HashMap::new();
    let mut xf = Vec::new();
    if let Some(path) = types.get("styles") {
        let s = pkg.text(path)?;
        let x = xml(&s)?;
        for n in desc(x.root(), "numFmt") {
            if let (Some(k), Some(v)) = (attr(n, "numFmtId"), attr(n, "formatCode")) {
                fmts.insert(k.to_string(), v.to_string());
            }
        }
        if let Some(n) = desc(x.root(), "cellXfs").next() {
            for style in n.children().filter(|n| tag(*n) == "xf") {
                xf.push(attr(style, "numFmtId").unwrap_or("0").to_string())
            }
        }
    }
    let mut text_budget = 0usize;
    let epoch1904 = child(root, "workbookPr")
        .is_some_and(|n| matches!(attr(n, "date1904"), Some("1" | "true")));
    for sheet in desc(root, "sheet") {
        let path = attr(sheet, "id")
            .and_then(|id| rels.get(id))
            .ok_or("missing sheet relationship")?;
        let s = pkg.text(path)?;
        let x = xml(&s)?;
        let mut sec = Section::new("sheet", path);
        sec.title = attr(sheet, "name").unwrap_or("").into();
        if let Some(state) = attr(sheet, "state") {
            sec.properties.insert("state".into(), state.into());
        }
        let parts = crate::office::relationships(pkg, path)?.0;
        let mut links = HashMap::new();
        for link in desc(x.root(), "hyperlink") {
            if let Some(pos) = attr(link, "ref") {
                let to = attr(link, "id")
                    .and_then(|id| parts.get(id))
                    .cloned()
                    .or_else(|| attr(link, "location").map(|v| format!("#{v}")));
                if let Some(to) = to {
                    links.insert(pos, to);
                }
            }
        }
        let mut merges = HashMap::new();
        let mut grid = Grid::new();
        let mut merge_area = 0usize;
        for m in desc(x.root(), "mergeCell") {
            if let Some((a, b)) = attr(m, "ref").and_then(|s| s.split_once(':')) {
                let (ra, ca) = address(a)?;
                let (rb, cb) = address(b)?;
                if rb >= ra && cb >= ca {
                    merge_area =
                        merge_area.saturating_add((rb - ra + 1).saturating_mul(cb - ca + 1));
                    if merge_area > CELL_LIMIT {
                        return Err("worksheet merged area exceeds limit".into());
                    }
                    for r in ra..=rb {
                        for c in ca..=cb {
                            if (r, c) != (ra, ca) {
                                grid.covered.insert((r, c));
                            }
                        }
                    }
                    merges.insert((ra, ca), (rb - ra + 1, cb - ca + 1));
                }
            }
        }
        let mut next_row = 0;
        for row in desc(x.root(), "row") {
            let r = match attr(row, "r") {
                Some(n) => n.parse::<usize>()?.checked_sub(1).ok_or("invalid row")?,
                None => next_row,
            };
            if r >= 1048576 {
                return Err("row out of bounds".into());
            }
            next_row = r + 1;
            let mut next_col = 0;
            for n in row.children().filter(|n| tag(*n) == "c") {
                let pos = attr(n, "r")
                    .map(address)
                    .transpose()?
                    .unwrap_or((r, next_col));
                next_col = pos.1 + 1;
                let raw = child(n, "v").and_then(|v| v.text()).unwrap_or("");
                let typ = attr(n, "t").unwrap_or("n");
                let text = match typ {
                    "s" => shared
                        .get(raw.parse::<usize>()?)
                        .ok_or("shared string out of bounds")?
                        .as_str(),
                    "b" if raw == "1" => "TRUE",
                    "b" if raw == "0" => "FALSE",
                    _ => raw,
                };
                text_budget = text_budget.saturating_add(text.len());
                if text_budget > PART_LIMIT {
                    return Err("spreadsheet text limit exceeded".into());
                }
                let mut c = Cell::text(if typ == "inlineStr" {
                    desc(n, "t").map(all_text).collect::<String>()
                } else {
                    text.into()
                });
                if let Some(f) = child(n, "f") {
                    c.properties.insert("formula".into(), all_text(f));
                    c.properties.insert("cached_value".into(), raw.into());
                }
                if let Some(a) = attr(n, "r") {
                    c.properties.insert("address".into(), a.into());
                    if let Some(l) = links.get(a) {
                        for run in &mut c.runs {
                            run.link = Some(l.clone())
                        }
                    }
                }
                if attr(row, "hidden") == Some("1") {
                    c.properties.insert("hidden".into(), "true".into());
                }
                if let Some(id) = attr(n, "s")
                    .and_then(|v| v.parse::<usize>().ok())
                    .and_then(|i| xf.get(i))
                {
                    if let Ok(num) = raw.parse::<f64>()
                        && let Some(value) = display_number(num, id, epoch1904)
                        && typ == "n"
                    {
                        c.runs = vec![Run::text(value)];
                        c.properties.insert("raw_value".into(), raw.into());
                    }
                    c.properties
                        .insert("number_format".into(), fmts.get(id).unwrap_or(id).clone());
                }
                if let Some((h, w)) = merges.get(&pos) {
                    if *h > 1 {
                        c.rowspan = Some(*h)
                    }
                    if *w > 1 {
                        c.colspan = Some(*w)
                    }
                }
                grid.add(pos.0, pos.1, c, &mut sec)?;
            }
        }
        grid.flush(&mut sec);
        doc.sections.push(sec);
    }
    Ok(doc)
}
fn display_number(value: f64, id: &str, epoch1904: bool) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    let fmt = id.parse::<u16>().ok()?;
    match fmt {
        9 => return Some(format!("{:.0}%", value * 100.)),
        10 => return Some(format!("{:.2}%", value * 100.)),
        14..=22 | 45..=47 => {}
        _ => return None,
    }
    if !(0.0..=2958465.).contains(&value) {
        return None;
    }
    let serial = value.floor() as i64;
    // Convert Excel's date epoch, including the historical 1900 leap-day sentinel.
    let date = if !epoch1904 && serial == 60 {
        "1900-02-29".into()
    } else {
        let days = if epoch1904 {
            serial - 24107
        } else {
            serial - 25569 + i64::from(serial < 60)
        };
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let day = z - era * 146097;
        let y = (day - day / 1460 + day / 36524 - day / 146096) / 365;
        let doy = day - (365 * y + y / 4 - y / 100);
        let m = (5 * doy + 2) / 153;
        let d = doy - (153 * m + 2) / 5 + 1;
        let month = if m < 10 { m + 3 } else { m - 9 };
        let year = y + era * 400 + i64::from(month <= 2);
        format!("{year:04}-{month:02}-{d:02}")
    };
    let seconds = (value.fract() * 86400.).round() as u32 % 86400;
    let time = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    Some(match fmt {
        14..=17 => date,
        22 => format!("{date} {time}"),
        46 => format!(
            "{:02}:{:02}:{:02}",
            (value * 24.).floor() as u64,
            seconds / 60 % 60,
            seconds % 60
        ),
        _ => time,
    })
}
