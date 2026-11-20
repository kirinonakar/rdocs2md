use crate::model::*;
use lopdf::{Document as Pdf, LoadOptions, Object, content::Content};
use std::collections::HashMap;
#[derive(Clone, Copy)]
struct Matrix([f32; 6]);
impl Matrix {
    const ID: Self = Self([1., 0., 0., 1., 0., 0.]);
    fn point(self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }
    fn concat(self, right: Self) -> Self {
        let [a, b, c, d, e, f] = right.0;
        let [g, h, i, j, _, _] = self.0;
        let (x, y) = self.point(e, f);
        Self([
            g * a + i * b,
            h * a + j * b,
            g * c + i * d,
            h * c + j * d,
            x,
            y,
        ])
    }
    fn shift(self, x: f32, y: f32) -> Self {
        self.concat(Self([1., 0., 0., 1., x, y]))
    }
}
mod layout;
use layout::{Fragment, HorizontalRule, approximate_width, layout};

const PDF_RULE_LIMIT: usize = 4096;
const TRACKED_GLYPH_LIMIT: usize = 65536;

#[derive(Clone, Copy)]
struct TextState {
    size: f32,
    leading: f32,
    char_spacing: f32,
    word_spacing: f32,
    scale: f32,
    rise: f32,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            size: 0.,
            leading: 0.,
            char_spacing: 0.,
            word_spacing: 0.,
            scale: 1.,
            rise: 0.,
        }
    }
}

fn resolve_object<'a>(pdf: &'a Pdf, obj: &'a Object) -> Option<&'a Object> {
    match obj {
        Object::Reference(id) => pdf.get_object(*id).ok(),
        _ => Some(obj),
    }
}

fn math_pi_encoding(pdf: &Pdf, font: &lopdf::Dictionary) -> Option<HashMap<u8, char>> {
    let base = font.get(b"BaseFont").ok()?.as_name().ok()?;
    if !base.ends_with(b"Universal-GreekwithMathPi") {
        return None;
    }
    let encoding = dictionary(pdf, font.get(b"Encoding").ok()?)?;
    let differences = encoding.get(b"Differences").ok()?.as_array().ok()?;
    let mut code = 0u8;
    let mut mapping = HashMap::new();
    for value in differences {
        if let Ok(n) = value.as_i64() {
            code = u8::try_from(n).ok()?;
        } else if let Ok(name) = value.as_name() {
            let character = match name {
                b"H11001" => '+',
                b"H11002" => '−',
                b"H11003" => '×',
                b"H11005" => '=',
                b"H11021" => '<',
                b"H11022" => '>',
                b"H9260" => 'κ',
                _ => '\u{fffd}',
            };
            mapping.insert(code, character);
            code = code.wrapping_add(1);
        }
    }
    Some(mapping)
}

struct FontMetrics<'a> {
    widths: &'a [Object],
    first: i64,
    cid_default: Option<f32>,
    cid_widths: Vec<CidWidth<'a>>,
}

struct CidWidth<'a> {
    first: i64,
    last: i64,
    widths: &'a [Object],
    constant: f32,
}

fn cid_widths(spec: &[Object]) -> Vec<CidWidth<'_>> {
    let mut ranges = Vec::new();
    let mut items = spec.iter();
    while let Some(first) = items.next().and_then(|item| item.as_i64().ok()) {
        let Some(range) = items.next() else { break };
        if let Ok(widths) = range.as_array() {
            if !widths.is_empty() {
                ranges.push(CidWidth {
                    first,
                    last: first.saturating_add(widths.len() as i64 - 1),
                    widths,
                    constant: 0.,
                });
            }
        } else if let Ok(last) = range.as_i64()
            && let Some(constant) = items.next().and_then(|item| item.as_float().ok())
        {
            ranges.push(CidWidth {
                first,
                last,
                widths: &[],
                constant,
            });
        } else {
            break;
        }
    }
    ranges.sort_by_key(|range| range.first);
    ranges
}

