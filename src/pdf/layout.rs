use crate::model::{Block, Cell, Properties, Run, Section, push_run};
use super::math::{Glyph, Script, Segment, to_latex};
use std::{collections::BTreeMap, ops::Range};

pub(super) struct Fragment {
    pub x: f32,
    pub end: f32,
    pub y: f32,
    pub size: f32,
    pub horizontal: bool,
    pub discretionary_hyphen: bool,
    pub runs: Vec<Run>,
}

#[derive(Clone, Copy)]
pub(super) struct HorizontalRule {
    pub left: f32,
    pub right: f32,
    pub y: f32,
    pub width: f32,
    pub color: [u8; 3],
    pub style: &'static str,
}

#[derive(Clone, Copy)]
pub(super) struct VerticalRule {
    pub x: f32,
    pub bottom: f32,
    pub top: f32,
    pub width: f32,
    pub color: [u8; 3],
    pub style: &'static str,
}

impl VerticalRule {
    fn border(&self) -> String {
        let [r, g, b] = self.color;
        format!("{}pt {} #{r:02X}{g:02X}{b:02X}", self.width, self.style)
    }
}

impl HorizontalRule {
    fn border(&self) -> String {
        let [r, g, b] = self.color;
        format!("{}pt {} #{r:02X}{g:02X}{b:02X}", self.width, self.style)
    }
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

fn hyhwp_eq_font(fragment: &Fragment) -> bool {
    fragment
        .runs
        .iter()
        .find_map(|run| run.style.font.as_deref())
        .is_some_and(|font| font.to_ascii_lowercase().contains("hyhwpeq"))
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

/// Rows are ordered by a quantised baseline. One visual row is often emitted as
/// several text objects whose baselines differ by hundredths of a point, so a
/// plain comparison scrambles option rows and inline math.
fn row_key(y: f32) -> i32 {
    (y * 2.).round() as i32
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
        let math_script = hyhwp_eq_font(f)
            && text.chars().count() <= 3
            && text
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '+' | '-' | '−' | '=' | '(' | ')'));
        if !drop_cap && !note && !math_script {
            continue;
        }
        let reach = if drop_cap {
            f.size
        } else if math_script {
            f.size * 3.
        } else {
            f.size * 1.6
        };
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
                    let nearby = p.size >= f.size * 1.25
                        && (p.size * 0.14..=p.size * 0.65).contains(&(f.y - p.y))
                        && (-f.size * 0.2..=f.size * 0.6).contains(&gap);
                    if math_script && hyhwp_eq_font(p) {
                        let radical_prefix = p.first_text().starts_with('√')
                            && p.size >= f.size * 1.25
                            && (p.size * 0.14..=p.size * 0.65).contains(&(f.y - p.y))
                            && (-f.size * 1.6..=f.size * 0.6).contains(&(p.x - f.end));
                        (nearby && hyhwp_eq_font(p)) || radical_prefix
                    } else {
                        nearby
                    }
                }
            })
            .max_by(|a, b| a.y.total_cmp(&b.y));
        if let Some(p) = candidate {
            let root_prefix = math_script
                && hyhwp_eq_font(p)
                && p.first_text().starts_with('√')
                && (-f.size * 1.6..=f.size * 0.6).contains(&(p.x - f.end));
            attachments.push((i, p.y, p.size, drop_cap, p.x, root_prefix));
        }
    }
    for (i, y, size, drop_cap, next_x, root_prefix) in attachments {
        let f = &mut pieces[i];
        f.y = y;
        f.size = size;
        if drop_cap {
            f.end = next_x;
        } else {
            if root_prefix {
                f.x = next_x - size * 0.55;
            }
            f.runs[0].style.script = Some("sup".into());
        }
    }
}

