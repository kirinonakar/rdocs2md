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
struct Fragment {
    x: f32,
    y: f32,
    runs: Vec<Run>,
    size: f32,
}
fn layout(mut pieces: Vec<Fragment>, sec: &mut Section) {
    if pieces.is_empty() {
        return;
    }
    // Three or more consistent cell origins over consecutive baselines form a simple grid.
    pieces.sort_by(|a, b| b.y.total_cmp(&a.y).then_with(|| a.x.total_cmp(&b.x)));
    let mut bands: Vec<Vec<Fragment>> = Vec::new();
    for f in pieces {
        if bands
            .last()
            .and_then(|b| b.first())
            .is_none_or(|first| (first.y - f.y).abs() > 1.0)
        {
            bands.push(Vec::new())
        }
        bands.last_mut().unwrap().push(f);
    }
    for band in &mut bands {
        band.sort_by(|a, b| a.x.total_cmp(&b.x))
    }
    let anchors: Vec<_> = bands[0].iter().map(|f| f.x).collect();
    let grid = (3..=32).contains(&anchors.len())
        && bands.len() >= 2
        && anchors.windows(2).all(|w| w[1] - w[0] > 24.)
        && bands.iter().all(|row| {
            row.len() == anchors.len()
                && row.iter().zip(&anchors).all(|(f, x)| (f.x - x).abs() < 2.)
        });
    if grid {
        let rows = bands
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|f| Cell {
                        runs: f.runs,
                        properties: Properties::from([
                            ("x_pt".into(), f.x.to_string()),
                            ("y_pt".into(), f.y.to_string()),
                        ]),
                        ..Cell::default()
                    })
                    .collect()
            })
            .collect();
        sec.blocks.push(Block::Table { rows });
        return;
    }
    let mut pieces: Vec<_> = bands.into_iter().flatten().collect();
    // A repeated gutter between text origins can distinguish two balanced columns.
    let mut xs: Vec<_> = pieces.iter().map(|s| s.x).collect();
    xs.sort_by(f32::total_cmp);
    let split = xs
        .windows(2)
        .enumerate()
        .filter(|(i, w)| *i >= 2 && *i + 3 < xs.len() && w[1] - w[0] > 105.)
        .max_by(|(_, a), (_, b)| (a[1] - a[0]).total_cmp(&(b[1] - b[0])))
        .map(|(_, w)| (w[0] + w[1]) / 2.);
    pieces.sort_by(|a, b| {
        split
            .map(|mid| (a.x >= mid).cmp(&(b.x >= mid)))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.y.total_cmp(&a.y))
            .then_with(|| a.x.total_cmp(&b.x))
    });
    let mut last: Option<(f32, f32)> = None;
    for f in pieces {
        if last.is_some_and(|(x, y)| {
            (y - f.y).abs() < 1.0 && f.x >= x && split.is_none_or(|mid| (f.x >= mid) == (x >= mid))
        }) && let Some(Block::Paragraph { runs, .. }) = sec.blocks.last_mut()
        {
            if last.is_some_and(|(x, _)| f.x > x + f.size * 1.5)
                && !runs
                    .last()
                    .is_some_and(|r| r.text.ends_with(char::is_whitespace))
            {
                push_run(runs, Run::text(" "))
            }
            for run in f.runs {
                push_run(runs, run)
            }
        } else {
            let props = Properties::from([
                ("x_pt".into(), f.x.to_string()),
                ("y_pt".into(), f.y.to_string()),
            ]);
            sec.blocks.push(Block::Paragraph {
                heading: 0,
                list: None,
                runs: f.runs,
                properties: props,
            })
        }
        last = Some((f.x, f.y));
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
        for (key, font) in &fonts {
            if let Ok(e) = font.get_font_encoding_with_limit(self.pdf, 4 * MIB) {
                enc.insert(key.clone(), e);
            }
        }

        let mut stack = Vec::new();
        let mut line = Matrix::ID;
        let mut leading = 0.;
        let mut font = Vec::new();
        let mut size = 0.;

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
                    stack.push((ctm, font.clone(), size, leading))
                }
                "Q" => {
                    if let Some((m, f, s, l)) = stack.pop() {
                        ctm = m;
                        font = f;
                        size = s;
                        leading = l
                    }
                }
                "cm" | "Tm" => {
                    let matrix = Matrix([num(0)?, num(1)?, num(2)?, num(3)?, num(4)?, num(5)?]);
                    if op.operator == "cm" {
                        ctm = ctm.concat(matrix)
                    } else {
                        line = matrix
                    }
                    moved = true
                }
                "BT" => {
                    line = Matrix::ID;
                    moved = true
                }
                "Tf" => {
                    font = args.first().ok_or("PDF font missing")?.as_name()?.to_vec();
                    size = num(1)?
                }
                "TL" => leading = num(0)?,
                "Td" | "TD" => {
                    let dy = num(1)?;
                    line = line.shift(num(0)?, dy);
                    if op.operator == "TD" {
                        leading = -dy
                    }
                    moved = true
                }
                "T*" => {
                    line = line.shift(0., -leading);
                    moved = true
                }
                "Tj" | "TJ" | "'" | "\"" => {
                    if matches!(op.operator.as_str(), "'" | "\"") {
                        line = line.shift(0., -leading);
                        moved = true
                    }
                    let Some(e) = enc.get(&font) else {
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
                    for part in parts {
                        if let Ok(b) = part.as_str() {
                            text.push_str(&Pdf::decode_text(e, b)?)
                        } else if part.as_float().is_ok_and(|v| v < -150.) && !text.ends_with(' ') {
                            text.push(' ')
                        }
                    }
                    if text.is_empty() {
                        continue;
                    }
                    let base = fonts
                        .get(&font)
                        .and_then(|f| f.get(b"BaseFont").ok())
                        .and_then(|n| n.as_name().ok())
                        .map(|n| String::from_utf8_lossy(n).into_owned());
                    let lower = base.as_deref().unwrap_or("").to_ascii_lowercase();
                    let basis = ctm.concat(line);
                    let effective_size = size.abs() * basis.0[2].hypot(basis.0[3]);
                    let run = Run {
                        text,
                        style: Style {
                            bold: lower.contains("bold"),
                            italic: lower.contains("italic") || lower.contains("oblique"),
                            font: base,
                            size: Some(format!("{effective_size}pt")),
                            ..Style::default()
                        },
                        ..Run::default()
                    };
                    if moved || self.fragments.is_empty() {
                        let (x, y) = ctm.point(line.0[4], line.0[5]);
                        if !x.is_finite() || !y.is_finite() {
                            return Err("non-finite PDF coordinates".into());
                        }
                        self.fragments.push(Fragment {
                            x,
                            y,
                            size: effective_size,
                            runs: vec![run],
                        });
                        moved = false;
                    } else if let Some(f) = self.fragments.last_mut() {
                        push_run(&mut f.runs, run)
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
                                    y,
                                    size: 0.,
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
        layout(reader.fragments, &mut sec);

        doc.sections.push(sec)
    }
    Ok(doc)
}
