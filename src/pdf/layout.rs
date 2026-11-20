use crate::model::{Block, Cell, Properties, Run, Section, push_run};
use std::collections::BTreeMap;

pub(super) struct Fragment {
    pub x: f32,
    pub end: f32,
    pub y: f32,
    pub size: f32,
    pub horizontal: bool,
    pub runs: Vec<Run>,
}

impl Fragment {
    fn first_text(&self) -> &str {
        self.runs.first().map_or("", |r| r.text.as_str())
    }

    fn last_text(&self) -> &str {
        self.runs.last().map_or("", |r| r.text.as_str())
    }

    fn styled(&self) -> bool {
        self.runs
            .iter()
            .filter(|r| !r.text.trim().is_empty())
            .all(|r| r.style.bold)
    }
}

pub(super) fn approximate_width(text: &str) -> f32 {
    text.chars()
        .map(|c| match c {
            ' ' | '\t' => 0.25,
            'i' | 'l' | 'I' | 'j' | '!' | '.' | ',' | ':' | ';' | '\'' => 0.26,
            'm' | 'w' | 'M' | 'W' => 0.82,
            'ﬀ' | 'ﬁ' | 'ﬂ' => 0.55,
            'ﬃ' | 'ﬄ' => 0.8,
            c if c.is_ascii_uppercase() => 0.66,
            c if is_cjk(c) => 1.,
            _ => 0.5,
        })
        .sum()
}

fn is_cjk(c: char) -> bool {
    joins_without_space(c) || matches!(c, '\u{ac00}'..='\u{d7af}')
}

fn joins_without_space(c: char) -> bool {
    matches!(c, '\u{2e80}'..='\u{9fff}' | '\u{20000}'..='\u{323af}')
}

fn coordinates(x: f32, y: f32) -> Properties {
    Properties::from([
        ("x_pt".into(), x.to_string()),
        ("y_pt".into(), y.to_string()),
    ])
}

fn join_runs(target: &mut Vec<Run>, incoming: Vec<Run>, space: bool) {
    if space
        && target
            .last()
            .is_some_and(|r| !r.text.ends_with(char::is_whitespace))
        && incoming
            .first()
            .is_some_and(|r| !r.text.starts_with(char::is_whitespace))
    {
        let separator = target
            .last()
            .map(|r| Run {
                text: " ".into(),
                style: r.style.clone(),
                ..Run::default()
            })
            .unwrap_or_else(|| Run::text(" "));
        push_run(target, separator);
    }
    for run in incoming {
        push_run(target, run);
    }
}

fn attach_raised_text(pieces: &mut [Fragment]) {
    // Small superscripts and large drop caps can have a baseline on a different row.
    let mut attachments = Vec::new();
    for (i, f) in pieces.iter().enumerate() {
        if !f.horizontal || f.runs.len() != 1 {
            continue;
        }
        let text = f.first_text().trim();
        let drop_cap =
            f.size >= 20. && text.chars().count() == 1 && text.starts_with(char::is_uppercase);
        let note = text.len() <= 3 && !text.is_empty() && text.bytes().all(|c| c.is_ascii_digit());
        if !drop_cap && !note {
            continue;
        }
        let reach = if drop_cap { f.size } else { f.size * 1.6 };
        let start = pieces.partition_point(|p| p.y > f.y + reach);
        let end = pieces.partition_point(|p| p.y >= f.y - reach);
        let candidate = pieces[start..end]
            .iter()
            .take(256)
            .filter(|p| p.horizontal)
            .filter(|p| {
                if drop_cap {
                    let gap = p.x - f.end;
                    p.size < f.size * 0.6
                        && (f.size * 0.25..=f.size * 0.92).contains(&(p.y - f.y))
                        && (-f.size * 0.16..=f.size * 0.25).contains(&gap)
                        && p.first_text().trim().len() > 3
                } else {
                    let gap = f.x - p.end;
                    p.size >= f.size * 1.25
                        && (p.size * 0.14..=p.size * 0.65).contains(&(f.y - p.y))
                        && (-f.size * 0.2..=f.size * 0.6).contains(&gap)
                }
            })
            .max_by(|a, b| a.y.total_cmp(&b.y));
        if let Some(p) = candidate {
            attachments.push((i, p.y, p.size, drop_cap, p.x));
        }
    }
    for (i, y, size, drop_cap, next_x) in attachments {
        let f = &mut pieces[i];
        f.y = y;
        f.size = size;
        if drop_cap {
            f.end = next_x;
        } else {
            f.runs[0].style.script = Some("sup".into());
        }
    }
}