enum Content {
    Line {
        runs: Vec<Run>,
        size: f32,
        discretionary_hyphen: bool,
    },
    Standalone(Vec<Run>),
    Table {
        prefix: Vec<Item>,
        rows: Vec<Vec<Cell>>,
        suffix: Vec<Item>,
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
                discretionary_hyphen: f.discretionary_hyphen,
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
                prev.discretionary_hyphen = f.discretionary_hyphen;
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

fn ruled_headings(
    items: &mut Vec<Item>,
    row: &[Fragment],
    rules: &[HorizontalRule],
) -> (f32, Vec<Vec<Cell>>) {
    let mut baseline = row[0].y;
    let mut headings = Vec::new();
    if rules.is_empty() {
        return (baseline, headings);
    }
    let gap = row[0].size * 2.5;
    let mut boundaries = vec![true; row.len() + 1];
    while let Some(last) = items.last() {
        if !(0. < last.y - baseline && last.y - baseline <= gap) || headings.len() == 4 {
            break;
        }
        let band = items.partition_point(|i| i.y > last.y + 1.15);
        // Require multiple labels, each above a separate rule covering its columns.
        if !(2..row.len()).contains(&(items.len() - band)) {
            break;
        }
        let top = rules.partition_point(|r| r.y > last.y);
        let minimum_y = baseline.max(last.y - row[0].size * 0.8);
        let bottom = rules.partition_point(|r| r.y > minimum_y);
        let mut spans = Vec::new();
        for item in &items[band..] {
            if item.line_size().is_none() {
                break;
            }
            let span = rules[top..bottom].iter().find_map(|rule| {
                if item.x < rule.left - 1. || item.end > rule.right + 1. {
                    return None;
                }
                let start = row.partition_point(|f| f.x < rule.left - 1.);
                let end = row.partition_point(|f| f.x <= rule.right + 1.);
                (end >= start + 2 && boundaries[start] && boundaries[end])
                    .then_some((start, end, rule))
            });
            let Some((start, end, rule)) = span else {
                break;
            };
            if spans
                .last()
                .is_some_and(|&(_, previous, _)| previous > start)
            {
                break;
            }
            spans.push((start, end, rule));
        }
        if spans.len() != items.len() - band {
            break;
        }

        baseline = last.y;
        let mut cells = Vec::new();
        let mut column = 0;
        for ((start, end, rule), item) in spans.into_iter().zip(items.drain(band..)) {
            cells.extend((column..start).map(|_| Cell::default()));
            let Content::Line { mut runs, .. } = item.content else {
                unreachable!();
            };
            clean_line(&mut runs);
            let mut properties = coordinates(item.x, item.y);
            properties.insert("border-bottom".into(), rule.border());
            cells.push(Cell {
                runs,
                colspan: Some(end - start),
                properties,
                ..Cell::default()
            });
            boundaries[start + 1..end].fill(false);
            column = end;
        }
        cells.extend((column..row.len()).map(|_| Cell::default()));
        headings.push(cells);
    }
    headings.reverse();
    (baseline, headings)
}

fn cells_in_rule(band: &[Fragment], rule: &HorizontalRule) -> Range<usize> {
    let start = band.partition_point(|f| f.x < rule.left - 1.);
    start..band.partition_point(|f| f.x <= rule.right + 1.)
}

fn positioned_cell(mut f: Fragment) -> Cell {
    clean_line(&mut f.runs);
    Cell {
        properties: coordinates(f.x, f.y),
        runs: f.runs,
        ..Cell::default()
    }
}

fn ruled_table(
    bands: &mut [Vec<Fragment>],
    baselines: &[f32],
    top: &HorizontalRule,
    bottom: &HorizontalRule,
) -> Option<Item> {
    let first = baselines.partition_point(|&y| y > top.y);
    let last = baselines.partition_point(|&y| y > bottom.y);
    if first == 0 || first >= last {
        return None;
    }
    let body: Vec<_> = (first..last)
        .map(|i| (i, cells_in_rule(&bands[i], top)))
        .filter(|(_, cells)| !cells.is_empty())
        .collect();
    let (i, cells) = body.iter().find(|(_, cells)| cells.len() >= 3)?;
    let template = &bands[*i][cells.clone()];
    let width = template.len();
    let size = template[0].size;
    let section_row =
        |row: &[Fragment]| row.len() == 1 && row[0].horizontal && row[0].x <= template[0].x + size;
    if width > 32
        || body.len().saturating_mul(width) > crate::model::CELL_LIMIT
        || !body.iter().any(|(i, r)| section_row(&bands[*i][r.clone()]))
        || body.iter().filter(|(_, r)| r.len() == width).count() < 2
        || body.iter().any(|(i, r)| {
            let row = &bands[*i][r.clone()];
            row.last().unwrap().end > top.right + 2.
                || (!section_row(row) && !table_row(row, template))
        })
    {
        return None;
    }

    // Use the full heading row just above the rule, then collect aligned wraps above it.
    let mut headers = Vec::new();
    for i in (0..first).rev() {
        let below = headers.last().map_or(top.y, |&(i, _, _)| baselines[i]);
        if baselines[i] - below > size * 1.65 || headers.len() == 4 {
            break;
        }
        let range = cells_in_rule(&bands[i], top);
        if range.is_empty() {
            continue;
        }
        let line = &bands[i][range.clone()];
        let mut columns = Vec::new();
        for f in line {
            let (column, anchor) = template
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| (f.x - a.x).abs().total_cmp(&(f.x - b.x).abs()))?;
            if !f.horizontal
                || (f.size - size).abs() > size * 0.12
                || (f.x - anchor.x).abs() > size * 1.7
                || columns.last().is_some_and(|&last| last >= column)
            {
                break;
            }
            columns.push(column);
        }
        if columns.is_empty()
            || columns.len() != line.len()
            || (headers.is_empty() && columns.len() != width)
        {
            break;
        }
        headers.push((i, range, columns));
    }
    let &(header_start, _, _) = headers.last()?;

    let mut prefix = Vec::new();
    let mut y = baselines[header_start];
    for i in (0..header_start).rev() {
        if baselines[i] - y > size * 2.5 || prefix.len() == 3 {
            break;
        }
        let Some(j) = bands[i]
            .iter()
            .position(|f| f.x >= top.left - size * 1.25 && f.x <= top.right)
        else {
            continue;
        };
        let f = &bands[i][j];
        if !f.horizontal || !f.styled() || f.x > top.left + size * 1.25 || f.end > top.right + 2. {
            break;
        }
        prefix.push(as_item(bands[i].remove(j)));
        y = baselines[i];
    }
    prefix.reverse();

    let mut heading = vec![Cell::default(); width];
    for (i, range, columns) in headers.into_iter().rev() {
        for (column, f) in columns.into_iter().zip(bands[i].drain(range)) {
            let cell = positioned_cell(f);
            let head = &mut heading[column];
            if head.runs.is_empty() {
                head.properties = cell.properties;
            }
            join_runs(&mut head.runs, cell.runs, true);
        }
    }
    let mut rows = vec![heading];
    for (i, range) in body {
        let mut row: Vec<_> = bands[i].drain(range).map(positioned_cell).collect();
        if row.len() == 1 {
            row[0].colspan = Some(width);
        }
        rows.push(row);
    }
    let header_border = top.border();
    for cell in &mut rows[0] {
        cell.properties
            .insert("border-bottom".into(), header_border.clone());
    }
    let footer_border = bottom.border();
    for cell in rows.last_mut().unwrap() {
        cell.properties
            .insert("border-bottom".into(), footer_border.clone());
    }

    let mut suffix = Vec::new();
    let mut previous_y = bottom.y;
    for i in last..bands.len() {
        if previous_y - baselines[i] > size * 1.6 || suffix.len() == 4 {
            break;
        }
        let range = cells_in_rule(&bands[i], top);
        if range.is_empty() {
            continue;
        }
        let [f] = &bands[i][range.clone()] else { break };
        if !f.horizontal || f.size >= size * 0.95 || (f.x - top.left).abs() > 1. {
            break;
        }
        suffix.extend(bands[i].drain(range).map(as_item));
        previous_y = baselines[i];
    }

    Some(Item {
        x: top.left,
        end: top.right,
        y,
        content: Content::Table {
            prefix,
            rows,
            suffix,
        },
    })
}

fn ruled_tables(bands: &mut [Vec<Fragment>], rules: &[HorizontalRule]) -> Vec<Item> {
    let baselines: Vec<_> = bands.iter().map(|b| b[0].y).collect();
    let mut tables = segmented_tables(bands, &baselines, rules);
    for (i, top) in rules.iter().enumerate() {
        let bottom = rules[i + 1..]
            .iter()
            .take(64)
            .find(|r| (r.left - top.left).abs() <= 1. && (r.right - top.right).abs() <= 1.);
        if let Some(bottom) = bottom
            && let Some(table) = ruled_table(bands, &baselines, top, bottom)
        {
            tables.push(table);
        }
    }
    tables
}

#[derive(Clone)]
struct HorizontalLevel {
    y: f32,
    segments: Vec<(f32, f32)>,
    border: String,
}

#[derive(Clone)]
struct VerticalSegment {
    x: f32,
    bottom: f32,
    top: f32,
    border: String,
}

fn horizontal_levels(rules: &[HorizontalRule]) -> Vec<HorizontalLevel> {
    let mut sorted = rules.to_vec();
    sorted.sort_by(|a, b| b.y.total_cmp(&a.y));
    let mut levels: Vec<(f32, Vec<&HorizontalRule>)> = Vec::new();
    for rule in &sorted {
        if let Some((y, items)) = levels.last_mut()
            && (*y - rule.y).abs() <= 1.5
        {
            *y = (*y + rule.y) * 0.5;
            items.push(rule);
        } else {
            levels.push((rule.y, vec![rule]));
        }
    }
    levels
        .into_iter()
        .map(|(y, items)| {
            let mut segments: Vec<_> = items.iter().map(|r| (r.left, r.right)).collect();
            segments.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut merged: Vec<(f32, f32)> = Vec::new();
            for (left, right) in segments {
                if let Some(last) = merged.last_mut()
                    && left <= last.1 + 2.5
                {
                    last.1 = last.1.max(right);
                } else {
                    merged.push((left, right));
                }
            }
            HorizontalLevel {
                y,
                segments: merged,
                border: items[0].border(),
            }
        })
        .collect()
}

fn vertical_segments(rules: &[VerticalRule]) -> Vec<VerticalSegment> {
    let mut sorted = rules.to_vec();
    sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut columns: Vec<(f32, Vec<&VerticalRule>)> = Vec::new();
    for rule in &sorted {
        if let Some((x, items)) = columns.last_mut()
            && (*x - rule.x).abs() <= 1.5
        {
            *x = (*x + rule.x) * 0.5;
            items.push(rule);
        } else {
            columns.push((rule.x, vec![rule]));
        }
    }
    let mut segments = Vec::new();
    for (x, items) in columns {
        let mut intervals: Vec<_> = items
            .iter()
            .map(|r| (r.bottom, r.top, r.border()))
            .collect();
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f32, f32, String)> = Vec::new();
        for (bottom, top, border) in intervals {
            if let Some(last) = merged.last_mut()
                && bottom <= last.1 + 2.5
            {
                last.1 = last.1.max(top);
            } else {
                merged.push((bottom, top, border));
            }
        }
        segments.extend(
            merged
                .into_iter()
                .map(|(bottom, top, border)| VerticalSegment {
                    x,
                    bottom,
                    top,
                    border,
                }),
        );
    }
    segments
}

