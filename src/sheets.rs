use crate::{flow::style_props, formatting, model::*, package::*};
use std::collections::{HashMap, HashSet};

pub(crate) fn record_hidden_axis(sec: &mut Section, key: &str, axis: &SectionRange) {
    if !axis.0.is_empty() {
        let ranges: Vec<_> = axis
            .0
            .iter()
            .map(|&(a, b)| {
                if a == b {
                    a.to_string()
                } else {
                    format!("{a}-{b}")
                }
            })
            .collect();
        sec.properties.insert(key.into(), ranges.join(","));
    }
}

pub(crate) fn hide_cell_axis(cell: &mut Cell, key: &str) {
    cell.properties.insert(key.into(), "true".into());
    cell.properties.insert("hidden".into(), "true".into());
}

pub(crate) fn ods_axis_hidden(axis: N, sheet: N) -> bool {
    matches!(attr(axis, "visibility"), Some("collapse" | "filter"))
        || axis
            .ancestors()
            .skip(1)
            .take_while(|&n| n != sheet)
            .any(|n| {
                matches!(tag(n), "table-column-group" | "table-row-group")
                    && matches!(attr(n, "display"), Some("false" | "0"))
            })
}

fn address_label(r: usize, mut c: usize) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push(char::from(b'A' + (c % 26) as u8));
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    let column: String = letters.into_iter().rev().collect();
    format!("{column}{}", r + 1)
}
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
    pub hidden_rows: SectionRange,
    pub hidden_columns: SectionRange,
}
impl Grid {
    pub fn new() -> Self {
        Self {
            covered: HashSet::new(),
            rows: vec![],
            start: 0,
            last: None,
            cells: 0,
            hidden_rows: SectionRange::default(),
            hidden_columns: SectionRange::default(),
        }
    }
    pub fn add(&mut self, r: usize, c: usize, mut v: Cell, sec: &mut Section) -> Result<()> {
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
        v.properties
            .entry("address".into())
            .or_insert_with(|| address_label(r, c));
        if self.hidden_rows.contains(r + 1) {
            hide_cell_axis(&mut v, "hidden_row");
        }
        if self.hidden_columns.contains(c + 1) {
            hide_cell_axis(&mut v, "hidden_column");
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

#[derive(Default)]
struct XlsxStyle {
    number_format: String,
    cell: Properties,
    font: Style,
}

fn xlsx_color(node: N) -> Option<String> {
    let value = attr(node, "rgb")?;
    formatting::color(if value.len() == 8 {
        value.get(2..)?
    } else {
        value
    })
}

fn xlsx_styles(root: N) -> Vec<XlsxStyle> {
    let entries = |group, name| {
        child(root, group)
            .into_iter()
            .flat_map(move |n| n.children().filter(move |c| tag(*c) == name))
    };
    let borders: Vec<_> = entries("borders", "border")
        .map(|node| {
            let mut props = Properties::new();
            for (side, key) in formatting::SIDES.into_iter().zip(formatting::BORDERS) {
                let Some(edge) = child(node, side) else {
                    continue;
                };
                let (width, style) = match attr(edge, "style") {
                    Some("double") => ("3px", "double"),
                    Some("medium") => ("2px", "solid"),
                    Some("thick") => ("3px", "solid"),
                    Some(
                        "mediumDashed" | "mediumDashDot" | "mediumDashDotDot" | "slantDashDot",
                    ) => ("2px", "dashed"),
                    Some("dashed" | "dashDot" | "dashDotDot") => ("1px", "dashed"),
                    Some("dotted" | "hair") => ("1px", "dotted"),
                    Some("thin") => ("1px", "solid"),
                    _ => continue,
                };
                let shade = child(edge, "color").and_then(xlsx_color);
                props.insert(
                    key.into(),
                    formatting::border(style, width, shade.as_deref()),
                );
            }
            props
        })
        .collect();
    let fills: Vec<_> = entries("fills", "fill")
        .map(|n| {
            child(n, "patternFill")
                .filter(|p| attr(*p, "patternType") == Some("solid"))
                .and_then(|p| child(p, "fgColor"))
                .and_then(xlsx_color)
        })
        .collect();
    let fonts: Vec<_> = entries("fonts", "font")
        .map(|n| {
            let mut font = Style::default();
            style_props(n, &mut font);
            font.font = child(n, "name")
                .and_then(|n| attr(n, "val"))
                .map(str::to_owned);
            font.size = formatting::units(child(n, "sz").and_then(|n| attr(n, "val")), 1.)
                .map(|n| format!("{n}pt"));
            font.color = child(n, "color").and_then(xlsx_color);
            font
        })
        .collect();
    entries("cellXfs", "xf")
        .map(|node| {
            let index = |key| attr(node, key).and_then(|v| v.parse::<usize>().ok());
            let mut style = XlsxStyle {
                number_format: attr(node, "numFmtId").unwrap_or("0").into(),
                cell: index("borderId")
                    .and_then(|i| borders.get(i))
                    .cloned()
                    .unwrap_or_default(),
                font: index("fontId")
                    .and_then(|i| fonts.get(i))
                    .cloned()
                    .unwrap_or_default(),
            };
            if let Some(shade) = index("fillId")
                .and_then(|i| fills.get(i))
                .and_then(Option::as_ref)
            {
                style.cell.insert("background-color".into(), shade.clone());
            }
            if let Some(align) = child(node, "alignment") {
                for (source, key) in [("horizontal", "text-align"), ("vertical", "vertical-align")]
                {
                    if let Some(v) = attr(align, source) {
                        formatting::box_property(&mut style.cell, key, v);
                    }
                }
                if matches!(attr(align, "wrapText"), Some("1" | "true")) {
                    style.cell.insert("white-space".into(), "pre-wrap".into());
                }
            }
            style
        })
        .collect()
}

pub fn xlsx(
    pkg: &mut Package,
    mut doc: Document,
    root: N,
    rels: &HashMap<String, String>,
    types: &HashMap<String, String>,
    range: Option<&SectionRange>,
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
        if let Some(root) = x.root().first_element_child() {
            xf = xlsx_styles(root);
        }
    }
    let mut text_budget = 0usize;
    let epoch1904 = child(root, "workbookPr")
        .is_some_and(|n| matches!(attr(n, "date1904"), Some("1" | "true")));
    let sheets: Vec<_> = desc(root, "sheet").collect();
    for i in section_indices(range, sheets.len())? {
        let sheet = sheets[i];
        let path = attr(sheet, "id")
            .and_then(|id| rels.get(id))
            .ok_or("missing sheet relationship")?;
        let s = pkg.text(path)?;
        let x = xml(&s)?;
        let mut sec = Section::new("sheet", path).indexed(i);
        sec.title = attr(sheet, "name").unwrap_or("").into();
        if let Some(state) = attr(sheet, "state") {
            sec.properties.insert("state".into(), state.into());
            if matches!(state, "hidden" | "veryHidden") {
                sec.properties.insert("hidden".into(), "true".into());
            }
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
        let mut hidden_columns = Vec::new();
        for col in
            desc(x.root(), "col").filter(|n| matches!(attr(*n, "hidden"), Some("1" | "true")))
        {
            let start = attr(col, "min")
                .ok_or("hidden column range start missing")?
                .parse::<usize>()?;
            let end = attr(col, "max")
                .ok_or("hidden column range end missing")?
                .parse::<usize>()?;
            if start == 0 || start > end || end > 16384 {
                return Err("hidden column range out of bounds".into());
            }
            hidden_columns.push((start, end));
        }
        grid.hidden_columns = SectionRange::from_spans(hidden_columns);
        record_hidden_axis(&mut sec, "hidden_columns", &grid.hidden_columns);
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
        let mut hidden_rows = Vec::new();
        for row in desc(x.root(), "row") {
            let r = match attr(row, "r") {
                Some(n) => n.parse::<usize>()?.checked_sub(1).ok_or("invalid row")?,
                None => next_row,
            };
            if r >= 1048576 {
                return Err("row out of bounds".into());
            }
            next_row = r + 1;
            let hidden = matches!(attr(row, "hidden"), Some("1" | "true"));
            if hidden {
                hidden_rows.push((r + 1, r + 1));
            }
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
                if hidden {
                    hide_cell_axis(&mut c, "hidden_row");
                }
                if let Some(style) = attr(n, "s")
                    .and_then(|v| v.parse::<usize>().ok())
                    .and_then(|i| xf.get(i))
                {
                    let id = &style.number_format;
                    if let Ok(num) = raw.parse::<f64>()
                        && let Some(value) = display_number(num, id, epoch1904)
                        && typ == "n"
                    {
                        c.runs[0].text = value;
                        c.properties.insert("raw_value".into(), raw.into());
                    }
                    c.properties.extend(style.cell.clone());
                    c.runs[0].style = style.font.clone();
                    c.properties
                        .insert("number_format".into(), fmts.get(id).unwrap_or(id).clone());
                } else if let Some(style) = xf.first() {
                    c.properties.extend(style.cell.clone());
                    c.runs[0].style = style.font.clone();
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
        record_hidden_axis(
            &mut sec,
            "hidden_rows",
            &SectionRange::from_spans(hidden_rows),
        );
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
