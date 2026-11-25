use super::Cursor;
use crate::model::PART_LIMIT;
use std::fmt::Write;

pub(crate) const SVG_START: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\"";
const LIMIT: usize = 16_384;

pub(super) struct Drawing {
    pub bounds: [i32; 4],
    pub order: i32,
    pub svg: Vec<u8>,
    pub partial: bool,
}

fn int(data: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn chunk<'a>(cursor: &mut Cursor<'a>) -> Option<&'a [u8]> {
    let size = usize::try_from(cursor.u32().ok()?).ok()?;
    cursor.take(size).ok()
}

fn color(value: i32) -> String {
    if (0..=0xFFFFFF).contains(&value) {
        format!(
            "#{:02x}{:02x}{:02x}",
            value & 255,
            value >> 8 & 255,
            value >> 16
        )
    } else {
        "none".into()
    }
}

pub(super) fn parse(bytes: &[u8]) -> Option<Drawing> {
    let mut data = Cursor::new(bytes);
    let header = chunk(&mut data)?;
    let order = int(header, 0)?;
    let bounds = [
        int(header, 8)?,
        int(header, 12)?,
        int(header, 16)?,
        int(header, 20)?,
    ];
    let [x, y, w, h] = bounds;
    if w <= 0 || h <= 0 || bounds.iter().any(|v| v.unsigned_abs() > 1_000_000) {
        return None;
    }
    let mut svg = format!(
        "{SVG_START} viewBox=\"{x} {y} {w} {h}\" width=\"{}pt\" height=\"{}pt\" overflow=\"visible\">",
        f64::from(w) / 25.,
        f64::from(h) / 25.
    );
    let start = svg.len();
    let mut count = 0;
    let mut partial = false;
    read_shapes(&mut data, &mut svg, &mut count, &mut partial, 0)?;
    if svg.len() == start {
        return None;
    }
    svg.push_str("</svg>");
    Some(Drawing {
        bounds,
        order,
        svg: svg.into_bytes(),
        partial,
    })
}

fn read_shapes(
    data: &mut Cursor<'_>,
    svg: &mut String,
    count: &mut usize,
    partial: &mut bool,
    depth: usize,
) -> Option<()> {
    if depth >= 32 {
        return None;
    }
    loop {
        *count += 1;
        if *count > LIMIT || svg.len() >= PART_LIMIT {
            return None;
        }
        let header = chunk(data)?;
        let kind = super::read_u16_at(header, 0).ok()?;
        let links = super::read_u16_at(header, 2).ok()?;
        let flags = int(header, 84)?;
        // Drawing text adds an embedded paragraph list before the private records.
        if flags & 0x80000 != 0 {
            return None;
        }
        let first = chunk(data)?;
        let second = chunk(data)?;
        if kind != 0 && draw_shape(svg, kind, header, first, second).is_none() {
            *partial = true;
        }
        *partial |= int(header, 68)? != 0 || flags & 0x40000 != 0;
        if links & 2 != 0 {
            read_shapes(data, svg, count, partial, depth + 1)?;
        }
        if links & 1 == 0 {
            return Some(());
        }
    }
}