fn interval_covers(intervals: &[(f32, f32)], left: f32, right: f32, tolerance: f32) -> bool {
    let mut intervals = intervals.to_vec();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut covered = left;
    let end = right;
    for (start, finish) in intervals {
        if finish < covered - tolerance {
            continue;
        }
        if start > covered + tolerance {
            return false;
        }
        covered = covered.max(finish);
        if covered >= end - tolerance {
            return true;
        }
    }
    covered >= end - tolerance
}

fn horizontal_edge(levels: &[HorizontalLevel], y: f32, left: f32, right: f32) -> Option<String> {
    levels.iter().find_map(|level| {
        ((level.y - y).abs() <= 1.5 && interval_covers(&level.segments, left, right, 1.5))
            .then(|| level.border.clone())
    })
}

fn vertical_edge(segments: &[VerticalSegment], x: f32, bottom: f32, top: f32) -> Option<String> {
    segments.iter().find_map(|segment| {
        ((segment.x - x).abs() <= 1.5
            && interval_covers(&[(segment.bottom, segment.top)], bottom, top, 1.5))
        .then(|| segment.border.clone())
    })
}

fn root(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

fn join(parent: &mut [usize], a: usize, b: usize) {
    let (a, b) = (root(parent, a), root(parent, b));
    if a != b {
        parent[b] = a;
    }
}

fn grid_components(
    horizontal: &[HorizontalLevel],
    vertical: &[VerticalSegment],
) -> Vec<(Vec<usize>, Vec<usize>)> {
    let mut parent: Vec<_> = (0..horizontal.len() + vertical.len()).collect();
    for (j, segment) in vertical.iter().enumerate() {
        let first = horizontal.partition_point(|line| line.y > segment.top + 2.);
        let last = horizontal.partition_point(|line| line.y >= segment.bottom - 2.);
        for i in first..last {
            let level = &horizontal[i];
            let crosses_x = level
                .segments
                .iter()
                .any(|(left, right)| segment.x >= left - 2. && segment.x <= right + 2.);
            let crosses_y = level.y >= segment.bottom - 2. && level.y <= segment.top + 2.;
            if crosses_x && crosses_y {
                join(&mut parent, i, horizontal.len() + j);
            }
        }
    }
    let mut components = BTreeMap::<usize, (Vec<usize>, Vec<usize>)>::new();
    for i in 0..horizontal.len() {
        let key = root(&mut parent, i);
        components.entry(key).or_default().0.push(i);
    }
    for j in 0..vertical.len() {
        let key = root(&mut parent, horizontal.len() + j);
        components.entry(key).or_default().1.push(j);
    }
    components
        .into_values()
        .filter(|(h, v)| h.len() >= 2 && !v.is_empty())
        .collect()
}

fn fragment_cell(mut fragments: Vec<Fragment>) -> Cell {
    fragments.sort_by(|a, b| b.y.total_cmp(&a.y).then_with(|| a.x.total_cmp(&b.x)));
    let mut lines: Vec<Vec<Fragment>> = Vec::new();
    for fragment in fragments {
        if let Some(line) = lines.last_mut()
            && line[0].y - fragment.y <= 1.15
        {
            line.push(fragment);
        } else {
            lines.push(vec![fragment]);
        }
    }
    for line in &mut lines {
        line.sort_by(|a, b| a.x.total_cmp(&b.x));
    }
    let fragments: Vec<_> = lines.into_iter().flatten().collect();
    let Some(first) = fragments.first() else {
        return Cell::default();
    };
    let position = coordinates(first.x, first.y);
    let mut runs = Vec::new();
    let mut previous_y = None;
    let mut previous_hyphen = false;
    for mut fragment in fragments {
        clean_line(&mut fragment.runs);
        if runs.is_empty() {
            runs = fragment.runs;
        } else {
            let korean_continuation =
                last_korean_syllable(&runs) && single_korean_syllable(&fragment.runs);
            if korean_continuation {
                join_runs(&mut runs, fragment.runs, false);
            } else if previous_y.is_some_and(|y: f32| (y - fragment.y).abs() <= 1.15) {
                join_runs(&mut runs, fragment.runs, true);
            } else {
                join_wrapped_line(&mut runs, fragment.runs, previous_hyphen);
            }
        }
        previous_y = Some(fragment.y);
        previous_hyphen = fragment.discretionary_hyphen;
    }
    Cell {
        runs,
        properties: position,
        ..Cell::default()
    }
}

fn last_korean_syllable(runs: &[Run]) -> bool {
    runs.iter()
        .rev()
        .filter_map(|run| run.text.trim_end().chars().next_back())
        .next()
        .is_some_and(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c))
}

fn single_korean_syllable(runs: &[Run]) -> bool {
    let text: String = runs.iter().map(|run| run.text.as_str()).collect();
    let mut chars = text.trim().chars();
    chars
        .next()
        .is_some_and(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c))
        && chars.next().is_none()
}

fn split_runs_at(runs: Vec<Run>, char_index: usize) -> Option<(Vec<Run>, Vec<Run>)> {
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut remaining = char_index;
    for run in runs {
        if run.image.is_some() || run.mathml.is_some() {
            return None;
        }
        let count = run.text.chars().count();
        if remaining >= count {
            if !run.text.is_empty() {
                remaining -= count;
                left.push(run);
            }
        } else if remaining == 0 {
            right.push(run);
        } else {
            let byte_index = run
                .text
                .char_indices()
                .nth(remaining)
                .map(|(index, _)| index)
                .unwrap_or(run.text.len());
            let mut before = run.clone();
            before.text = run.text[..byte_index].to_owned();
            let mut after = run;
            after.text = after.text[byte_index..].to_owned();
            if !before.text.is_empty() {
                left.push(before);
            }
            if !after.text.is_empty() {
                right.push(after);
            }
            remaining = 0;
        }
    }
    (remaining == 0 && !left.is_empty() && !right.is_empty()).then_some((left, right))
}