enum Content {
    Line {
        runs: Vec<Run>,
        size: f32,
    },
    Standalone(Vec<Run>),
    Table {
        prefix: Vec<Item>,
        rows: Vec<Vec<Cell>>,
    },
}

struct Item {
    x: f32,
    end: f32,
    y: f32,
    content: Content,
}

impl Item {
    fn line_size(&self) -> Option<f32> {
        match self.content {
            Content::Line { size, .. } => Some(size),
            _ => None,
        }
    }
}

fn as_item(f: Fragment) -> Item {
    Item {
        x: f.x,
        end: f.end,
        y: f.y,
        content: if f.horizontal && f.size > 0. {
            Content::Line {
                runs: f.runs,
                size: f.size,
            }
        } else {
            Content::Standalone(f.runs)
        },
    }
}

fn baseline_bands(mut fragments: Vec<Fragment>) -> Vec<Vec<Fragment>> {
    fragments.sort_by(|a, b| b.y.total_cmp(&a.y));
    attach_raised_text(&mut fragments);
    fragments.sort_by(|a, b| b.y.total_cmp(&a.y).then_with(|| a.x.total_cmp(&b.x)));
    let mut bands: Vec<Vec<Fragment>> = Vec::new();
    for f in fragments {
        if bands
            .last()
            .and_then(|b| b.first())
            .is_none_or(|first| first.y - f.y > 1.15)
        {
            bands.push(Vec::new());
        }
        bands.last_mut().unwrap().push(f);
    }
    for band in &mut bands {
        band.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut lines: Vec<Fragment> = Vec::new();
        for f in std::mem::take(band) {
            if let Some(prev) = lines.last_mut()
                && prev.horizontal
                && f.horizontal
                && f.x - prev.end <= prev.size.min(f.size) * 0.65
            {
                let space = f.x - prev.end > prev.size.min(f.size) * 0.13
                    && !f.first_text().starts_with([',', '.', ':', ';', ')', ']'])
                    && !f.runs.first().is_some_and(|r| r.style.script.is_some());
                prev.end = prev.end.max(f.end);
                join_runs(&mut prev.runs, f.runs, space);
            } else {
                lines.push(f);
            }
        }
        *band = lines;
    }
    bands
}

fn attach_labels(bands: &mut [Vec<Fragment>]) {
    for i in 0..bands.len() {
        let (above, rest) = bands.split_at_mut(i);
        let band = &mut rest[0];
        let mut j = 0;
        while j + 1 < band.len() {
            let label = &band[j];
            let body = &band[j + 1];
            let gap = body.x - label.end;
            if label.horizontal
                && label.last_text().trim_end().ends_with(':')
                && body.horizontal
                && (0. ..label.size * 4.).contains(&gap)
                && label.end - label.x < label.size * 9.
                && (label.styled() || gap < label.size * 1.45)
            {
                let mut label = band.remove(j);
                let body = &mut band[j];
                // A right-aligned label can continue from the preceding row.
                if let Some(previous) = above.last_mut()
                    && let Some(k) = previous.iter().position(|p| {
                        p.horizontal
                            && p.styled()
                            && !p.last_text().trim_end().ends_with(':')
                            && (p.end - label.end).abs() < label.size * 0.5
                            && (label.size * 0.8..label.size * 1.6).contains(&(p.y - label.y))
                            && p.end - p.x < label.size * 9.
                    })
                {
                    let mut first = previous.remove(k);
                    join_runs(&mut first.runs, label.runs, true);
                    label.runs = first.runs;
                    if j < previous.len() {
                        let next = &mut previous[j];
                        if (next.x - body.x).abs() < 2. {
                            join_runs(&mut label.runs, std::mem::take(&mut next.runs), true);
                            next.runs = label.runs;
                            continue;
                        }
                    }
                }
                join_runs(&mut label.runs, std::mem::take(&mut body.runs), true);
                body.runs = label.runs;
            }
            j += 1;
        }
    }
}