impl<'a> FontMetrics<'a> {
    fn new(pdf: &'a Pdf, font: &'a lopdf::Dictionary) -> Self {
        let array = |dict: &'a lopdf::Dictionary, key: &[u8]| {
            resolve_object(pdf, dict.get(key).ok()?)?.as_array().ok()
        };
        if font.get(b"Encoding").and_then(Object::as_name).ok() == Some(b"Identity-H")
            && let Some(cid) = array(font, b"DescendantFonts")
                .and_then(|a| a.first())
                .and_then(|d| dictionary(pdf, d))
        {
            return Self {
                widths: &[],
                first: 0,
                cid_default: Some(cid.get(b"DW").and_then(Object::as_float).unwrap_or(1000.)),
                cid_widths: cid_widths(array(cid, b"W").map_or(&[], Vec::as_slice)),
            };
        }
        Self {
            widths: array(font, b"Widths").map_or(&[], Vec::as_slice),
            first: font.get(b"FirstChar").and_then(Object::as_i64).unwrap_or(0),
            cid_default: None,
            cid_widths: Vec::new(),
        }
    }

    fn advance(&self, bytes: &[u8], decoded: &str, state: TextState) -> f32 {
        let (width, count, spaces) = if let Some(default) = self.cid_default {
            let (codes, _) = bytes.as_chunks::<2>();
            let width: f32 = codes
                .iter()
                .map(|b| {
                    self.cid_width(i64::from(u16::from_be_bytes(*b)))
                        .unwrap_or(default)
                })
                .sum();
            (width / 1000., codes.len(), 0)
        } else {
            let width = bytes.iter().try_fold(0., |sum, &b| {
                let index = usize::try_from(i64::from(b).checked_sub(self.first)?).ok()?;
                Some(sum + self.widths.get(index)?.as_float().ok()?)
            });
            (
                width.map_or_else(|| approximate_width(decoded), |width| width / 1000.),
                bytes.len(),
                bytes.iter().filter(|&&b| b == b' ').count(),
            )
        };
        (width * state.size
            + count as f32 * state.char_spacing
            + spaces as f32 * state.word_spacing)
            * state.scale
    }

    fn cid_width(&self, code: i64) -> Option<f32> {
        let index = self.cid_widths.partition_point(|range| range.first <= code);
        let range = self.cid_widths.get(index.checked_sub(1)?)?;
        if code > range.last {
            return None;
        }
        if range.widths.is_empty() {
            return Some(range.constant);
        }
        let offset = usize::try_from(code.checked_sub(range.first)?).ok()?;
        range.widths.get(offset)?.as_float().ok()
    }
}
fn raster(stream: &lopdf::Stream) -> Result<Option<Vec<u8>>> {
    if image_ext(&stream.content).is_some() {
        return Ok(Some(stream.content.clone()));
    }
    let d = &stream.dict;
    if d.has(b"Decode")
        || d.has(b"SMask")
        || d.get(b"BitsPerComponent").and_then(Object::as_i64).ok() != Some(8)
    {
        return Ok(None);
    }
    let channels = match d.get(b"ColorSpace").and_then(Object::as_name) {
        Ok(b"DeviceRGB") => 3,
        Ok(b"DeviceGray") => 1,
        _ => return Ok(None),
    };
    let width = usize::try_from(d.get(b"Width")?.as_i64()?)?;
    let height = usize::try_from(d.get(b"Height")?.as_i64()?)?;
    let stride = width
        .checked_mul(3)
        .and_then(|n| n.checked_add(3))
        .ok_or("invalid PDF image dimensions")?
        & !3;
    let size = stride
        .checked_mul(height)
        .ok_or("PDF image size overflow")?;
    if width == 0 || height == 0 || size > PART_LIMIT {
        return Err("PDF raster dimensions exceed limit".into());
    }
    let Ok(raw) = stream.get_plain_content_with_limit(PART_LIMIT) else {
        return Ok(None);
    };
    if raw.len() != width * height * channels {
        return Ok(None);
    }
    let mut bmp = vec![0u8; 54 + size];
    bmp[..2].copy_from_slice(b"BM");
    for (pos, n) in [
        (2, 54 + size),
        (10, 54),
        (14, 40),
        (18, width),
        (22, height),
        (34, size),
    ] {
        bmp[pos..pos + 4].copy_from_slice(&(n as u32).to_le_bytes())
    }
    bmp[26] = 1;
    bmp[28] = 24;
    for (y, row) in raw.chunks_exact(width * channels).enumerate() {
        let start = 54 + (height - 1 - y) * stride;
        for (i, p) in row.chunks_exact(channels).enumerate() {
            let bgr = if channels == 3 {
                [p[2], p[1], p[0]]
            } else {
                [p[0]; 3]
            };
            bmp[start + 3 * i..start + 3 * i + 3].copy_from_slice(&bgr)
        }
    }
    Ok(Some(bmp))
}
fn annotations(pdf: &Pdf, id: lopdf::ObjectId, fragments: &mut [Fragment]) -> Result<()> {
    let list = pdf.get_page_annotations(id)?;
    if fragments.len().saturating_mul(list.len()) > 8_000_000 {
        return Err("PDF annotation matching limit exceeded".into());
    }
    for a in list {
        let typ = a
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or_default();
        if !matches!(typ, b"Link" | b"Underline" | b"StrikeOut") {
            continue;
        }
        let Some(rect) = a
            .get(b"Rect")
            .ok()
            .and_then(|o| o.as_array().ok())
            .filter(|a| a.len() == 4)
        else {
            continue;
        };
        let mut r = [0.; 4];
        for (i, n) in rect.iter().enumerate() {
            r[i] = n.as_float()?
        }
        let url = a
            .get(b"A")
            .ok()
            .and_then(|o| dictionary(pdf, o))
            .filter(|d| d.get(b"S").and_then(Object::as_name).ok() == Some(b"URI"))
            .and_then(|a| a.get(b"URI").ok())
            .and_then(|o| lopdf::decode_text_string(o).ok());
        for f in fragments.iter_mut().filter(|f| {
            f.x >= r[0].min(r[2]) - 1.
                && f.x <= r[0].max(r[2]) + 1.
                && f.y >= r[1].min(r[3]) - 1.
                && f.y <= r[1].max(r[3]) + 1.
        }) {
            for run in &mut f.runs {
                match typ {
                    b"Link" => run.link = url.clone(),
                    b"Underline" => run.style.underline = true,
                    b"StrikeOut" => run.style.strike = true,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
fn dictionary<'a>(pdf: &'a Pdf, obj: &'a Object) -> Option<&'a lopdf::Dictionary> {
    match obj {
        Object::Reference(id) => pdf.get_dictionary(*id).ok(),
        _ => obj.as_dict().ok(),
    }
}
fn resource_dict<'a>(
    pdf: &'a Pdf,
    resources: Option<&'a lopdf::Dictionary>,
    key: &[u8],
) -> Option<&'a lopdf::Dictionary> {
    dictionary(pdf, resources?.get(key).ok()?)
}
struct PageReader<'a> {
    pdf: &'a Pdf,
    doc: &'a mut Document,
    images: bool,
    total: &'a mut usize,
    fragments: Vec<Fragment>,
    rules: Vec<HorizontalRule>,
    active: Vec<lopdf::ObjectId>,
}
impl PageReader<'_> {
    fn content(
        &mut self,
        bytes: &[u8],
        resources: Option<&lopdf::Dictionary>,
        mut ctm: Matrix,
    ) -> Result<()> {
        *self.total += bytes.len() + 64;
        if *self.total > 512 * MIB {
            return Err("PDF content expansion limit exceeded".into());
        }
        let mut fonts = HashMap::new();
        if let Some(dict) = resource_dict(self.pdf, resources, b"Font") {
            for (key, obj) in dict.iter() {
                if let Some(font) = dictionary(self.pdf, obj) {
                    fonts.insert(key.clone(), font);
                }
            }
        }
        let mut enc = HashMap::new();
        let mut math_pi = HashMap::new();
        let mut metrics = HashMap::new();
        for (key, font) in &fonts {
            if !font.has(b"ToUnicode")
                && let Some(mapping) = math_pi_encoding(self.pdf, font)
            {
                math_pi.insert(key.clone(), mapping);
            } else if let Ok(e) = font.get_font_encoding_with_limit(self.pdf, 4 * MIB) {
                enc.insert(key.clone(), e);
            }
            if enc.contains_key(key) || math_pi.contains_key(key) {
                metrics.insert(key.clone(), FontMetrics::new(self.pdf, font));
            }
        }

        let mut stack = Vec::new();
        let mut line = Matrix::ID;
        let mut position = line;
        let mut font = Vec::new();
        let mut state = TextState::default();
        let mut path_point: Option<(f32, f32)> = None;
        let mut path_rules = Vec::new();

        let mut moved = true;
        for op in Content::decode_iter(bytes) {
            let op = op?;
            let args = &op.operands;
            let num = |i: usize| {
                args.get(i)
                    .ok_or("PDF operand missing")
                    .and_then(|v| v.as_float().map_err(|_| "invalid PDF numeric operand"))
            };
            match op.operator.as_str() {
                "q" => {
                    if stack.len() > 128 {
                        return Err("PDF graphics state depth exceeded".into());
                    }
                    stack.push((ctm, font.clone(), state))
                }
                "Q" => {
                    if let Some((m, f, s)) = stack.pop() {
                        ctm = m;
                        font = f;
                        state = s;
                        moved = true;
                    }
                }
                "cm" | "Tm" => {
                    let matrix = Matrix([num(0)?, num(1)?, num(2)?, num(3)?, num(4)?, num(5)?]);
                    if op.operator == "cm" {
                        ctm = ctm.concat(matrix)
                    } else {
                        line = matrix;
                        position = line;
                    }
                    moved = true
                }
                "m" | "l" => {
                    let next = num(0).ok().zip(num(1).ok()).map(|(x, y)| ctm.point(x, y));
                    if op.operator == "l"
                        && path_rules.len() + self.rules.len() < PDF_RULE_LIMIT
                        && let (Some((x, y)), Some((end, end_y))) = (path_point, next)
                        && x.is_finite()
                        && end.is_finite()
                        && (y - end_y).abs() <= 0.5
                        && (x - end).abs() >= 10.
                    {
                        path_rules.push(HorizontalRule {
                            left: x.min(end),
                            right: x.max(end),
                            y,
                        });
                    }
                    path_point = next;
                }
                "c" | "v" | "y" | "h" | "re" => path_point = None,
                "S" | "s" | "B" | "B*" | "b" | "b*" => {
                    let remaining = PDF_RULE_LIMIT - self.rules.len();
                    self.rules.extend(path_rules.drain(..).take(remaining));
                    path_point = None;
                }
                "f" | "F" | "f*" | "n" => {
                    path_rules.clear();
                    path_point = None;
                }
                "BT" => {
                    line = Matrix::ID;
                    position = line;
                    moved = true
                }
                "Tf" => {
                    font = args.first().ok_or("PDF font missing")?.as_name()?.to_vec();
                    state.size = num(1)?
                }
                "TL" => state.leading = num(0)?,
                "Tc" => state.char_spacing = num(0)?,
                "Tw" => state.word_spacing = num(0)?,
                "Tz" => state.scale = num(0)? / 100.,
                "Ts" => {
                    state.rise = num(0)?;
                    moved = true;
                }
                "Td" | "TD" => {
                    let dy = num(1)?;
                    line = line.shift(num(0)?, dy);
                    if op.operator == "TD" {
                        state.leading = -dy
                    }
                    position = line;
                    moved = true
                }
                "T*" => {
                    line = line.shift(0., -state.leading);
                    position = line;
                    moved = true
                }
                "Tj" | "TJ" | "'" | "\"" => {
                    if matches!(op.operator.as_str(), "'" | "\"") {
                        if op.operator == "\"" {
                            state.word_spacing = num(0)?;
                            state.char_spacing = num(1)?;
                        }
                        line = line.shift(0., -state.leading);
                        position = line;
                        moved = true
                    }
                    let Some(font_metrics) = metrics.get(&font) else {
                        self.doc
                            .warn("Text using an unmapped PDF font was skipped.");
                        continue;
                    };
                    let obj = args.last().ok_or("PDF text operand missing")?;
                    let parts = if let Object::Array(a) = obj {
                        a.as_slice()
                    } else {
                        std::slice::from_ref(obj)
                    };
                    let mut text = String::new();
                    let mut segments = Vec::new();
                    let mut start = 0.;
                    let mut advance = 0.;
                    let mut pending_space = false;
                    let mut decimal_gap = None;
                    // Some tables space individual glyphs an entire column apart
                    // with Tc, even when several cells share one PDF string.
                    let tracking = state.char_spacing * state.scale;
                    let separate_glyphs = state.scale > 0. && state.char_spacing > state.size.abs();
                    let code_width = if font_metrics.cid_default.is_some() {
                        2
                    } else {
                        1
                    };
                    for part in parts {
                        if let Ok(b) = part.as_str() {
                            let decoded = if let Some(e) = enc.get(&font) {
                                Pdf::decode_text(e, b)?
                            } else {
                                let mapping = &math_pi[&font];
                                b.iter()
                                    .map(|code| mapping.get(code).copied().unwrap_or('\u{fffd}'))
                                    .collect()
                            };
                            if let Some(end) = decimal_gap.take()
                                && decoded.trim().split_once('.').is_some_and(
                                    |(whole, fraction)| {
                                        !whole.is_empty()
                                            && !fraction.is_empty()
                                            && whole
                                                .bytes()
                                                .chain(fraction.bytes())
                                                .all(|b| b.is_ascii_digit())
                                    },
                                )
                            {
                                segments.push((std::mem::take(&mut text), start, end));
                            }
                            if separate_glyphs
                                && self.fragments.len() + segments.len() + b.len() / code_width
                                    <= TRACKED_GLYPH_LIMIT
                                && decoded.chars().count() * code_width == b.len()
                            {
                                if !text.is_empty() {
                                    segments.push((std::mem::take(&mut text), start, advance));
                                }
                                for (code, glyph) in b.chunks_exact(code_width).zip(decoded.chars())
                                {
                                    let start = advance;
                                    let glyph = glyph.to_string();
                                    advance += font_metrics.advance(code, &glyph, state);
                                    segments.push((glyph, start, advance - tracking));
                                }
                                pending_space = false;
                                continue;
                            }
                            if text.is_empty() {
                                start = advance;
                            }
                            if pending_space
                                && !text.is_empty()
                                && !text.ends_with(char::is_whitespace)
                                && !decoded.starts_with(char::is_whitespace)
                            {
                                text.push(' ');
                            }
                            text.push_str(&decoded);
                            advance += font_metrics.advance(b, &decoded, state);
                            pending_space = false;
                        } else if let Ok(offset) = part.as_float() {
                            if offset < -1100. && !text.is_empty() {
                                segments.push((std::mem::take(&mut text), start, advance));
                                decimal_gap = None;
                            } else if offset < -800. && !text.is_empty() && decimal_gap.is_none() {
                                // Decimal columns can be closer than the general text split.
                                decimal_gap = Some(advance);
                            }
                            advance -= offset * state.size * state.scale / 1000.;
                            pending_space |= offset < -120.;
                        }
                    }
                    let basis = ctm.concat(position);
                    position = position.shift(advance, 0.);
                    if !text.is_empty() {
                        segments.push((text, start, advance));
                    }
                    let base = fonts
                        .get(&font)
                        .and_then(|f| f.get(b"BaseFont").ok())
                        .and_then(|n| n.as_name().ok())
                        .map(|n| String::from_utf8_lossy(n).into_owned());
                    let lower = base.as_deref().unwrap_or("").to_ascii_lowercase();
                    let effective_size = state.size.abs() * basis.0[2].hypot(basis.0[3]);
                    let style = Style {
                        bold: lower.contains("bold") || lower.contains("heavy"),
                        italic: lower.contains("italic") || lower.contains("oblique"),
                        font: base,
                        size: Some(format!("{effective_size}pt")),
                        ..Style::default()
                    };
                    for (text, start, end) in segments {
                        let (x, y) = basis.point(start, state.rise);
                        let (end, _) = basis.point(end, state.rise);
                        if !x.is_finite()
                            || !y.is_finite()
                            || !end.is_finite()
                            || !effective_size.is_finite()
                        {
                            return Err("non-finite PDF coordinates".into());
                        }
                        let run = Run {
                            text,
                            style: style.clone(),
                            ..Run::default()
                        };
                        if !moved
                            && !separate_glyphs
                            && start == 0.
                            && let Some(f) = self.fragments.last_mut()
                        {
                            f.end = f.end.max(end);
                            push_run(&mut f.runs, run);
                        } else {
                            self.fragments.push(Fragment {
                                x,
                                end: x.max(end),
                                y,
                                size: effective_size,
                                horizontal: basis.0[0] > 0. && basis.0[1].abs() < basis.0[0] * 0.15,
                                runs: vec![run],
                            });
                        }
                        moved = separate_glyphs;
                    }
                }
                "Do" => {
                    let key = args.first().ok_or("PDF XObject name missing")?.as_name()?;
                    if let Some(obj) =
                        resource_dict(self.pdf, resources, b"XObject").and_then(|d| d.get(key).ok())
                    {
                        let id = obj.as_reference()?;
                        let stream = self.pdf.get_object(id)?.as_stream()?;
                        match stream.dict.get(b"Subtype")?.as_name()? {
                            b"Form" => {
                                if self.active.len() >= 32 || self.active.contains(&id) {
                                    return Err("cyclic or deeply nested PDF Form XObject".into());
                                }
                                let transform = if let Ok(matrix) = stream.dict.get(b"Matrix") {
                                    let a = matrix.as_array()?;
                                    if a.len() != 6 {
                                        return Err("bad PDF form matrix".into());
                                    }
                                    let mut m = [0.; 6];
                                    for (i, v) in a.iter().enumerate() {
                                        m[i] = v.as_float()?
                                    }
                                    Matrix(m)
                                } else {
                                    Matrix::ID
                                };
                                let r = resource_dict(self.pdf, Some(&stream.dict), b"Resources")
                                    .or(resources);
                                let content = stream.get_plain_content_with_limit(PART_LIMIT)?;
                                self.active.push(id);
                                self.content(&content, r, ctm.concat(transform))?;
                                self.active.pop();
                                moved = true;
                            }
                            b"Image" => {
                                let name = format!("pdf/{}-{}", id.0, id.1);
                                if self.images && !self.doc.assets.contains_key(&name) {
                                    if let Some(bytes) = raster(stream)? {
                                        *self.total += bytes.len();
                                        if *self.total > 512 * MIB {
                                            return Err("PDF asset budget exceeded".into());
                                        }
                                        self.doc.assets.insert(name.clone(), bytes);
                                    } else {
                                        self.doc.warn("PDF image filters without a standalone raster encoding were skipped.");
                                    }
                                }
                                let (x, y) = ctm.point(0., 0.);
                                self.fragments.push(Fragment {
                                    x,
                                    end: x.max(ctm.point(1., 0.).0).max(ctm.point(1., 1.).0),
                                    y,
                                    size: 0.,
                                    horizontal: false,
                                    runs: vec![Run {
                                        image: Some(name),
                                        ..Run::default()
                                    }],
                                });
                                moved = true;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }
}
pub fn parse(data: &[u8], images: bool) -> Result<Document> {
    let pdf =
        Pdf::load_mem_with_options(data, LoadOptions::with_max_decompressed_size(PART_LIMIT))?;
    if pdf.is_encrypted() || pdf.encryption_state.is_some() {
        return Err("encrypted PDF is unsupported".into());
    }
    let mut doc = Document::new("pdf");
    if let Ok(info) = pdf
        .trailer
        .get(b"Info")
        .and_then(Object::as_reference)
        .and_then(|id| pdf.get_dictionary(id))
    {
        for (k, v) in info.iter() {
            if let Ok(s) = lopdf::decode_text_string(v) {
                doc.metadata
                    .insert(String::from_utf8_lossy(k).into_owned(), s);
            }
        }
    }
    let mut total = 0usize;
    for (idx, id) in pdf.page_iter().enumerate() {
        if idx >= 32768 {
            return Err("PDF page limit exceeded".into());
        }
        let mut sec = Section::new("page", format!("{} {} R", id.0, id.1));
        let bytes = pdf.get_page_content_with_limit(id, PART_LIMIT)?;
        let (direct, ids) = pdf.get_page_resources(id)?;
        let resources = direct.or_else(|| ids.first().and_then(|id| pdf.get_dictionary(*id).ok()));
        let mut reader = PageReader {
            pdf: &pdf,
            doc: &mut doc,
            images,
            total: &mut total,
            fragments: Vec::new(),
            rules: Vec::new(),
            active: Vec::new(),
        };
        reader.content(&bytes, resources, Matrix::ID)?;
        annotations(&pdf, id, &mut reader.fragments)?;
        if !reader
            .fragments
            .iter()
            .flat_map(|f| &f.runs)
            .any(|r| !r.text.trim().is_empty())
        {
            reader
                .doc
                .warn("A PDF page has no extractable text. OCR is unavailable.");
        }
        layout(reader.fragments, reader.rules, &mut sec);

        doc.sections.push(sec)
    }
    Ok(doc)
}