fn split_fragment_at(fragment: Fragment, boundary: f32) -> Vec<Fragment> {
    let width = fragment.end - fragment.x;
    if !fragment.horizontal || boundary <= fragment.x + 1. || boundary >= fragment.end - 1. {
        return vec![fragment];
    }
    let chars: Vec<_> = fragment
        .runs
        .iter()
        .flat_map(|run| run.text.chars())
        .collect();
    if chars.len() < 3 {
        return vec![fragment];
    }
    let weights: Vec<_> = chars
        .iter()
        .map(|&c| approximate_width(&c.to_string()))
        .collect();
    let total: f32 = weights.iter().sum();
    if total <= 0. {
        return vec![fragment];
    }
    let target = total * ((boundary - fragment.x) / width).clamp(0., 1.);
    let mut cumulative = 0.;
    let mut split = None;
    let mut distance = f32::INFINITY;
    for index in 1..chars.len() {
        cumulative += weights[index - 1];
        if chars[index - 1].is_whitespace() || chars[index].is_whitespace() {
            let current = (cumulative - target).abs();
            if current < distance {
                distance = current;
                split = Some(index);
            }
        }
    }
    if distance > (total * 0.18).max(1.25) {
        return vec![fragment];
    }
    let Some((left_runs, right_runs)) = split.and_then(|i| split_runs_at(fragment.runs.clone(), i))
    else {
        return vec![fragment];
    };
    let right = Fragment {
        x: boundary,
        end: fragment.end,
        y: fragment.y,
        size: fragment.size,
        horizontal: fragment.horizontal,
        discretionary_hyphen: fragment.discretionary_hyphen,
        runs: right_runs,
    };
    let left = Fragment {
        x: fragment.x,
        end: boundary,
        y: fragment.y,
        size: fragment.size,
        horizontal: fragment.horizontal,
        discretionary_hyphen: fragment.discretionary_hyphen,
        runs: left_runs,
    };
    vec![left, right]
}

fn grid_tables(
    bands: &mut [Vec<Fragment>],
    rules: &[HorizontalRule],
    vertical_rules: &[VerticalRule],
) -> Vec<Item> {
    let horizontal = horizontal_levels(rules);
    let vertical = vertical_segments(vertical_rules);
    let components = grid_components(&horizontal, &vertical);
    let mut tables = Vec::new();
    for (horizontal_ids, vertical_ids) in components {
        let component_horizontal: Vec<_> = horizontal_ids
            .iter()
            .map(|&i| horizontal[i].clone())
            .collect();
        let component_vertical: Vec<_> =
            vertical_ids.iter().map(|&i| vertical[i].clone()).collect();
        let mut levels: Vec<_> = horizontal_ids.iter().map(|&i| &horizontal[i]).collect();
        levels.sort_by(|a, b| b.y.total_cmp(&a.y));
        let mut y_edges = Vec::new();
        for level in levels {
            if y_edges
                .last()
                .is_none_or(|y: &f32| (*y - level.y).abs() > 1.5)
            {
                y_edges.push(level.y);
            }
        }
        let mut x_edges: Vec<_> = horizontal_ids
            .iter()
            .flat_map(|&i| horizontal[i].segments.iter().flat_map(|(a, b)| [*a, *b]))
            .chain(vertical_ids.iter().map(|&i| vertical[i].x))
            .collect();
        x_edges.sort_by(f32::total_cmp);
        x_edges.dedup_by(|a, b| (*a - *b).abs() <= 1.5);
        if x_edges.len() < 3 || y_edges.len() < 2 {
            continue;
        }
        let (left, right) = (x_edges[0], *x_edges.last().unwrap());
        let (top, bottom) = (y_edges[0], *y_edges.last().unwrap());
        if right - left < 30. || top - bottom < 8. {
            continue;
        }
        let columns = x_edges.len() - 1;
        let row_count = y_edges.len() - 1;
        // The outermost horizontal edges must close the whole grid. A callout
        // border is often drawn as two strokes interrupted by its label, and
        // gluing such a stroke to unrelated rules on the same baseline would
        // turn the surrounding prose on both sides into table cells.
        let closed = |y: f32| {
            component_horizontal.iter().any(|level| {
                (level.y - y).abs() <= 1.5 && interval_covers(&level.segments, left, right, 1.5)
            })
        };
        if !closed(top) || !closed(bottom) {
            continue;
        }
        if columns == 0
            || row_count == 0
            || columns.saturating_mul(row_count) > crate::model::CELL_LIMIT
            || vertical_ids
                .iter()
                .filter(|&&i| {
                    let line = &vertical[i];
                    line.x > left + 2.
                        && line.x < right - 2.
                        && line.top - line.bottom >= (top - bottom) * 0.2
                })
                .count()
                == 0
        {
            continue;
        }

        // A framed callout can overlap a long page divider and look like a
        // one-row grid. Keep multi-line prose in reading flow instead of
        // turning unrelated text on both sides into table cells.
        if row_count == 1 && columns > 1 {
            let mut characters = vec![0usize; columns];
            let mut line_y = vec![Vec::<i32>::new(); columns];
            for fragment in bands.iter().flatten().filter(|fragment| {
                fragment.y <= top + 2.
                    && fragment.y >= bottom - 2.
                    && fragment.end > left
                    && fragment.x < right
            }) {
                let center = (fragment.x + fragment.end) * 0.5;
                let Some(column) = (0..columns).find(|&column| {
                    center >= x_edges[column] - 1. && center <= x_edges[column + 1] + 1.
                }) else {
                    continue;
                };
                characters[column] += fragment
                    .runs
                    .iter()
                    .map(|run| run.text.chars().count())
                    .sum::<usize>();
                let y = (fragment.y * 10.).round() as i32;
                if !line_y[column].iter().any(|&known| (known - y).abs() <= 11) {
                    line_y[column].push(y);
                }
            }
            if characters
                .iter()
                .zip(&line_y)
                .any(|(&count, lines)| count >= 40 && lines.len() >= 2)
            {
                continue;
            }
        }

        let mut contents: Vec<Vec<Vec<Fragment>>> = (0..row_count)
            .map(|_| (0..columns).map(|_| Vec::new()).collect())
            .collect();
        for band in bands.iter_mut() {
            let mut remaining = Vec::new();
            for fragment in std::mem::take(band) {
                let Some(row) = (0..row_count).find(|&row| {
                    fragment.y <= y_edges[row] + 2. && fragment.y >= y_edges[row + 1] - 2.
                }) else {
                    remaining.push(fragment);
                    continue;
                };
                let split_edges: Vec<_> = (1..columns)
                    .filter_map(|column| {
                        vertical_edge(
                            &component_vertical,
                            x_edges[column],
                            y_edges[row + 1],
                            y_edges[row],
                        )
                        .map(|_| x_edges[column])
                    })
                    .collect();
                let fragments = split_edges.into_iter().fold(vec![fragment], |parts, edge| {
                    parts
                        .into_iter()
                        .flat_map(|fragment| split_fragment_at(fragment, edge))
                        .collect()
                });
                for fragment in fragments {
                    let x = (fragment.x + fragment.end) * 0.5;
                    if let Some(column) = (0..columns)
                        .find(|&column| x >= x_edges[column] - 1. && x <= x_edges[column + 1] + 1.)
                    {
                        contents[row][column].push(fragment);
                    } else {
                        remaining.push(fragment);
                    }
                }
            }
            *band = remaining;
        }

        let mut occupied = vec![vec![false; columns]; row_count];
        let mut rows = Vec::new();
        for row in 0..row_count {
            let mut cells = Vec::new();
            for column in 0..columns {
                if occupied[row][column] {
                    continue;
                }
                let mut end_column = column + 1;
                while end_column < columns
                    && vertical_edge(
                        &component_vertical,
                        x_edges[end_column],
                        y_edges[row + 1],
                        y_edges[row],
                    )
                    .is_none()
                {
                    end_column += 1;
                }
                let mut end_row = row + 1;
                while end_row < row_count
                    && (column..end_column).all(|col| {
                        horizontal_edge(
                            &component_horizontal,
                            y_edges[end_row],
                            x_edges[col],
                            x_edges[col + 1],
                        )
                        .is_none()
                    })
                {
                    end_row += 1;
                }
                let mut fragments = Vec::new();
                for row_index in row..end_row {
                    for column_index in column..end_column {
                        occupied[row_index][column_index] = true;
                        fragments.append(&mut contents[row_index][column_index]);
                    }
                }
                let mut cell = fragment_cell(fragments);
                if end_column - column > 1 {
                    cell.colspan = Some(end_column - column);
                }
                if end_row - row > 1 {
                    cell.rowspan = Some(end_row - row);
                }
                let cell_left = x_edges[column];
                let cell_right = x_edges[end_column];
                let cell_top = y_edges[row];
                let cell_bottom = y_edges[end_row];
                if let Some(border) =
                    horizontal_edge(&component_horizontal, cell_top, cell_left, cell_right)
                {
                    cell.properties.insert("border-top".into(), border);
                }
                if let Some(border) =
                    horizontal_edge(&component_horizontal, cell_bottom, cell_left, cell_right)
                {
                    cell.properties.insert("border-bottom".into(), border);
                }
                if let Some(border) =
                    vertical_edge(&component_vertical, cell_left, cell_bottom, cell_top)
                {
                    cell.properties.insert("border-left".into(), border);
                }
                if let Some(border) =
                    vertical_edge(&component_vertical, cell_right, cell_bottom, cell_top)
                {
                    cell.properties.insert("border-right".into(), border);
                }
                cells.push(cell);
            }
            rows.push(cells);
        }
        tables.push(Item {
            x: left,
            end: right,
            y: top,
            content: Content::Table {
                prefix: Vec::new(),
                rows,
                suffix: Vec::new(),
            },
        });
    }
    tables
}

