use super::{
    Cell, Content, Fragment, HorizontalRule, Item, Range, as_item, cells_in_rule, fragment_cell,
    fragment_text, horizontal_rule_lines, join_wrapped_line,
};

const EDGE_TOLERANCE: f32 = 1.5;

struct RowRule<'a> {
    bounds: HorizontalRule,
    segments: Vec<&'a HorizontalRule>,
}

fn rule_groups(rules: &[HorizontalRule]) -> Vec<Vec<RowRule<'_>>> {
    let mut groups: Vec<Vec<RowRule<'_>>> = Vec::new();
    for segments in horizontal_rule_lines(rules) {
        let mut bounds = *segments[0];
        bounds.right = segments.last().unwrap().right;
        if bounds.right - bounds.left < 30. || bounds.width > 3. {
            continue;
        }
        let line = RowRule { bounds, segments };
        if let Some(group) = groups.iter_mut().rev().find(|group| {
            let first = &group[0].bounds;
            group.last().unwrap().bounds.y - bounds.y > 0.25
                && (first.left - bounds.left).abs() <= EDGE_TOLERANCE
                && (first.right - bounds.right).abs() <= EDGE_TOLERANCE
        }) {
            group.push(line);
        } else {
            groups.push(vec![line]);
        }
    }
    groups
}

fn is_caption(fragment: &Fragment) -> bool {
    let text = fragment_text(fragment);
    let mut words = text.split_whitespace();
    words
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case("table"))
        && words
            .next()
            .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_digit()))
}

fn caption_lines(
    bands: &[Vec<Fragment>],
    first: usize,
    top: &HorizontalRule,
) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    for (i, band) in bands[..first].iter().enumerate().rev() {
        let j = band.partition_point(|f| f.x < top.left - EDGE_TOLERANCE);
        let Some(f) = band.get(j).filter(|f| f.x <= top.left + EDGE_TOLERANCE) else {
            continue;
        };
        let next_y = lines
            .last()
            .map_or(top.y, |&(i, j): &(usize, usize)| bands[i][j].y);
        if !f.horizontal || f.end > top.right + 2.5 || f.y - next_y > f.size * 1.65 {
            break;
        }
        lines.push((i, j));
        if is_caption(f) {
            lines.reverse();
            return lines;
        }
        if lines.len() == 3 {
            break;
        }
    }
    Vec::new()
}

type Row = Vec<(usize, Range<usize>)>;

fn contents_between(
    bands: &[Vec<Fragment>],
    baselines: &[f32],
    top: &HorizontalRule,
    bottom: &HorizontalRule,
) -> Option<Row> {
    let first = baselines.partition_point(|&y| y > top.y);
    let last = baselines.partition_point(|&y| y > bottom.y);
    let mut previous_y = top.y;
    let mut row = Vec::new();
    let mut size = 0.;
    for (i, band) in bands.iter().enumerate().take(last).skip(first) {
        let range = cells_in_rule(band, top);
        let line = &band[range.clone()];
        let Some(f) = line.first() else { continue };
        size = line.iter().map(|f| f.size).fold(0., f32::max);
        if previous_y - f.y > size * 1.8
            || line.iter().any(|f| {
                !f.horizontal
                    || f.end > top.right.max(bottom.right) + 2.5
                    || (f.x <= top.left + EDGE_TOLERANCE && is_caption(f))
            })
        {
            return None;
        }
        previous_y = f.y;
        row.push((i, range));
    }
    (!row.is_empty() && previous_y - bottom.y <= size * 1.2).then_some(row)
}