fn table_row(row: &[Fragment], template: &[Fragment]) -> bool {
    row.len() == template.len()
        && row.iter().zip(template).enumerate().all(|(i, (f, t))| {
            let pitch = template
                .get(i + 1)
                .map(|next| next.x - t.x)
                .unwrap_or_else(|| t.x - template[i - 1].x);
            let center_distance = ((f.x + f.end) - (t.x + t.end)).abs() / 2.;
            f.horizontal && (f.x - t.x).abs().min(center_distance) < (pitch * 0.26).clamp(2., 12.)
        })
}

fn table_start(row: &[Fragment]) -> bool {
    (3..=32).contains(&row.len())
        && row.windows(2).all(|p| {
            p[1].x - p[0].x > p[0].size * 2. && (p[0].end - p[0].x) < (p[1].x - p[0].x) * 0.78
        })
        && row
            .last()
            .is_some_and(|last| last.end - last.x < last.size * 16.)
}

fn table_heading(items: &mut Vec<Item>, row: &[Fragment]) -> (Vec<Item>, Option<Vec<Cell>>) {
    let mut cursor = items.len();
    let mut baseline = row[0].y;
    let max_gap = row[0].size * 2.6;
    let mut headings = None;
    for _ in 0..2 {
        let Some(last) = cursor.checked_sub(1).map(|i| &items[i]) else {
            break;
        };
        if last.y - baseline > max_gap {
            break;
        }
        let start = items[..cursor].partition_point(|i| i.y > last.y + 1.15);
        let mut columns = Vec::new();
        for fragment in &items[start..cursor] {
            let nearest = row.iter().enumerate().min_by(|(_, a), (_, b)| {
                (fragment.x - a.x)
                    .abs()
                    .total_cmp(&(fragment.x - b.x).abs())
            });
            let Some((column, target)) = nearest else {
                break;
            };
            if fragment.line_size().is_none()
                || (fragment.x - target.x).abs() > max_gap
                || columns.last().is_some_and(|&previous| previous >= column)
            {
                break;
            }
            columns.push(column);
        }
        let minimum = if headings.is_none() {
            row.len()
        } else {
            row.len().div_ceil(2)
        };
        if columns.len() != cursor - start || columns.len() < minimum {
            break;
        }
        baseline = last.y;
        let cells = headings
            .get_or_insert_with(|| (0..row.len()).map(|_| Cell::default()).collect::<Vec<_>>());
        for (column, item) in columns.into_iter().zip(items.drain(start..cursor)) {
            if let Content::Line { mut runs, .. } = item.content {
                clean_line(&mut runs);
                let space = !runs.last().is_some_and(|r| r.text.ends_with('-'));
                join_runs(&mut runs, std::mem::take(&mut cells[column].runs), space);
                cells[column].runs = runs;
            }
        }
        cursor = start;
    }
    if headings.is_some() {
        while cursor > 0 && items.len() - cursor < 4 {
            let item = &items[cursor - 1];
            if item.line_size().is_none()
                || item.y - baseline > max_gap
                || item.x < row[0].x - max_gap
                || item.end > row.last().unwrap().end + max_gap
            {
                break;
            }
            baseline = item.y;
            cursor -= 1;
        }
    }
    (items.split_off(cursor), headings)
}