// Some PDFs draw each column's horizontal border separately. Repeated sets
// of touching segments give us both the table bounds and the column edges.
fn segmented_tables(
    bands: &mut [Vec<Fragment>],
    baselines: &[f32],
    rules: &[HorizontalRule],
) -> Vec<Item> {
    let mut borders = Vec::new();
    for line in rules.chunk_by(|a, b| (a.y - b.y).abs() <= 0.25) {
        let mut segments: Vec<_> = line.iter().collect();
        segments.sort_by(|a, b| a.left.total_cmp(&b.left));
        for columns in segments.chunk_by(|a, b| (a.right - b.left).abs() <= 1.) {
            if (2..=32).contains(&columns.len()) {
                borders.push(columns.to_vec());
            }
        }
    }
    let mut tables = Vec::new();
    for (i, top) in borders.iter().enumerate() {
        let mut matching = borders.iter().skip(i + 1).take(64).filter(|line| {
            line.len() == top.len()
                && line
                    .iter()
                    .zip(top)
                    .all(|(a, b)| (a.left - b.left).abs() <= 1. && (a.right - b.right).abs() <= 1.)
        });
        if let (Some(header), Some(bottom)) = (matching.next(), matching.next())
            && let Some(table) = segmented_table(bands, baselines, top, header, bottom)
        {
            tables.push(table);
        }
    }
    tables
}

fn segmented_table(
    bands: &mut [Vec<Fragment>],
    baselines: &[f32],
    top: &[&HorizontalRule],
    header: &[&HorizontalRule],
    bottom: &[&HorizontalRule],
) -> Option<Item> {
    let left = top[0].left;
    let right = top.last()?.right;
    let first = baselines.partition_point(|&y| y > top[0].y);
    let body = baselines.partition_point(|&y| y > header[0].y);
    let last = baselines.partition_point(|&y| y > bottom[0].y);
    if first >= body
        || body >= last
        || (last - first).saturating_mul(top.len()) > crate::model::CELL_LIMIT
    {
        return None;
    }

    let mut plan = Vec::new();
    let mut row = 0;
    let mut previous = None;
    for (i, band) in bands.iter().enumerate().skip(first).take(last - first) {
        let start = band.partition_point(|f| f.x < left - 1.);
        let end = band.partition_point(|f| f.x <= right + 1.);
        let line = &band[start..end];
        if line.is_empty() {
            continue;
        }
        let mut columns = Vec::new();
        for f in line {
            let col = top.partition_point(|rule| rule.right <= f.x + 0.5);
            let border = top.get(col)?;
            if !f.horizontal
                || f.end > border.right + 2.
                || columns.last().is_some_and(|&c| c >= col)
            {
                return None;
            }
            columns.push(col);
        }
        let full = line.len() == top.len();
        if previous.is_none() && (i >= body || !full) {
            return None;
        }
        if i >= body && full {
            row += 1;
        } else if let Some((last_y, last_size)) = previous
            && ((i >= body && row == 0)
                || (i >= body && columns[0] == 0)
                || last_y - line[0].y > last_size * 1.8)
        {
            return None;
        }
        previous = Some((line[0].y, line[0].size));
        plan.push((i, start..end, row, columns));
    }
    if row < 2 {
        return None;
    }

    let y = baselines[plan[0].0];
    let mut rows = vec![vec![Cell::default(); top.len()]; row + 1];
    let mut discretionary = vec![false; top.len()];
    let mut current_row = 0;
    for (i, range, row, columns) in plan {
        if row != current_row {
            discretionary.fill(false);
            current_row = row;
        }
        for (f, col) in bands[i].drain(range).zip(columns) {
            let hyphen = f.discretionary_hyphen;
            let cell = positioned_cell(f);
            let target = &mut rows[row][col];
            if target.runs.is_empty() {
                *target = cell;
            } else {
                join_wrapped_line(&mut target.runs, cell.runs, discretionary[col]);
            }
            discretionary[col] = hyphen;
        }
    }
    for (i, cell) in rows[0].iter_mut().enumerate() {
        cell.properties.insert("border-top".into(), top[i].border());
        cell.properties
            .insert("border-bottom".into(), header[i].border());
    }
    for (i, cell) in rows[row].iter_mut().enumerate() {
        cell.properties
            .insert("border-bottom".into(), bottom[i].border());
    }
    Some(Item {
        x: left,
        end: right,
        y,
        content: Content::Table {
            prefix: Vec::new(),
            rows,
            suffix: Vec::new(),
        },
    })
}

fn fragment_text(fragment: &Fragment) -> String {
    fragment.runs.iter().map(|run| run.text.as_str()).collect()
}

fn numeric_value(fragment: &Fragment) -> bool {
    let text = fragment_text(fragment);
    let text = text.trim();
    text.chars().any(char::is_numeric)
        && text.chars().all(|c| {
            c.is_numeric()
                || matches!(
                    c,
                    ',' | '.'
                        | ' '
                        | '\u{3000}'
                        | '円'
                        | '¥'
                        | '￥'
                        | '$'
                        | '€'
                        | '£'
                        | '₩'
                        | '('
                        | ')'
                        | '+'
                        | '-'
                        | '−'
                        | '%'
                )
        })
}

fn origin_clusters(fragments: &[&Fragment], tolerance: f32) -> Vec<(f32, usize)> {
    let mut positions: Vec<_> = fragments.iter().map(|f| f.x).collect();
    positions.sort_by(f32::total_cmp);
    let mut clusters: Vec<(f32, usize)> = Vec::new();
    for x in positions {
        if let Some((center, count)) = clusters.last_mut()
            && (x - *center).abs() <= tolerance
        {
            *center = (*center * *count as f32 + x) / (*count + 1) as f32;
            *count += 1;
        } else {
            clusters.push((x, 1));
        }
    }
    clusters
}