fn build_table(
    bands: &mut [Vec<Fragment>],
    baselines: &[f32],
    lines: &[RowRule<'_>],
    contents: &[Row],
) -> Option<Item> {
    let top = &lines[0].bounds;
    let captions = caption_lines(bands, baselines.partition_point(|&y| y > top.y), top);
    let template = lines.iter().find(|line| line.segments.len() > 1);
    let columns = template.map_or(1, |line| line.segments.len());
    if (captions.is_empty() && (columns == 1 || contents.len() < 3))
        || columns > 32
        || contents.len().saturating_mul(columns) > crate::model::CELL_LIMIT
    {
        return None;
    }
    let mut cuts = Vec::new();
    if let Some(template) = template {
        let mut repeats = 0;
        for line in lines.iter().filter(|line| line.segments.len() > 1) {
            if line.segments.len() != columns
                || line.segments.iter().zip(&template.segments).any(|(a, b)| {
                    (a.left - b.left).abs() > EDGE_TOLERANCE
                        || (a.right - b.right).abs() > EDGE_TOLERANCE
                })
            {
                return None;
            }
            repeats += 1;
        }
        if repeats < 2 {
            return None;
        }
        cuts.extend(template.segments.iter().skip(1).map(|rule| rule.left));
    }

    let is_header = contents[0]
        .iter()
        .flat_map(|(i, range)| &bands[*i][range.clone()])
        .flat_map(|f| &f.runs)
        .filter(|r| !r.text.trim().is_empty())
        .all(|r| r.style.italic);
    let column_at = |f: &Fragment| cuts.partition_point(|&cut| f.x + 0.75 >= cut);
    let merged_header = columns > 1
        && is_header
        && lines[0].segments.len() == 1
        && contents[0]
            .iter()
            .all(|(i, range)| bands[*i][range.clone()].iter().all(|f| column_at(f) == 0));
    let mut rows_per_column = vec![0; columns];
    for (row, ranges) in contents.iter().enumerate() {
        if row == 0 && merged_header {
            continue;
        }
        let mut seen = vec![false; columns];
        for f in ranges
            .iter()
            .flat_map(|(i, range)| &bands[*i][range.clone()])
        {
            let column = column_at(f);
            if cuts.get(column).is_some_and(|cut| f.end > cut + 2.5) {
                return None;
            }
            seen[column] = true;
        }
        for (count, seen) in rows_per_column.iter_mut().zip(seen) {
            *count += usize::from(seen);
        }
    }
    if rows_per_column.iter().any(|&n| n < 2) {
        return None;
    }

    let left = top.left;
    let right = top.right;
    let mut rows = Vec::new();
    for (row, ranges) in contents.iter().enumerate() {
        let merged = row == 0 && merged_header;
        let mut cells: Vec<Vec<Fragment>> = (0..if merged { 1 } else { columns })
            .map(|_| Vec::new())
            .collect();
        for (i, range) in ranges {
            for f in bands[*i].drain(range.clone()) {
                cells[column_at(&f)].push(f);
            }
        }
        let mut cells: Vec<Cell> = cells.into_iter().map(|f| fragment_cell(f, true)).collect();
        for (col, cell) in cells.iter_mut().enumerate() {
            let left = col.checked_sub(1).map_or(left, |i| cuts[i]);
            let right = cuts.get(col).copied().filter(|_| !merged).unwrap_or(right);
            crate::formatting::points(&mut cell.properties, "width", Some(f64::from(right - left)));
            let border =
                |line: &RowRule<'_>| line.segments.get(col).unwrap_or(&line.segments[0]).border();
            if row == 0 {
                cell.properties
                    .insert("border-top".into(), border(&lines[0]));
                if is_header {
                    cell.properties.insert("table_header".into(), "true".into());
                } else if col == 0 {
                    cell.properties
                        .insert("table_headerless".into(), "true".into());
                }
            }
            cell.properties
                .insert("border-bottom".into(), border(&lines[row + 1]));
        }
        if merged {
            cells[0].colspan = Some(columns);
        }
        rows.push(cells);
    }

    let mut prefix = Vec::new();
    let mut caption = captions.into_iter().map(|(i, j)| bands[i].remove(j));
    if let Some(mut first) = caption.next() {
        for line in caption {
            join_wrapped_line(&mut first.runs, line.runs, first.discretionary_hyphen);
            first.end = first.end.max(line.end);
            first.discretionary_hyphen = line.discretionary_hyphen;
        }
        prefix.push(as_item(first));
    }
    Some(Item {
        x: left,
        end: right,
        y: prefix.first().map_or(top.y, |caption| caption.y),
        content: Content::Table {
            prefix,
            rows,
            suffix: Vec::new(),
        },
    })
}

pub(super) fn tables(bands: &mut [Vec<Fragment>], rules: &[HorizontalRule]) -> Vec<Item> {
    let baselines: Vec<_> = bands.iter().map(|b| b[0].y).collect();
    let mut tables = Vec::new();
    for group in rule_groups(rules)
        .into_iter()
        .filter(|group| group.len() >= 3)
    {
        let mut rows = Vec::new();
        for (index, pair) in group.windows(2).enumerate() {
            let row = contents_between(bands, &baselines, &pair[0].bounds, &pair[1].bounds);
            let end = index + usize::from(row.is_some());
            if let Some(row) = row {
                rows.push(row);
                if end + 1 < group.len() {
                    continue;
                }
            }
            if rows.len() >= 2
                && let Some(table) =
                    build_table(bands, &baselines, &group[end - rows.len()..=end], &rows)
            {
                tables.push(table);
            }
            rows.clear();
        }
    }
    tables
}