fn page_items(bands: Vec<Vec<Fragment>>) -> Vec<Item> {
    let mut items = Vec::new();
    let mut bands = bands.into_iter().filter(|b| !b.is_empty()).peekable();
    while let Some(first) = bands.next() {
        if table_start(&first) && bands.peek().is_some_and(|row| table_row(row, &first)) {
            let x = first[0].x;
            let y = first[0].y;
            let mut end = first.last().unwrap().end;
            let mut previous_y = y;
            let mut rows = Vec::new();
            let mut rest = Vec::new();
            while let Some(row) = bands.next_if(|row| {
                previous_y - row[0].y <= first[0].size * 2.5 && table_row(row, &first)
            }) {
                previous_y = row[0].y;
                end = end.max(row.last().unwrap().end);
                rest.push(row);
            }
            // Require vertically adjacent rows. Widely spaced text can start at the same x.
            if !rest.is_empty() {
                let (prefix, heading) = table_heading(&mut items, &first);
                rows.extend(heading);
                let y = prefix.first().map_or(y, |item| item.y);
                for row in std::iter::once(first).chain(rest) {
                    rows.push(
                        row.into_iter()
                            .map(|f| Cell {
                                properties: coordinates(f.x, f.y),
                                runs: f.runs,
                                ..Cell::default()
                            })
                            .collect(),
                    );
                }
                items.push(Item {
                    x,
                    end,
                    y,
                    content: Content::Table { prefix, rows },
                });
                continue;
            }
        }
        items.extend(first.into_iter().map(as_item));
    }
    items
}

fn gutter(items: &[Item]) -> Option<f32> {
    // Use repeated right-column starts and actual occupied line widths. A split
    // must separate nearby lines on both sides, including staggered baselines.
    let mut origins = BTreeMap::<i32, (usize, f32)>::new();
    for item in items.iter().filter(|i| i.line_size().is_some()) {
        let entry = origins
            .entry((item.x / 2.).round() as i32)
            .or_insert((0, item.x));
        entry.0 += 1;
        entry.1 = entry.1.min(item.x);
    }
    let mut candidates: Vec<_> = origins.into_values().filter(|(n, _)| *n >= 3).collect();
    candidates.sort_by_key(|&(n, _)| std::cmp::Reverse(n));
    candidates
        .into_iter()
        .take(24)
        .filter_map(|(_, origin)| {
            let cut = origin - 2.5;
            let mut matches = [[0usize; 2]; 2];
            let mut crossing = 0;
            for (direction, paired) in matches.iter_mut().enumerate() {
                let mut latest = [f32::INFINITY; 2];
                for index in 0..items.len() {
                    let item = if direction == 0 {
                        &items[index]
                    } else {
                        &items[items.len() - 1 - index]
                    };
                    let Some(size) = item.line_size() else {
                        continue;
                    };
                    let side = if item.x >= cut {
                        1
                    } else if item.end <= cut + 1. && cut - item.x > size * 5. {
                        0
                    } else {
                        crossing += usize::from(direction == 0 && item.end > cut + 1.);
                        continue;
                    };
                    latest[side] = item.y;
                    if (latest[1 - side] - item.y).abs() <= (size * 2.6).max(15.) {
                        paired[side] += 1;
                    }
                }
            }
            let left = matches[0][0].max(matches[1][0]);
            let right = matches[0][1].max(matches[1][1]);
            let support = left.min(right);
            let score = 3 * support + left.max(right) - (2 * crossing).min(support * 2);
            (support >= 2 && crossing < support * 2 + 4).then_some((score, cut))
        })
        .max_by_key(|&(score, _)| score)
        .map(|(_, cut)| cut)
}

fn read_region(items: Vec<Item>, out: &mut Vec<Block>, depth: usize) {
    if items.is_empty() {
        return;
    }
    if depth < 8
        && let Some(cut) = gutter(&items)
    {
        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut spanning = Vec::new();
        for item in items {
            if item.x >= cut {
                reflow(std::mem::take(&mut spanning), out);
                right.push(item);
            } else if item.end <= cut + 1. || matches!(&item.content, Content::Standalone(_)) {
                reflow(std::mem::take(&mut spanning), out);
                left.push(item);
            } else {
                read_region(std::mem::take(&mut left), out, depth + 1);
                read_region(std::mem::take(&mut right), out, depth + 1);
                spanning.push(item);
            }
        }
        reflow(spanning, out);
        read_region(left, out, depth + 1);
        read_region(right, out, depth + 1);
    } else {
        reflow(items, out);
    }
}