fn value_clusters(fragments: &[&Fragment], tolerance: f32) -> Vec<(f32, usize)> {
    let mut positions: Vec<_> = fragments.iter().map(|f| f.end).collect();
    positions.sort_by(f32::total_cmp);
    let mut clusters: Vec<(f32, usize)> = Vec::new();
    for x in positions {
        if let Some((center, count)) = clusters.last_mut()
            && (x - *center).abs() <= tolerance
        {
            *center = (*center * *count as f32 + x) / (*count + 1) as f32;
            *count += 1;
        } else {
            clusters.push((x, 1));
        }
    }
    clusters
}

fn table_row_from_band(band: &mut Vec<Fragment>, labels: [f32; 2], values: [f32; 2]) -> Vec<Cell> {
    let mut columns: [Vec<Fragment>; 4] = std::array::from_fn(|_| Vec::new());
    for fragment in std::mem::take(band) {
        let column = if numeric_value(&fragment) {
            1 + usize::from((fragment.end - values[1]).abs() < (fragment.end - values[0]).abs()) * 2
        } else {
            usize::from((fragment.x - labels[1]).abs() < (fragment.x - labels[0]).abs()) * 2
        };
        columns[column].push(fragment);
    }
    columns.into_iter().map(fragment_cell).collect()
}

fn parallel_value_tables(bands: &mut [Vec<Fragment>]) -> Vec<Item> {
    let value_fragments: Vec<_> = bands
        .iter()
        .flatten()
        .filter(|f| numeric_value(f))
        .collect();
    if value_fragments.len() < 6 {
        return Vec::new();
    }
    let mut value_groups: Vec<_> = value_clusters(&value_fragments, 18.)
        .into_iter()
        .filter(|(_, count)| *count >= 3)
        .collect();
    value_groups.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    value_groups.truncate(2);
    if value_groups.len() != 2 || (value_groups[0].0 - value_groups[1].0).abs() < 50. {
        return Vec::new();
    }
    value_groups.sort_by(|a, b| a.0.total_cmp(&b.0));
    let value_edges = [value_groups[0].0, value_groups[1].0];

    let labels: Vec<_> = bands
        .iter()
        .flatten()
        .filter(|f| !numeric_value(f))
        .collect();
    let mut label_groups: Vec<_> = origin_clusters(&labels, 8.)
        .into_iter()
        .filter(|(_, count)| *count >= 4)
        .collect();
    label_groups.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    label_groups.truncate(2);
    if label_groups.len() != 2 || (label_groups[0].0 - label_groups[1].0).abs() < 60. {
        return Vec::new();
    }
    label_groups.sort_by(|a, b| a.0.total_cmp(&b.0));
    let label_edges = [label_groups[0].0, label_groups[1].0];

    let mut sizes: Vec<_> = value_fragments.iter().map(|f| f.size).collect();
    sizes.sort_by(f32::total_cmp);
    let value_size = sizes[sizes.len() / 2];
    let mut headings = Vec::new();
    let mut value_rows = Vec::new();
    for (i, band) in bands.iter().enumerate() {
        let has_value = band.iter().any(|f| {
            numeric_value(f) && value_edges.iter().any(|edge| (f.end - edge).abs() <= 18.)
        });
        if has_value {
            value_rows.push(i);
            continue;
        }
        let left = band.iter().any(|f| {
            !numeric_value(f) && f.size >= value_size * 1.2 && (f.x - label_edges[0]).abs() <= 8.
        });
        let right = band.iter().any(|f| {
            !numeric_value(f) && f.size >= value_size * 1.2 && (f.x - label_edges[1]).abs() <= 8.
        });
        if left && right {
            headings.push(i);
        }
    }
    if headings.is_empty() {
        return Vec::new();
    }
    let band_y: Vec<_> = bands
        .iter()
        .map(|band| band.first().map_or(f32::NAN, |f| f.y))
        .collect();
    headings.sort_by(|a, b| band_y[*b].total_cmp(&band_y[*a]));
    value_rows.sort_by(|a, b| band_y[*b].total_cmp(&band_y[*a]));

    let mut tables = Vec::new();
    for (index, &heading) in headings.iter().enumerate() {
        let top = band_y[heading];
        let bottom = headings
            .get(index + 1)
            .map(|&i| band_y[i])
            .unwrap_or(f32::NEG_INFINITY);
        let rows_for_table: Vec<_> = value_rows
            .iter()
            .copied()
            .filter(|&i| band_y[i] < top && band_y[i] > bottom)
            .collect();
        if rows_for_table.len() < 2 {
            continue;
        }
        let y = band_y[heading];
        let mut rows = vec![table_row_from_band(
            &mut bands[heading],
            label_edges,
            value_edges,
        )];
        for row in rows_for_table {
            rows.push(table_row_from_band(
                &mut bands[row],
                label_edges,
                value_edges,
            ));
        }
        tables.push(Item {
            x: label_edges[0],
            end: value_edges[1],
            y,
            content: Content::Table {
                prefix: Vec::new(),
                rows,
                suffix: Vec::new(),
            },
        });
    }
    tables
}

fn page_items(bands: Vec<Vec<Fragment>>, rules: &[HorizontalRule]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut bands = bands.into_iter().filter(|b| !b.is_empty()).peekable();
    while let Some(first) = bands.next() {
        if table_start(&first) && bands.peek().is_some_and(|row| table_row(row, &first)) {
            let x = first[0].x;
            let y = first[0].y;
            let mut end = first.last().unwrap().end;
            let mut previous_y = y;
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
                let (y, mut rows) = ruled_headings(&mut items, &first, rules);
                let mut prefix = Vec::new();
                if rows.is_empty() {
                    let (preceding, heading) = table_heading(&mut items, &first);
                    prefix = preceding;
                    rows.extend(heading);
                }
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
                    content: Content::Table {
                        prefix,
                        rows,
                        suffix: Vec::new(),
                    },
                });
                continue;
            }
        }
        items.extend(first.into_iter().map(as_item));
    }
    items
}

fn gutter(items: &[Item], verticals: &[VerticalRule]) -> Option<f32> {
    let line_items: Vec<_> = items
        .iter()
        .filter(|item| item.line_size().is_some())
        .collect();
    let top = line_items
        .iter()
        .map(|item| item.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let bottom = line_items
        .iter()
        .map(|item| item.y)
        .fold(f32::INFINITY, f32::min);
    let text_height = top - bottom;
    if text_height.is_finite() {
        let ruled_cut = verticals
            .iter()
            .filter_map(|rule| {
                let rule_height = rule.top - rule.bottom;
                if rule_height < (text_height * 0.6).max(120.) {
                    return None;
                }
                let (mut left, mut right, mut crossing) = (0, 0, 0);
                for item in &line_items {
                    if item.end <= rule.x - 2. {
                        left += 1;
                    } else if item.x >= rule.x + 2. {
                        right += 1;
                    } else if item.x < rule.x && item.end > rule.x {
                        crossing += 1;
                    }
                }
                let support = left.min(right);
                (support >= 3 && crossing < support * 2 + 4).then_some((
                    support,
                    rule_height.min(text_height),
                    rule.x,
                ))
            })
            .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
        if let Some((_, _, cut)) = ruled_cut {
            return Some(cut);
        }
    }

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

fn read_region(items: Vec<Item>, out: &mut Vec<Block>, depth: usize, verticals: &[VerticalRule]) {
    if items.is_empty() {
        return;
    }
    if depth < 8
        && let Some(cut) = gutter(&items, verticals)
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
                read_region(std::mem::take(&mut left), out, depth + 1, verticals);
                read_region(std::mem::take(&mut right), out, depth + 1, verticals);
                spanning.push(item);
            }
        }
        reflow(spanning, out);
        read_region(left, out, depth + 1, verticals);
        read_region(right, out, depth + 1, verticals);
    } else {
        reflow(items, out);
    }
}