fn draw_shape(
    out: &mut String,
    kind: u16,
    header: &[u8],
    first: &[u8],
    second: &[u8],
) -> Option<()> {
    let (w, h) = (int(header, 12)?, int(header, 16)?);
    let (x, y) = (int(header, 20)?, int(header, 24)?);
    if [x, y, w, h].iter().any(|v| v.unsigned_abs() > 1_000_000) || w < 0 || h < 0 {
        return None;
    }
    let flags = int(header, 84)?;
    let stroke = color(int(header, 56)?);
    let width = int(header, 60)?.clamp(0, 10_000);
    let fill = color(int(header, 64)?);
    let mut attributes = format!("stroke=\"{stroke}\" stroke-width=\"{width}\" fill=\"{fill}\"");
    if let Some((dash, gap)) = match int(header, 44)? {
        1 => Some((3.4, 2.72)),
        2 => Some((1.7, 1.36)),
        3 => Some((6.12, 1.36)),
        4 => Some((8.5, 1.36)),
        _ => None,
    } {
        let w = f64::from(width);
        write!(attributes, " stroke-dasharray=\"{} {}\"", dash * w, gap * w).ok()?;
    }
    let transform = if flags & 0x20000 != 0 && matches!(kind, 2 | 3 | 8) && w > 0 && h > 0 {
        let (x0, y0) = (int(header, 96)?, int(header, 100)?);
        let (x1, y1) = (int(header, 104)?, int(header, 108)?);
        let (x2, y2) = (int(header, 112)?, int(header, 116)?);
        let f = |a, b| f64::from(a) - f64::from(b);
        format!(
            "matrix({} {} {} {} {} {})",
            f(x1, x0) / f64::from(w),
            f(y1, y0) / f64::from(w),
            f(x2, x1) / f64::from(h),
            f(y2, y1) / f64::from(h),
            i64::from(x) + i64::from(x0),
            i64::from(y) + i64::from(y0)
        )
    } else {
        format!("translate({x} {y})")
    };
    let shape = match kind {
        1 => {
            let flip = int(first, 0)?;
            let (x1, x2) = if flip & 1 == 0 { (0, w) } else { (w, 0) };
            let (y1, y2) = if flip & 2 == 0 { (0, h) } else { (h, 0) };
            format!("<line x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\"/>")
        }
        2 => {
            let radius = if flags & 1 != 0 {
                w.min(h) / 10
            } else if flags & 4 != 0 {
                w.min(h) / 2
            } else {
                0
            };
            format!("<rect width=\"{w}\" height=\"{h}\" rx=\"{radius}\"/>")
        }
        3 => {
            let (rx, ry) = (f64::from(w) / 2., f64::from(h) / 2.);
            format!("<ellipse cx=\"{rx}\" cy=\"{ry}\" rx=\"{rx}\" ry=\"{ry}\"/>")
        }
        5 | 7 | 10 => {
            let count = usize::try_from(int(first, 0)?).ok()?;
            if !(2..=LIMIT).contains(&count) {
                return None;
            }
            let points = second
                .get(..count * 8)?
                .as_chunks::<8>()
                .0
                .iter()
                .map(|p| Some([f64::from(int(p, 0)?), f64::from(int(p, 4)?)]))
                .collect::<Option<Vec<_>>>()?;
            let closed =
                kind == 10 || points.first() == points.last() || kind == 7 && flags & 0x20 != 0;
            let path = path(&points, kind == 7, closed);
            format!("<path d=\"{path}\"/>")
        }
        _ => return None,
    };
    write!(out, "<g transform=\"{transform}\" {attributes}>{shape}</g>").ok()
}

fn path(points: &[[f64; 2]], curve: bool, closed: bool) -> String {
    let (mut points, mut result) = (points, String::new());
    if closed && points.len() > 2 && points.first() == points.last() {
        points = &points[..points.len() - 1];
    }
    let [x, y] = points[0];
    let _ = write!(result, "M{x} {y}");
    if curve && points.len() > 2 {
        let tangents = tangents(points, closed);
        let segments = points.len() - usize::from(!closed);
        for i in 0..segments {
            let j = (i + 1) % points.len();
            let [x, y] = points[j];
            let [x1, y1] = std::array::from_fn(|k| points[i][k] + tangents[i][k] / 3.);
            let [x2, y2] = std::array::from_fn(|k| points[j][k] - tangents[j][k] / 3.);
            let _ = write!(result, "C{x1:.3} {y1:.3} {x2:.3} {y2:.3} {x} {y}");
        }
    } else {
        for [x, y] in &points[1..] {
            let _ = write!(result, "L{x} {y}");
        }
    }
    if closed {
        result.push('Z');
    }
    result
}

fn tangents(points: &[[f64; 2]], closed: bool) -> Vec<[f64; 2]> {
    let n = points.len();
    let mut rhs: Vec<[f64; 2]> = (0..n)
        .map(|i| {
            let prev = if closed {
                (i + n - 1) % n
            } else {
                i.saturating_sub(1)
            };
            let next = if closed {
                (i + 1) % n
            } else {
                (i + 1).min(n - 1)
            };
            std::array::from_fn(|k| 3. * (points[next][k] - points[prev][k]))
        })
        .collect();
    if closed {
        // The periodic spline is diagonally dominant. Each Jacobi iteration
        // at least halves the error, including at the join.
        let mut tangents = vec![[0.; 2]; n];
        let mut next = tangents.clone();
        for _ in 0..32 {
            for (i, pair) in next.iter_mut().enumerate() {
                for (k, value) in pair.iter_mut().enumerate() {
                    *value =
                        (rhs[i][k] - tangents[(i + n - 1) % n][k] - tangents[(i + 1) % n][k]) / 4.;
                }
            }
            std::mem::swap(&mut tangents, &mut next);
        }
        return tangents;
    }
    // Solve the tridiagonal system with natural boundary conditions.
    let mut upper = vec![0.; n];
    upper[0] = 0.5;
    rhs[0] = rhs[0].map(|v| v / 2.);
    for i in 1..n {
        let diagonal = if i == n - 1 { 2. } else { 4. } - upper[i - 1];
        upper[i] = 1. / diagonal;
        let previous = rhs[i - 1];
        for (value, previous) in rhs[i].iter_mut().zip(previous) {
            *value = (*value - previous) / diagonal;
        }
    }
    for i in (0..n - 1).rev() {
        let next = rhs[i + 1];
        for (value, next) in rhs[i].iter_mut().zip(next) {
            *value -= upper[i] * next;
        }
    }
    rhs
}