fn continues(prev: &LineInfo, next: &LineInfo, right: f32) -> bool {
    let size = prev.size.max(next.size);
    let gap = prev.y - next.y;
    if gap < size * 0.55
        || gap > size * 1.65
        || prev.size.min(next.size) < size * 0.85
        || prev.bold != next.bold
        || (next.x - prev.x > size * 0.8 && !prev.drop_cap)
        || prev.x - next.x > size * 4.
        || next.first.starts_with(['•', '▪', '■', '◦'])
    {
        return false;
    }
    let filled = right - prev.end <= size * 2.4;
    let wraps_word =
        prev.last.ends_with(['-', '\u{ad}']) && next.first.starts_with(char::is_alphabetic);
    let sentence_end = prev.last.ends_with(['.', '!', '?', ':', '。', '！', '？']);
    let continuation = next.first.starts_with(char::is_lowercase) && !sentence_end;
    wraps_word
        || (size >= 18. && (prev.x - next.x).abs() < size * 0.3)
        || (continuation && prev.end - prev.x > size * 9.)
        || (filled && prev.end - prev.x > size * 12.)
}

struct LineInfo {
    x: f32,
    end: f32,
    y: f32,
    size: f32,
    bold: bool,
    drop_cap: bool,
    first: String,
    last: String,
}

impl LineInfo {
    fn new(item: &Item, runs: &[Run], size: f32) -> Self {
        let first = runs
            .iter()
            .find_map(|r| r.text.split_whitespace().next())
            .unwrap_or("");
        let last = runs
            .iter()
            .rev()
            .find_map(|r| r.text.split_whitespace().next_back())
            .unwrap_or("");
        Self {
            x: item.x,
            end: item.end,
            y: item.y,
            size,
            bold: runs
                .iter()
                .rev()
                .find(|r| !r.text.trim().is_empty() && r.style.script.is_none())
                .is_some_and(|r| r.style.bold),
            drop_cap: runs.first().is_some_and(|r| {
                r.text.chars().count() == 1
                    && r.style
                        .size
                        .as_deref()
                        .and_then(|s| s.strip_suffix("pt"))
                        .and_then(|s| s.parse::<f32>().ok())
                        .is_some_and(|s| s > size * 2.5)
            }),
            first: first.into(),
            last: last.into(),
        }
    }
}

fn join_wrapped_line(
    previous: &LineInfo,
    next: &LineInfo,
    runs: &mut Vec<Run>,
    incoming: Vec<Run>,
) {
    let mut space = true;
    if let Some(last) = runs.last_mut()
        && let Some(after) = next.first.chars().next()
    {
        if last.text.ends_with('\u{ad}') {
            last.text.pop();
            space = false;
        } else if let Some(stem) = previous.last.strip_suffix('-')
            && stem.ends_with(char::is_alphanumeric)
            && after.is_alphanumeric()
        {
            // Compound fragments often already contain a hyphen, digit or
            // spelled-out number. Keep their visible hyphen when joining.
            let compound = stem.contains('-')
                || stem.contains(char::is_numeric)
                || matches!(
                    stem.to_ascii_lowercase().as_str(),
                    "one"
                        | "two"
                        | "three"
                        | "four"
                        | "five"
                        | "six"
                        | "seven"
                        | "eight"
                        | "nine"
                        | "ten"
                        | "self"
                        | "steady"
                        | "phase"
                );
            if !compound && after.is_lowercase() {
                last.text.pop();
            }
            space = false;
        } else if last.text.ends_with(joins_without_space) && joins_without_space(after) {
            space = false;
        }
    }
    join_runs(runs, incoming, space);
}

fn clean_line(runs: &mut Vec<Run>) {
    for r in runs.iter_mut() {
        if r.text.contains(['ﬀ', 'ﬁ', 'ﬂ', 'ﬃ', 'ﬄ']) {
            r.text = r
                .text
                .replace('ﬀ', "ff")
                .replace('ﬁ', "fi")
                .replace('ﬂ', "fl")
                .replace('ﬃ', "ffi")
                .replace('ﬄ', "ffl");
        }
    }
    let Some(first) = runs.iter().position(|r| !r.text.trim().is_empty()) else {
        return;
    };
    let last = runs
        .iter()
        .rposition(|r| !r.text.trim().is_empty())
        .unwrap();
    runs.truncate(last + 1);
    runs.drain(..first);
    let leading = runs[0].text.len() - runs[0].text.trim_start().len();
    runs[0].text.drain(..leading);
    let last = runs.last_mut().unwrap();
    last.text.truncate(last.text.trim_end().len());
}