fn continues(prev: &LineInfo, next: &LineInfo, right: f32) -> bool {
    let size = prev.size.max(next.size);
    let gap = prev.y - next.y;
    let indent = next.x - prev.x;
    let hanging_indent = prev.list_marker() && indent <= size * 2.;
    let hanging_label = hanging_indent && indent >= -size * 0.55 && !next.list_marker();
    let same_baseline_label = hanging_label && gap.abs() <= 1.15;
    if (gap < size * 0.55 && !same_baseline_label)
        || gap > size * 1.8
        || prev.size.min(next.size) < size * 0.85
        || prev.bold != next.bold
        || (indent > size * 0.55 && !prev.drop_cap && !hanging_indent)
        || indent < -size * 4.
        || next.list_marker()
    {
        return false;
    }
    let filled = right - prev.end <= size * 2.4;
    let wraps_word =
        prev.last.ends_with(['-', '\u{ad}']) && next.first.starts_with(char::is_alphabetic);
    let sentence_end = prev.last.ends_with(['.', '!', '?', ':', '。', '！', '？']);
    let continuation = next.first.starts_with(char::is_lowercase) && !sentence_end;
    let korean_continuation = indent.abs() <= size * 0.55
        && prev
            .last
            .chars()
            .next_back()
            .is_some_and(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c))
        && next
            .first
            .chars()
            .next()
            .is_some_and(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c))
        && !sentence_end;
    wraps_word
        || hanging_label
        || korean_continuation
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
    discretionary_hyphen: bool,
    first: String,
    last: String,
}

impl LineInfo {
    fn list_marker(&self) -> bool {
        self.first.starts_with(['•', '▪', '■', '◦'])
            || self
                .first
                .chars()
                .next()
                .is_some_and(|c| ('①'..='⑳').contains(&c) || ('❶'..='❿').contains(&c))
            || self.first.strip_suffix(['.', ')']).is_some_and(|n| {
                !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit())
            })
    }

    fn new(item: &Item, runs: &[Run], size: f32, discretionary_hyphen: bool) -> Self {
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
            discretionary_hyphen,
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

fn join_wrapped_line(runs: &mut Vec<Run>, incoming: Vec<Run>, discretionary_hyphen: bool) {
    let mut space = true;
    let next = incoming
        .iter()
        .find_map(|r| r.text.split_whitespace().next());
    if let Some(last) = runs.last_mut()
        && let Some(next) = next
        && let Some(after) = next.chars().next()
    {
        if last.text.ends_with('\u{ad}')
            || (discretionary_hyphen && last.text.ends_with('-') && after.is_alphabetic())
        {
            last.text.pop();
            space = false;
        } else if let Some(stem) = last
            .text
            .split_whitespace()
            .next_back()
            .and_then(|t| t.strip_suffix('-'))
            && stem.ends_with(char::is_alphanumeric)
            && after.is_alphanumeric()
        {
            // Compound fragments often already contain a hyphen, digit or
            // spelled-out number. Keep their visible hyphen when joining.
            let compound = stem.contains('-')
                || next.trim_end_matches(['-', '\u{ad}']).contains('-')
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
    let mut previous: Option<LineInfo> = None;
    // Start x, top baseline and the widest end of the paragraph on top of
    // `out`: a merged paragraph has to keep the box of every line it holds.
    let mut bounds: Option<(f32, f32, f32)> = None;
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
                let Content::Line {
                    runs,
                    discretionary_hyphen,
                    ..
                } = &item.content
                else {
                    unreachable!()
                };
                Some(LineInfo::new(&item, runs, item_size, *discretionary_hyphen))
            }
            _ => None,
        };
        let x = item.x;
        let y = item.y;
        match item.content {
            Content::Line { runs, size, .. } => {
                let info = info.unwrap();
                // One visual row is frequently emitted as several text objects.
                // Keep them on one line instead of starting a paragraph per object.
                let same_row = previous.as_ref().is_some_and(|prev| {
                    (prev.y - info.y).abs() <= 1.2 && info.x >= prev.x - size * 2.6
                }) && matches!(out.last(), Some(Block::Paragraph { .. }));
                if same_row {
                    let (x, end, row_y, row_size, bold, first, gap) = {
                        let prev = previous.as_ref().unwrap();
                        (
                            prev.x,
                            prev.end.max(info.end),
                            prev.y,
                            prev.size.max(info.size),
                            prev.bold,
                            prev.first.clone(),
                            info.x - prev.end,
                        )
                    };
                    // Earlier rows may already be merged, so measure the box
                    // from the first line instead of restarting at this row.
                    // The widest end of the earlier rows has to survive as well.
                    // A fragment of this row may also start left of the first one.
                    let (old_left, top, old_end) = bounds.unwrap_or((x, row_y, end));
                    // `previous` keeps this row's own right edge so that the gap
                    // to the next fragment is still measured inside the row.
                    let row_end = end;
                    let left = old_left.min(info.x);
                    let end = old_end.max(end);
                    let last = info.last.clone();
                    if let Some(Block::Paragraph {
                        runs: para,
                        properties,
                        ..
                    }) = out.last_mut()
                    {
                        join_runs(para, runs, gap > size * 0.12);
                        properties.insert("last_y_pt".into(), y.to_string());
                        properties.insert("x_pt".into(), left.to_string());
                        properties.insert("w_pt".into(), (end - left).max(0.).to_string());
                        properties.insert(
                            "h_pt".into(),
                            (top - row_y + row_size).max(0.).to_string(),
                        );
                    }
                    bounds = Some((left, top, end));
                    previous = Some(LineInfo {
                        x,
                        end: row_end,
                        y: row_y,
                        size: row_size,
                        bold,
                        drop_cap: false,
                        discretionary_hyphen: false,
                        first,
                        last,
                    });
                    continue;
                }
                let right = right_edges[idx.saturating_sub(4)..(idx + 5).min(right_edges.len())]
                    .iter()
                    .filter(|(_, s)| s.is_some_and(|s| (s - size).abs() < size * 0.15))
                    .map(|&(end, _)| end)
                    .fold(item.end, f32::max);
                if let Some(prev) = &previous
                    && continues(prev, &info, right)
                    && let Some(Block::Paragraph {
                        runs: para,
                        properties,
                        ..
                    }) = out.last_mut()
                {
                    join_wrapped_line(para, runs, prev.discretionary_hyphen);
                    // Merge the horizontal extent of every line: the top row keeps
                    // its origin, but an outdented continuation row must stay in.
                    let (old_left, top, old_end) = bounds.unwrap_or((prev.x, prev.y, prev.end));
                    let left = old_left.min(info.x);
                    let end = old_end.max(info.end);
                    bounds = Some((left, top, end));
                    properties.insert("x_pt".into(), left.to_string());
                    properties.insert("last_y_pt".into(), y.to_string());
                    properties.insert("w_pt".into(), (end - left).max(0.).to_string());
                    properties.insert(
                        "h_pt".into(),
                        (top - info.y + info.size).max(0.).to_string(),
                    );
                } else {
                    let mut properties = coordinates(x, y);
                    properties.insert("last_y_pt".into(), y.to_string());
                    properties.insert("w_pt".into(), (info.end - x).max(0.).to_string());
                    properties.insert("h_pt".into(), size.to_string());
                    bounds = Some((x, y, info.end));
                    out.push(Block::Paragraph {
                        heading: 0,
                        list: None,
                        runs,
                        properties,
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
                bounds = None;
                previous = None;
            }
            Content::Table {
                prefix,
                rows,
                suffix,
            } => {
                reflow(prefix, out);
                out.push(Block::Table { rows });
                reflow(suffix, out);
                bounds = None;
                previous = None;
            }
        }
    }
}

fn math_glyph(f: &Fragment) -> Glyph {
    Glyph {
        x: f.x,
        end: f.end,
        y: f.y,
        size: f.size,
        segments: f
            .runs
            .iter()
            .map(|r| Segment {
                text: r.text.clone(),
                script: match r.style.script.as_deref() {
                    Some("sup") => Some(Script::Sup),
                    Some("sub") => Some(Script::Sub),
                    _ => None,
                },
                latex: false,
            })
            .collect(),
    }
}

fn zone_root(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

/// Rebuilds HWP equation objects as single LaTeX runs. Only the equation font
/// is grouped, so surrounding prose keeps its own fragments and reading order.
fn math_zones(pieces: &mut Vec<Fragment>) {
    let candidates: Vec<usize> = (0..pieces.len())
        .filter(|&i| pieces[i].horizontal && pieces[i].size > 0. && hyhwp_eq_font(&pieces[i]))
        .collect();
    // Some PDF writers draw the same glyph twice (bold simulation or a shadow),
    // which would otherwise duplicate every token inside one formula.
    let text_of = |i: usize| -> String {
        pieces[i]
            .runs
            .iter()
            .map(|r| r.text.trim())
            .collect::<Vec<_>>()
            .join("")
    };
    let mut math: Vec<usize> = Vec::with_capacity(candidates.len());
    let mut duplicates: Vec<usize> = Vec::new();
    for i in candidates {
        let text = text_of(i);
        let duplicate = math.iter().any(|&k| {
            text_of(k) == text
                && (pieces[k].y - pieces[i].y).abs() < 0.6
                && (pieces[k].x - pieces[i].x).abs() < 1.5
        });
        if duplicate {
            duplicates.push(i);
        } else {
            math.push(i);
        }
    }
    if math.len() < 2 && duplicates.is_empty() {
        return;
    }
    let glyphs: Vec<Glyph> = math.iter().map(|&i| math_glyph(&pieces[i])).collect();
    let mut parent: Vec<usize> = (0..math.len()).collect();
    for a in 0..math.len() {
        for b in (a + 1)..math.len() {
            let (left, right) = (&glyphs[a], &glyphs[b]);
            let size = left.size.max(right.size);
            let gap = (right.x - left.end).max(left.x - right.end);
            if (left.y - right.y).abs() <= size * 2.5 && gap <= size * 2. {
                let (ra, rb) = (zone_root(&mut parent, a), zone_root(&mut parent, b));
                parent[ra] = rb;
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for a in 0..math.len() {
        let root = zone_root(&mut parent, a);
        groups.entry(root).or_default().push(a);
    }
    let mut consumed = Vec::new();
    let mut rebuilt = Vec::new();
    for members in groups.into_values() {
        if members.len() < 2 || members.len() > 40 {
            continue;
        }
        let items: Vec<Glyph> = members.iter().map(|&m| glyphs[m].clone()).collect();
        let Some(latex) = to_latex(&items) else {
            continue;
        };
        // A rebuilt formula that dropped glyphs is worse than the plain text
        // that was extracted from the same page, so it is discarded.
        let alnum = |text: &str| text.chars().filter(|c| c.is_alphanumeric()).count();
        let plain: String = items
            .iter()
            .flat_map(|g| g.segments.iter().map(|s| s.text.as_str()))
            .collect();
        if alnum(&latex) + 2 < alnum(&plain) || latex.len() > 320 {
            continue;
        }
        let x = items.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
        let end = items
            .iter()
            .map(|g| g.end)
            .fold(f32::NEG_INFINITY, f32::max);
        let size = items.iter().map(|g| g.size).fold(0., f32::max);
        let y = items
            .iter()
            .find(|g| g.size >= size - 0.01)
            .map_or(0., |g| g.y);
        let text: Vec<&str> = members
            .iter()
            .flat_map(|&m| pieces[math[m]].runs.iter().map(|r| r.text.as_str()))
            .collect();
        rebuilt.push(Fragment {
            x,
            end,
            y,
            size,
            horizontal: true,
            discretionary_hyphen: false,
            runs: vec![Run {
                latex: Some(latex),
                text: text.join(" "),
                ..Run::default()
            }],
        });
        consumed.extend(members.into_iter().map(|m| math[m]));
    }
    if rebuilt.is_empty() && duplicates.is_empty() {
        return;
    }
    consumed.extend(duplicates);
    consumed.sort_unstable();
    consumed.dedup();
    for i in consumed.into_iter().rev() {
        pieces.remove(i);
    }
    pieces.extend(rebuilt);
}

pub(super) fn layout(
    mut pieces: Vec<Fragment>,
    mut rules: Vec<HorizontalRule>,
    verticals: Vec<VerticalRule>,
    sec: &mut Section,
) {
    pieces.retain(|f| {
        let total: usize = f
            .runs
            .iter()
            .map(|r| r.text.chars().filter(|c| !c.is_whitespace()).count())
            .sum();
        let unknown: usize = f
            .runs
            .iter()
            .map(|r| r.text.chars().filter(|c| *c == '\u{fffd}').count())
            .sum();
        f.runs
            .iter()
            .any(|r| r.image.is_some() || !r.text.trim().is_empty())
            && (total == 0 || unknown * 2 < total)
    });
    math_zones(&mut pieces);
    let mut bands = baseline_bands(pieces);
    attach_labels(&mut bands);
    bands.retain(|band| !band.is_empty());
    rules.sort_by(|a, b| b.y.total_cmp(&a.y));
    let mut items = grid_tables(&mut bands, &rules, &verticals);
    bands.retain(|band| !band.is_empty());
    items.extend(parallel_value_tables(&mut bands));
    bands.retain(|band| !band.is_empty());
    items.extend(ruled_tables(&mut bands, &rules));
    items.extend(page_items(bands, &rules));
    items.sort_by(|a, b| {
        row_key(b.y)
            .cmp(&row_key(a.y))
            .then_with(|| a.x.total_cmp(&b.x))
    });
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
                read_region(std::mem::take(&mut region), &mut sec.blocks, 0, &verticals);
            }
        }
        region.push(item);
    }
    read_region(region, &mut sec.blocks, 0, &verticals);
}