fn reflow(items: Vec<Item>, out: &mut Vec<Block>) {
    let mut previous = None;
    // Compare line endings among similarly sized neighbors. Titles, footers and
    // side notes can have different margins from the surrounding body.
    let right_edges: Vec<_> = items.iter().map(|i| (i.end, i.line_size())).collect();
    for (idx, mut item) in items.into_iter().enumerate() {
        let info = match &mut item.content {
            Content::Line { runs, .. } => {
                clean_line(runs);
                if runs.iter().all(|r| r.text.trim().is_empty()) {
                    continue;
                }
                let item_size = item.line_size().unwrap();
                let Content::Line { runs, .. } = &item.content else {
                    unreachable!()
                };
                Some(LineInfo::new(&item, runs, item_size))
            }
            _ => None,
        };
        let x = item.x;
        let y = item.y;
        match item.content {
            Content::Line { runs, size } => {
                let info = info.unwrap();
                let right = right_edges[idx.saturating_sub(4)..(idx + 5).min(right_edges.len())]
                    .iter()
                    .filter(|(_, s)| s.is_some_and(|s| (s - size).abs() < size * 0.15))
                    .map(|&(end, _)| end)
                    .fold(item.end, f32::max);
                if let Some(prev) = &previous
                    && continues(prev, &info, right)
                    && let Some(Block::Paragraph { runs: para, .. }) = out.last_mut()
                {
                    join_wrapped_line(prev, &info, para, runs);
                } else {
                    out.push(Block::Paragraph {
                        heading: 0,
                        list: None,
                        runs,
                        properties: coordinates(x, y),
                    });
                }
                previous = Some(info);
            }
            Content::Standalone(runs) => {
                out.push(Block::Paragraph {
                    heading: 0,
                    list: None,
                    runs,
                    properties: coordinates(x, y),
                });
                previous = None;
            }
            Content::Table { prefix, rows } => {
                reflow(prefix, out);
                out.push(Block::Table { rows });
                previous = None;
            }
        }
    }
}

pub(super) fn layout(mut pieces: Vec<Fragment>, sec: &mut Section) {
    pieces.retain(|f| {
        f.runs
            .iter()
            .any(|r| r.image.is_some() || !r.text.trim().is_empty())
    });
    let mut bands = baseline_bands(pieces);
    attach_labels(&mut bands);
    let mut items = page_items(bands);
    items.sort_by(|a, b| b.y.total_cmp(&a.y).then_with(|| a.x.total_cmp(&b.x)));
    // Separate isolated running headers and page numbers from the column flow.
    let mut gaps: Vec<_> = items
        .windows(2)
        .map(|pair| pair[0].y - pair[1].y)
        .filter(|&gap| gap >= 5.)
        .collect();
    gaps.sort_by(f32::total_cmp);
    let typical_gap = gaps.get(gaps.len() / 2).copied().unwrap_or(0.);
    let mut region = Vec::new();
    let count = items.len();
    for (idx, item) in items.into_iter().enumerate() {
        if let Some(prev) = region.last() {
            let prev: &Item = prev;
            let size = prev
                .line_size()
                .unwrap_or(0.)
                .max(item.line_size().unwrap_or(0.));
            let gap = prev.y - item.y;
            let near_edge = (idx <= 3 || idx + 3 >= count)
                && gap > (size * 2.1).max(19.).max(typical_gap * 1.9);
            let after_title = idx <= 12 && gap > (size * 2.1).max(40.).max(typical_gap * 3.5);
            if near_edge || after_title {
                read_region(std::mem::take(&mut region), &mut sec.blocks, 0);
            }
        }
        region.push(item);
    }
    read_region(region, &mut sec.blocks, 0);
}
