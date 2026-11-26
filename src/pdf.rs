use crate::{model::*, visibility::TINY_POINTS};
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
mod math;
use layout::{Fragment, HorizontalRule, VerticalRule, approximate_width, layout};

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

#[derive(Clone, Copy)]
struct Stroke {
    width: f32,
    color: [u8; 3],
    style: &'static str,
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            width: 1.,
            color: [0; 3],
            style: "solid",
        }
    }
}

fn resolve_object<'a>(pdf: &'a Pdf, obj: &'a Object) -> Option<&'a Object> {
    match obj {
        Object::Reference(id) => pdf.get_object(*id).ok(),
        _ => Some(obj),
    }
}

fn thin_rectangle(points: &[(f32, f32)]) -> Option<(f32, f32, f32, f32)> {
    if points.len() != 4 {
        return None;
    }
    let (left, right) = points
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |b, p| {
            (b.0.min(p.0), b.1.max(p.0))
        });
    let (bottom, top) = points
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |b, p| {
            (b.0.min(p.1), b.1.max(p.1))
        });
    if !left.is_finite() || !bottom.is_finite() || right - left <= 0. || top - bottom <= 0. {
        return None;
    }
    let mut corners = 0u8;
    for &(x, y) in points {
        let column = if (x - left).abs() <= 0.5 {
            0
        } else if (x - right).abs() <= 0.5 {
            1
        } else {
            return None;
        };
        let row = if (y - bottom).abs() <= 0.5 {
            0
        } else if (y - top).abs() <= 0.5 {
            1
        } else {
            return None;
        };
        let corner = 1 << (row * 2 + column);
        if corners & corner != 0 {
            return None;
        }
        corners |= corner;
    }
    if corners != 0b1111 {
        return None;
    }
    let width = right - left;
    let height = top - bottom;
    ((width >= 8. && height <= 2.5) || (height >= 8. && width <= 2.5))
        .then_some((left, right, bottom, top))
}

fn filled_path_rules(
    shapes: &[Vec<(f32, f32)>],
    stroke: Stroke,
    rules: &mut Vec<HorizontalRule>,
    verticals: &mut Vec<VerticalRule>,
) {
    for points in shapes {
        let Some((left, right, bottom, top)) = thin_rectangle(points) else {
            continue;
        };
        if rules.len() + verticals.len() >= PDF_RULE_LIMIT {
            break;
        }
        let (width, height) = (right - left, top - bottom);
        let thickness = width.min(height).clamp(0.25, 100.);
        if height <= 2.5 {
            rules.push(HorizontalRule {
                left,
                right,
                y: (bottom + top) * 0.5,
                width: thickness,
                color: stroke.color,
                style: "solid",
            });
        } else {
            verticals.push(VerticalRule {
                x: (left + right) * 0.5,
                bottom,
                top,
                width: thickness,
                color: stroke.color,
                style: "solid",
            });
        }
    }
}

fn finish_path_shape(shape: &mut Vec<(f32, f32)>, shapes: &mut Vec<Vec<(f32, f32)>>) {
    if shape.is_empty() {
        return;
    }
    if shape.len() <= 4 && shapes.len() < PDF_RULE_LIMIT {
        shapes.push(std::mem::take(shape));
    } else {
        shape.clear();
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

// The embedded HYhwpEQ font uses private-use characters for ordinary math glyphs.
fn hyhwp_eq_unicode(character: char) -> Option<char> {
    let code = character as u32;
    match code {
        0xe000..=0xe019 => char::from_u32(u32::from(b'A') + code - 0xe000),
        0xe01a..=0xe02b => char::from_u32(u32::from(b'a') + code - 0xe01a),
        0xe02c => Some('S'),
        0xe02d => Some('t'),
        0xe02e => Some('u'),
        0xe02f => Some('V'),
        0xe030 => Some('W'),
        0xe031 => Some('X'),
        0xe032 => Some('y'),
        0xe033 => Some('z'),
        0xe034 => Some('1'),
        0xe035 => Some('2'),
        0xe036 => Some('3'),
        0xe037 => Some('4'),
        0xe038 => Some('5'),
        0xe039 => Some('6'),
        0xe03a => Some('7'),
        0xe03b => Some('8'),
        0xe03c => Some('9'),
        0xe03d => Some('0'),
        0xe03e => Some('!'),
        0xe03f => Some('@'),
        0xe040 => Some('#'),
        0xe041 => Some('$'),
        0xe042 => Some('%'),
        0xe043 => Some('*'),
        0xe044 => Some('('),
        0xe045 => Some(')'),
        0xe046 | 0xe06d | 0xe0c5 => Some('−'),
        0xe047 => Some('='),
        0xe048 => Some('+'),
        0xe049 | 0xe07a | 0xe081 | 0xe084 => Some('['),
        0xe04a | 0xe07b | 0xe085 => Some(']'),
        0xe04b | 0xe078 | 0xe083 => Some('{'),
        0xe04c | 0xe079 => Some('}'),
        0xe04d | 0xe07d => Some('|'),
        0xe04e => Some(';'),
        0xe04f => Some(':'),
        0xe050 => Some('\''),
        0xe051 => Some('"'),
        0xe052 => Some(','),
        0xe053 | 0xe080 => Some('.'),
        0xe054 => Some('/'),
        0xe055 => Some('<'),
        0xe056 => Some('>'),
        0xe057 => Some('?'),
        0xe058 | 0xe06c => Some('←'),
        0xe059 => Some('¯'),
        0xe05a | 0xe061 | 0xe06e => Some('→'),
        0xe05b | 0xe072 => Some('∫'),
        0xe05c => Some('√'),
        0xe05d => Some('´'),
        0xe05e => Some('`'),
        0xe05f => Some('·'),
        0xe060 => Some('∴'),
        0xe062 => Some('↔'),
        0xe063 => Some('↑'),
        0xe064 => Some('↓'),
        0xe065 => Some('⌒'),
        0xe066 => Some('~'),
        0xe067 => Some('Σ'),
        0xe068 => Some('Π'),
        0xe06a => Some('∩'),
        0xe06b => Some('∪'),
        0xe06f => Some('⇐'),
        0xe070 => Some('≡'),
        0xe071 => Some('⇒'),
        0xe073 => Some('∬'),
        0xe074 => Some('∭'),
        0xe075 => Some('∮'),
        0xe076 => Some('∯'),
        0xe077 => Some('∰'),
        0xe07c | 0xe07e => Some('('),
        0xe07f | 0xe082 => Some(')'),
        0xe086 => Some('Α'),
        0xe087 => Some('Β'),
        0xe088 | 0xe0c1 => Some('Γ'),
        0xe089 => Some('Δ'),
        0xe08a => Some('Ε'),
        0xe08b => Some('Ζ'),
        0xe08c => Some('Η'),
        0xe08d => Some('Θ'),
        0xe08e => Some('Ι'),
        0xe08f => Some('Κ'),
        0xe090 => Some('Μ'),
        0xe091 => Some('Ν'),
        0xe092 => Some('Ξ'),
        0xe093 => Some('Ο'),
        0xe094 => Some('Π'),
        0xe095 => Some('Ρ'),
        0xe096 => Some('Σ'),
        0xe097 => Some('Τ'),
        0xe098 => Some('Υ'),
        0xe099 => Some('Φ'),
        0xe09a => Some('Χ'),
        0xe09b => Some('Ψ'),
        0xe09c => Some('Ω'),
        0xe09d => Some('α'),
        0xe09e => Some('β'),
        0xe09f => Some('γ'),
        0xe0a0 => Some('δ'),
        0xe0a1 => Some('ε'),
        0xe0a2 => Some('ζ'),
        0xe0a3 => Some('η'),
        0xe0a4 => Some('θ'),
        0xe0a5 => Some('ι'),
        0xe0a6 => Some('κ'),
        0xe0a7 => Some('λ'),
        0xe0a8 => Some('μ'),
        0xe0a9 => Some('ν'),
        0xe0aa => Some('ξ'),
        0xe0ab => Some('ο'),
        0xe0ac => Some('π'),
        0xe0ad => Some('ρ'),
        0xe0ae => Some('σ'),
        0xe0af => Some('τ'),
        0xe0b0 => Some('υ'),
        0xe0b1 | 0xe0c2 => Some('φ'),
        0xe0b2 => Some('χ'),
        0xe0b3 => Some('ψ'),
        0xe0b4 => Some('ω'),
        0xe0b5 => Some('ℵ'),
        0xe0b6 => Some('ℏ'),
        0xe0b7 | 0xe0ba | 0xe0bb => Some('ℓ'),
        0xe0c0 => Some('ς'),
        0xe0c4 => Some('∂'),
        0xe0c6 => Some('↔'),
        0xe0c7 => Some('⇔'),
        0xe0c8 => Some('°'),
        0xe0c9 => Some('†'),
        0xe0ca => Some('‡'),
        0xe0cb..=0xe0e4 => char::from_u32(u32::from(b'A') + code - 0xe0cb),
        0xe0e5..=0xe0fe => char::from_u32(u32::from(b'a') + code - 0xe0e5),
        0xe0ff => Some('□'),
        _ => None,
    }
}

fn hyhwp_eq_cid(cid: u16) -> Option<char> {
    // Its subset CIDs start at 0x00f9 for U+E000; this recovers entries omitted
    // from the PDF's incomplete ToUnicode map.
    let offset = u32::from(cid.checked_sub(0x00f9)?);
    hyhwp_eq_unicode(char::from_u32(0xe000 + offset)?)
}

fn decode_hyhwp_eq(encoding: &lopdf::Encoding<'_>, bytes: &[u8]) -> String {
    let mut text = String::new();
    for code in bytes.as_chunks::<2>().0 {
        let decoded = Pdf::decode_text(encoding, code).unwrap_or_else(|_| "�".into());
        if decoded.contains('�')
            && let Some(cid) = code
                .first()
                .copied()
                .zip(code.get(1).copied())
                .map(|(high, low)| u16::from_be_bytes([high, low]))
            && let Some(character) = hyhwp_eq_cid(cid)
        {
            text.push(character);
            continue;
        }
        for character in decoded.chars() {
            text.push(hyhwp_eq_unicode(character).unwrap_or(character));
        }
    }
    if !bytes.len().is_multiple_of(2)
        && let Ok(decoded) = Pdf::decode_text(encoding, &bytes[bytes.len() - 1..])
    {
        text.extend(
            decoded
                .chars()
                .map(|character| hyhwp_eq_unicode(character).unwrap_or(character)),
        );
    }
    text
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
#[derive(Clone, Copy, PartialEq)]
enum ColorModel {
    Gray,
    Rgb,
    Cmyk,
}

struct ColorSpace {
    model: ColorModel,
    /// Indexed spaces resolve to one RGB triple per palette index.
    palette: Option<Vec<[u8; 3]>>,
    channels: usize,
}

fn model_space(model: ColorModel) -> ColorSpace {
    ColorSpace {
        model,
        palette: None,
        channels: match model {
            ColorModel::Gray => 1,
            ColorModel::Rgb => 3,
            ColorModel::Cmyk => 4,
        },
    }
}

fn color_model(name: &[u8]) -> Option<ColorModel> {
    match name {
        b"DeviceGray" | b"G" | b"CalGray" => Some(ColorModel::Gray),
        b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(ColorModel::Rgb),
        b"DeviceCMYK" | b"CMYK" => Some(ColorModel::Cmyk),
        _ => None,
    }
}

fn color_space(pdf: &Pdf, obj: &Object) -> Option<ColorSpace> {
    match resolve_object(pdf, obj)? {
        Object::Name(name) => color_model(name).map(model_space),
        Object::Array(items) => {
            let family = resolve_object(pdf, items.first()?)?.as_name().ok()?;
            match family {
                // ICCBased profiles declare their own component count, and the
                // raw samples can be read without interpreting the profile.
                b"ICCBased" => {
                    let dict = resolve_object(pdf, items.get(1)?)?.as_dict().ok()?;
                    let model = match dict.get(b"N").and_then(Object::as_i64).ok()? {
                        1 => ColorModel::Gray,
                        3 => ColorModel::Rgb,
                        4 => ColorModel::Cmyk,
                        _ => return None,
                    };
                    Some(model_space(model))
                }
                b"CalRGB" => Some(model_space(ColorModel::Rgb)),
                b"CalGray" => Some(model_space(ColorModel::Gray)),
                b"Indexed" => {
                    let base = color_space(pdf, items.get(1)?)?;
                    if base.palette.is_some() {
                        return None;
                    }
                    let high = usize::try_from(items.get(2)?.as_i64().ok()?).ok()?;
                    let count = high.checked_add(1)?;
                    let lookup = resolve_object(pdf, items.get(3)?)?;
                    let bytes = match lookup {
                        Object::String(bytes, _) => bytes.clone(),
                        _ => lookup
                            .as_stream()
                            .ok()?
                            .get_plain_content_with_limit(PART_LIMIT)
                            .ok()?,
                    };
                    if count > 4096 || bytes.len() < count.checked_mul(base.channels)? {
                        return None;
                    }
                    let palette = bytes
                        .chunks_exact(base.channels)
                        .take(count)
                        .map(|entry| to_rgb(base.model, entry))
                        .collect();
                    Some(ColorSpace {
                        model: ColorModel::Rgb,
                        palette: Some(palette),
                        channels: 1,
                    })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Ink-to-light conversion that keeps scanned CMYK art readable without
/// applying an ICC profile.
fn to_rgb(model: ColorModel, s: &[u8]) -> [u8; 3] {
    match model {
        ColorModel::Gray => [s[0]; 3],
        ColorModel::Rgb => [s[0], s[1], s[2]],
        ColorModel::Cmyk => {
            let k = u16::from(s[3]);
            let f = |v: u8| ((255 - u16::from(v)) * (255 - k) / 255) as u8;
            [f(s[0]), f(s[1]), f(s[2])]
        }
    }
}

/// Expands 1, 2, 4, 8 or 16 bit samples into one byte per component.
///
/// `scale` is false for an Indexed image, whose samples are palette indices:
/// stretching them to 0-255 would point past the palette and turn the image
/// black.
fn unpack(
    raw: &[u8],
    width: usize,
    height: usize,
    channels: usize,
    bpc: i64,
    scale: bool,
) -> Option<Vec<u8>> {
    let count = width.checked_mul(height)?.checked_mul(channels)?;
    match bpc {
        8 => (raw.len() >= count).then(|| raw[..count].to_vec()),
        16 => (raw.len() >= count.checked_mul(2)?)
            .then(|| raw.chunks_exact(2).take(count).map(|p| p[0]).collect()),
        1 | 2 | 4 => {
            let bpc = usize::try_from(bpc).ok()?;
            let row = width.checked_mul(bpc * channels)?.checked_add(7)? / 8;
            if raw.len() < row.checked_mul(height)? {
                return None;
            }
            let max = (1u16 << bpc) - 1;
            let mut out = Vec::with_capacity(count);
            for line in raw.chunks_exact(row).take(height) {
                let mut bit = 0;
                for _ in 0..width * channels {
                    let byte = line[bit / 8];
                    let shift = 8 - bpc - bit % 8;
                    let value = u16::from((byte >> shift) & (max as u8));
                    out.push(if scale {
                        (value * 255 / max) as u8
                    } else {
                        value as u8
                    });
                    bit += bpc;
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// Applies a `/Decode` array, which is how inverted bilevel scans stay legible.
///
/// `max` is the largest value that [`unpack`] produced. `palette` is the highest
/// index of an Indexed image: its samples are indices, so a decoded value stays
/// an index instead of becoming a 0-255 color component.
fn apply_decode(
    samples: &mut [u8],
    channels: usize,
    decode: Option<&[Object]>,
    max: u8,
    palette: Option<usize>,
) {
    let Some(decode) = decode.filter(|d| d.len() == channels * 2) else {
        return;
    };
    let full = f32::from(max);
    for (index, sample) in samples.iter_mut().enumerate() {
        let channel = index % channels;
        let (Ok(min), Ok(top)) = (
            decode[channel * 2].as_float(),
            decode[channel * 2 + 1].as_float(),
        ) else {
            continue;
        };
        if palette.is_none() && (min, top) == (0., 1.) {
            continue;
        }
        let value = min + f32::from(*sample) / full * (top - min);
        *sample = match palette {
            Some(high) => value.round().clamp(0., high.min(255) as f32) as u8,
            None => (value.clamp(0., 1.) * 255.) as u8,
        };
    }
}

fn raster(pdf: &Pdf, stream: &lopdf::Stream) -> Result<Option<Vec<u8>>> {
    // Already encoded images (JPEG, PNG) pass through unchanged.
    if image_ext(&stream.content).is_some() {
        return Ok(Some(stream.content.clone()));
    }
    let d = &stream.dict;
    let (Ok(width), Ok(height)) = (
        d.get(b"Width").and_then(Object::as_i64),
        d.get(b"Height").and_then(Object::as_i64),
    ) else {
        return Ok(None);
    };
    let (Ok(width), Ok(height)) = (usize::try_from(width), usize::try_from(height)) else {
        return Ok(None);
    };
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let mask = matches!(d.get(b"ImageMask"), Ok(Object::Boolean(true)));
    let space = if mask {
        Some(model_space(ColorModel::Gray))
    } else {
        d.get(b"ColorSpace").ok().and_then(|o| color_space(pdf, o))
    };
    let Some(space) = space else {
        return Ok(None);
    };
    let bpc = if mask {
        1
    } else {
        d.get(b"BitsPerComponent")
            .and_then(Object::as_i64)
            .unwrap_or(8)
    };
    let channels = space.channels;
    let Some(pixels) = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(channels))
    else {
        return Ok(None);
    };
    if pixels > PART_LIMIT {
        return Ok(None);
    }
    let Ok(raw) = stream.get_plain_content_with_limit(PART_LIMIT) else {
        return Ok(None);
    };
    // An Indexed image already holds palette indices, so only the bit packing is
    // undone here; scaling them to 0-255 would index past the palette.
    let palette = space.palette.as_deref();
    let Some(mut samples) = unpack(&raw, width, height, channels, bpc, palette.is_none()) else {
        return Ok(None);
    };
    // Unscaled indices are at most 2^bpc-1, which is what `/Decode` maps from.
    let max = match (palette.is_some(), bpc) {
        (true, 1..=4) => ((1u16 << bpc) - 1) as u8,
        _ => 255,
    };
    let decode = d.get(b"Decode").ok().and_then(|o| o.as_array().ok());
    apply_decode(
        &mut samples,
        channels,
        decode.map(Vec::as_slice),
        max,
        palette.map(|p| p.len().saturating_sub(1)),
    );
    let (out_channels, colors) = match (&space.palette, space.model) {
        (Some(palette), _) => {
            let mut colors = Vec::with_capacity(pixels * 3);
            for &index in &samples {
                colors.extend_from_slice(&palette.get(index as usize).copied().unwrap_or_default());
            }
            (3, colors)
        }
        (None, ColorModel::Gray) => (1, samples),
        (None, ColorModel::Rgb) => (3, samples),
        (None, ColorModel::Cmyk) => {
            let mut colors = Vec::with_capacity(pixels / 4 * 3);
            for entry in samples.chunks_exact(4) {
                colors.extend_from_slice(&to_rgb(ColorModel::Cmyk, entry));
            }
            (3, colors)
        }
    };
    crate::raster::png(width, height, out_channels, &colors).map(Some)
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

fn page_box(pdf: &Pdf, mut id: lopdf::ObjectId) -> Option<[f32; 4]> {
    let mut media = None;
    let mut crop = None;
    for _ in 0..128 {
        let Ok(dict) = pdf.get_dictionary(id) else {
            break;
        };
        let bounds = |key: &[u8]| {
            let values = resolve_object(pdf, dict.get(key).ok()?)?.as_array().ok()?;
            let [a, b, c, d] = values.as_slice() else {
                return None;
            };
            let coords = [
                a.as_float().ok()?,
                b.as_float().ok()?,
                c.as_float().ok()?,
                d.as_float().ok()?,
            ];
            coords.iter().all(|v| v.is_finite()).then(|| {
                let [a, b, c, d] = coords;
                [a.min(c), b.min(d), a.max(c), b.max(d)]
            })
        };
        media = media.or_else(|| bounds(b"MediaBox"));
        crop = crop.or_else(|| bounds(b"CropBox"));
        if media.is_some() && crop.is_some() {
            break;
        }
        let Ok(parent) = dict.get(b"Parent").and_then(Object::as_reference) else {
            break;
        };
        id = parent;
    }
    match (media, crop) {
        (Some([l, b, r, t]), Some([cl, cb, cr, ct])) => {
            Some([l.max(cl), b.max(cb), r.min(cr), t.min(ct)])
        }
        (media, crop) => media.or(crop),
    }
}

#[derive(Clone, Copy, Default)]
struct TextPaint {
    fill_white: bool,
    stroke_white: bool,
    mode: i64,
}

impl TextPaint {
    fn is_white(self) -> bool {
        match self.mode {
            0 | 4 => self.fill_white,
            1 | 5 => self.stroke_white,
            2 | 6 => self.fill_white && self.stroke_white,
            _ => false,
        }
    }
}

struct PageReader<'a> {
    pdf: &'a Pdf,
    doc: &'a mut Document,
    images: bool,
    total: &'a mut usize,
    fragments: Vec<Fragment>,
    rules: Vec<HorizontalRule>,
    verticals: Vec<VerticalRule>,
    active: Vec<lopdf::ObjectId>,
    bounds: Option<[f32; 4]>,
    user_unit: f64,
    has_images: bool,
}
impl PageReader<'_> {
    fn content(
        &mut self,
        bytes: &[u8],
        resources: Option<&lopdf::Dictionary>,
        mut ctm: Matrix,
        mut stroke: Stroke,
        mut paint: TextPaint,
    ) -> Result<()> {
        if crate::limits::expired() {
            return Err("time limit exceeded".into());
        }
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
        let mut path_vertical_rules = Vec::new();
        let mut path_shape = Vec::new();
        let mut path_shapes = Vec::<Vec<(f32, f32)>>::new();
        let mut marked_spans = Vec::new();

        let mut moved = true;
        for (index, op) in Content::decode_iter(bytes).enumerate() {
            if index % 256 == 0 {
                crate::limits::check_request()?;
            }
            let op = op?;
            let args = &op.operands;
            let num = |i: usize| {
                args.get(i)
                    .ok_or("PDF operand missing")
                    .and_then(|v| v.as_float().map_err(|_| "invalid PDF numeric operand"))
            };
            match op.operator.as_str() {
                "BMC" => marked_spans.push(false),
                "BDC" => marked_spans.push(
                    args.first().and_then(|o| o.as_name().ok()) == Some(b"Span")
                        && args
                            .get(1)
                            .and_then(|o| o.as_dict().ok())
                            .is_some_and(|properties| properties.has(b"MCID")),
                ),
                "EMC" => {
                    marked_spans.pop();
                }
                "q" => {
                    if stack.len() > 128 {
                        return Err("PDF graphics state depth exceeded".into());
                    }
                    stack.push((ctm, font.clone(), state, stroke, paint))
                }
                "Q" => {
                    if let Some((m, f, s, line_style, text_paint)) = stack.pop() {
                        ctm = m;
                        font = f;
                        state = s;
                        stroke = line_style;
                        paint = text_paint;
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
                "w" => {
                    if let Ok(w) = num(0)
                        && w.is_finite()
                        && w >= 0.
                    {
                        stroke.width = w;
                    }
                }
                "G" | "RG" => {
                    let component = |i| num(i).ok().map(|v| (255. * v.clamp(0., 1.)).round() as u8);
                    if let Some(r) = component(0) {
                        stroke.color = if op.operator == "G" {
                            [r; 3]
                        } else if let (Some(g), Some(b)) = (component(1), component(2)) {
                            [r, g, b]
                        } else {
                            stroke.color
                        };
                    }
                    paint.stroke_white = if op.operator == "G" {
                        num(0).is_ok_and(|v| v >= 1.)
                    } else {
                        (0..3).all(|i| num(i).is_ok_and(|v| v >= 1.))
                    };
                }
                "g" | "rg" => {
                    let count = if op.operator == "g" { 1 } else { 3 };
                    paint.fill_white = (0..count).all(|i| num(i).is_ok_and(|v| v >= 1.));
                }
                "k" => paint.fill_white = (0..4).all(|i| num(i).is_ok_and(|v| v <= 0.)),
                "K" => paint.stroke_white = (0..4).all(|i| num(i).is_ok_and(|v| v <= 0.)),
                "cs" | "sc" | "scn" => paint.fill_white = false,
                "CS" | "SC" | "SCN" => paint.stroke_white = false,
                "Tr" => {
                    if let Some(mode) = args.first().and_then(|v| v.as_i64().ok()) {
                        paint.mode = mode;
                    }
                }
                "d" => {
                    if let Some(pattern) = args.first().and_then(|a| a.as_array().ok()) {
                        stroke.style = match pattern.first().and_then(|p| p.as_float().ok()) {
                            Some(n) if n <= stroke.width => "dotted",
                            Some(_) => "dashed",
                            None => "solid",
                        };
                    }
                }
                "m" | "l" => {
                    let next = num(0).ok().zip(num(1).ok()).map(|(x, y)| ctm.point(x, y));
                    if op.operator == "m" {
                        finish_path_shape(&mut path_shape, &mut path_shapes);
                    }
                    if op.operator == "l"
                        && path_rules.len()
                            + path_vertical_rules.len()
                            + self.rules.len()
                            + self.verticals.len()
                            < PDF_RULE_LIMIT
                        && let (Some((x, y)), Some((end, end_y))) = (path_point, next)
                        && x.is_finite()
                        && end.is_finite()
                    {
                        if (y - end_y).abs() <= 0.5 && (x - end).abs() >= 10. {
                            path_rules.push((x.min(end), x.max(end), y));
                        } else if (x - end).abs() <= 0.5 && (y - end_y).abs() >= 10. {
                            path_vertical_rules.push((x, y.min(end_y), y.max(end_y)));
                        }
                    }
                    if let Some(point) = next
                        && path_shape.len() <= 4
                    {
                        path_shape.push(point);
                    }
                    path_point = next;
                }
                "h" => {
                    finish_path_shape(&mut path_shape, &mut path_shapes);
                    path_point = None;
                }
                "re" => {
                    finish_path_shape(&mut path_shape, &mut path_shapes);
                    if let (Ok(x), Ok(y), Ok(w), Ok(h)) = (num(0), num(1), num(2), num(3))
                        && path_shapes.len() < PDF_RULE_LIMIT
                    {
                        path_shapes.push(vec![
                            ctm.point(x, y),
                            ctm.point(x + w, y),
                            ctm.point(x + w, y + h),
                            ctm.point(x, y + h),
                        ]);
                    }
                    path_point = None;
                }
                "c" | "v" | "y" => {
                    finish_path_shape(&mut path_shape, &mut path_shapes);
                    path_point = None;
                }
                "S" | "s" | "B" | "B*" | "b" | "b*" => {
                    finish_path_shape(&mut path_shape, &mut path_shapes);
                    filled_path_rules(&path_shapes, stroke, &mut self.rules, &mut self.verticals);
                    path_shapes.clear();
                    let horizontal_scale = ctm.0[2].hypot(ctm.0[3]);
                    let vertical_scale = ctm.0[0].hypot(ctm.0[1]);
                    for (left, right, y) in path_rules.drain(..) {
                        if self.rules.len() + self.verticals.len() >= PDF_RULE_LIMIT {
                            break;
                        }
                        self.rules.push(HorizontalRule {
                            left,
                            right,
                            y,
                            width: (stroke.width * horizontal_scale).clamp(0.25, 100.),
                            color: stroke.color,
                            style: stroke.style,
                        });
                    }
                    for (x, bottom, top) in path_vertical_rules.drain(..) {
                        if self.rules.len() + self.verticals.len() >= PDF_RULE_LIMIT {
                            break;
                        }
                        self.verticals.push(VerticalRule {
                            x,
                            bottom,
                            top,
                            width: (stroke.width * vertical_scale).clamp(0.25, 100.),
                            color: stroke.color,
                            style: stroke.style,
                        });
                    }
                    path_point = None;
                }
                "f" | "F" | "f*" => {
                    finish_path_shape(&mut path_shape, &mut path_shapes);
                    filled_path_rules(&path_shapes, stroke, &mut self.rules, &mut self.verticals);
                    path_shapes.clear();
                    path_rules.clear();
                    path_vertical_rules.clear();
                    path_point = None;
                }
                "n" => {
                    path_rules.clear();
                    path_vertical_rules.clear();
                    path_shapes.clear();
                    path_shape.clear();
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
                    let base = fonts
                        .get(&font)
                        .and_then(|f| f.get(b"BaseFont").ok())
                        .and_then(|n| n.as_name().ok())
                        .map(|n| String::from_utf8_lossy(n).into_owned());
                    let hyhwp_eq = base
                        .as_deref()
                        .is_some_and(|name| name.to_ascii_lowercase().contains("hyhwpeq"));
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
                                if hyhwp_eq {
                                    decode_hyhwp_eq(e, b)
                                } else {
                                    match Pdf::decode_text(e, b) {
                                        Ok(text) => text,
                                        Err(_) => {
                                            self.doc.warn("Text with an unreadable PDF font encoding was kept as replacement characters.");
                                            "\u{fffd}".repeat(b.len())
                                        }
                                    }
                                }
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
                        let (end_x, _) = basis.point(end, state.rise);
                        if !x.is_finite()
                            || !y.is_finite()
                            || !end_x.is_finite()
                            || !effective_size.is_finite()
                        {
                            return Err("non-finite PDF coordinates".into());
                        }
                        // A positioned hyphen in its own marked span can be
                        // dropped at a wrap without stripping compound hyphens.
                        let discretionary_hyphen =
                            moved && text == "-" && marked_spans.last() == Some(&true);
                        let mut run = Run {
                            text,
                            style: style.clone(),
                            ..Run::default()
                        };
                        if paint.is_white() {
                            run.visibility_flags.push(VisibilityFlag::WhiteText);
                        }
                        if f64::from(effective_size) * self.user_unit < TINY_POINTS {
                            run.visibility_flags.push(VisibilityFlag::TinyText);
                        }
                        if let Some([left, bottom, right, top]) = self.bounds {
                            let points = [start, end].map(|x| {
                                [state.rise - state.size * 0.25, state.rise + state.size]
                                    .map(|y| basis.point(x, y))
                            });
                            if [
                                points.iter().flatten().all(|p| p.0 < left - 0.5),
                                points.iter().flatten().all(|p| p.1 < bottom - 0.5),
                                points.iter().flatten().all(|p| p.0 > right + 0.5),
                                points.iter().flatten().all(|p| p.1 > top + 0.5),
                            ]
                            .into_iter()
                            .any(|outside| outside)
                            {
                                run.visibility_flags.push(VisibilityFlag::OffPage);
                            }
                        }
                        if matches!(paint.mode, 3 | 7) {
                            run.visibility_flags.push(VisibilityFlag::HiddenText);
                        }
                        if !moved
                            && !separate_glyphs
                            && start == 0.
                            && let Some(f) = self.fragments.last_mut()
                        {
                            f.end = f.end.max(end_x);
                            f.discretionary_hyphen = false;
                            push_run(&mut f.runs, run);
                        } else {
                            self.fragments.push(Fragment {
                                x,
                                end: x.max(end_x),
                                y,
                                size: effective_size,
                                horizontal: basis.0[0] > 0. && basis.0[1].abs() < basis.0[0] * 0.15,
                                discretionary_hyphen,
                                runs: vec![run],
                            });
                        }
                        moved = separate_glyphs;
                    }
                }
                "BI" => self.has_images = true,
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
                                self.content(&content, r, ctm.concat(transform), stroke, paint)?;
                                self.active.pop();
                                moved = true;
                            }
                            b"Image" => {
                                self.has_images = true;
                                let name = format!("pdf/{}-{}", id.0, id.1);
                                if self.images && !self.doc.assets.contains_key(&name) {
                                    if let Some(bytes) = raster(self.pdf, stream)? {
                                        *self.total += bytes.len();
                                        if *self.total > 512 * MIB {
                                            return Err("PDF asset budget exceeded".into());
                                        }
                                        self.doc.assets.insert(name.clone(), bytes);
                                    } else {
                                        self.doc.warn("PDF images that could not be decoded as a standalone raster (for example JPX or JBIG2) were skipped.");
                                    }
                                }
                                let (x, y) = ctm.point(0., 0.);
                                self.fragments.push(Fragment {
                                    x,
                                    end: x.max(ctm.point(1., 0.).0).max(ctm.point(1., 1.).0),
                                    y,
                                    size: 0.,
                                    horizontal: false,
                                    discretionary_hyphen: false,
                                    runs: vec![Run {
                                        image: Some(name),
                                        image_geometry: {
                                            let [a, b, c, d, _, _] = ctm.0;
                                            let width = f64::from(a.hypot(b)) * self.user_unit;
                                            let height = f64::from(c.hypot(d)) * self.user_unit;
                                            (width > 0.
                                                && width <= 10000.
                                                && height > 0.
                                                && height <= 10000.)
                                                .then_some(ImageGeometry {
                                                    width: Some(width),
                                                    height: Some(height),
                                                    rotation: f64::from(-b.atan2(a)).to_degrees(),
                                                    flip_y: a * d - b * c < 0.,
                                                    ..ImageGeometry::default()
                                                })
                                        },
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
pub fn parse(data: &[u8], images: bool, range: Option<&SectionRange>) -> Result<Document> {
    let mut pdf =
        Pdf::load_mem_with_options(data, LoadOptions::with_max_decompressed_size(PART_LIMIT))?;
    crate::limits::check_request()?;
    // An encrypted PDF that opens with the empty password only carries permission
    // restrictions (an owner password with no user password), so decrypt it and
    // convert its content. A document that demands a real user password stays refused.
    if pdf.is_encrypted() && pdf.decrypt("").is_err() {
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
    let pages: Vec<_> = pdf.page_iter().take(32769).collect();
    if pages.len() > 32768 {
        return Err("PDF page limit exceeded".into());
    }
    crate::limits::check_request()?;
    let outlines = page_outlines(&pdf, &pages);
    let mut total = 0usize;
    for idx in section_indices(range, pages.len())? {
        crate::limits::check_request()?;
        if crate::limits::expired() {
            doc.warn("The time limit was reached, so the remaining PDF pages were skipped. Raise or remove the timeout to convert them.");
            // Leave a marker where extraction stopped: warnings travel outside the
            // body, so a reader of the body alone would not see the gap.
            let mut skipped = Section::new("page", String::new()).indexed(idx);
            skipped
                .properties
                .insert("page".into(), (idx + 1).to_string());
            skipped
                .properties
                .insert("skipped".into(), "time_limit".into());
            doc.sections.push(skipped);
            break;
        }
        let id = pages[idx];
        let mut sec = Section::new("page", format!("{} {} R", id.0, id.1)).indexed(idx);
        // The source string is a PDF object identifier; keep the human page
        // number so that citations do not need to resolve object numbers.
        sec.properties.insert("page".into(), (idx + 1).to_string());
        // One malformed page or operator must not discard the whole document:
        // keep whatever was extracted and report the failure as a warning.
        let document_warnings = std::mem::take(&mut doc.warnings);
        if let Err(e) = read_page(&pdf, &mut doc, &mut total, id, images, &mut sec) {
            doc.warn(&format!(
                "PDF page {} could not be read completely, so its extracted text may be missing or partial.",
                idx + 1
            ));
            sec.properties.insert("page_error".into(), e.to_string());
        }
        crate::limits::check_request()?;
        sec.pdf_warnings = std::mem::replace(&mut doc.warnings, document_warnings);
        for warning in &sec.pdf_warnings {
            doc.warn(warning);
        }
        if let Some(entries) = outlines.get(&idx) {
            apply_outlines(&mut sec, entries);
        }
        doc.sections.push(sec)
    }
    Ok(doc)
}

/// Build a range from a complete cached parse, with warnings for these pages.
pub(crate) fn cached_range(doc: &Document, range: &str) -> Result<Document> {
    let range = SectionRange::parse(range)?;
    let mut selected = Document::new("pdf");
    selected.metadata.clone_from(&doc.metadata);
    for index in section_indices(Some(&range), doc.sections.len())? {
        crate::limits::check_request()?;
        let page = &doc.sections[index];
        for warning in &page.pdf_warnings {
            selected.warn(warning);
        }
        selected.sections.push(Section {
            kind: page.kind.clone(),
            source: page.source.clone(),
            index: page.index,
            title: page.title.clone(),
            properties: page.properties.clone(),
            blocks: page.blocks.clone(),
            ..Section::default()
        });
    }
    crate::visibility::annotate(&mut selected);
    Ok(selected)
}

/// Reads one page into `sec`. Failures are returned to the caller so that the
/// page loop can keep the other pages; anything already extracted stays.
fn read_page(
    pdf: &Pdf,
    doc: &mut Document,
    total: &mut usize,
    id: lopdf::ObjectId,
    images: bool,
    sec: &mut Section,
) -> Result<()> {
    let bytes = pdf.get_page_content_with_limit(id, PART_LIMIT)?;
    let (direct, ids) = pdf.get_page_resources(id)?;
    let resources = direct.or_else(|| ids.first().and_then(|id| pdf.get_dictionary(*id).ok()));
    let mut reader = PageReader {
        pdf,
        doc,
        images,
        total,
        fragments: Vec::new(),
        rules: Vec::new(),
        verticals: Vec::new(),
        active: Vec::new(),
        bounds: page_box(pdf, id),
        user_unit: pdf
            .get_dictionary(id)
            .ok()
            .and_then(|d| resolve_object(pdf, d.get(b"UserUnit").ok()?))
            .and_then(|v| v.as_float().ok())
            .filter(|v| v.is_finite() && *v > 0.)
            .map_or(1., f64::from),
        has_images: false,
    };
    let mut error = reader
        .content(
            &bytes,
            resources,
            Matrix::ID,
            Stroke::default(),
            TextPaint::default(),
        )
        .err();
    if error.is_none()
        && let Err(e) = annotations(pdf, id, &mut reader.fragments)
    {
        error = Some(e);
    }
    if !reader
        .fragments
        .iter()
        .flat_map(|f| &f.runs)
        .any(|r| !r.text.trim().is_empty())
    {
        let status = if reader.has_images {
            reader.doc.warn("No text was extracted from a PDF page containing images. Text visible in the images requires OCR, which rdocs2md does not perform.");
            "ocr_needed_for_image_text"
        } else {
            reader.doc.warn("No text was extracted from a PDF page. Text drawn as outlines or other graphics may still be visible.");
            "no_text_extracted"
        };
        sec.properties
            .insert("text_extraction".into(), status.into());
    }
    layout(reader.fragments, reader.rules, reader.verticals, sec);
    match error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// One outline (bookmark) entry: the page it targets and its nesting depth.
struct Outline {
    page: lopdf::ObjectId,
    level: u8,
    title: String,
}

fn outlines(pdf: &Pdf) -> Vec<Outline> {
    let mut out = Vec::new();
    let Some(root) = pdf
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Outlines").ok())
        .and_then(|o| dictionary(pdf, o))
    else {
        return out;
    };
    walk_outlines(pdf, root, 1, &mut out, 0);
    out
}

fn walk_outlines(
    pdf: &Pdf,
    node: &lopdf::Dictionary,
    level: u8,
    out: &mut Vec<Outline>,
    depth: u8,
) {
    if depth > 12 || out.len() >= 8192 {
        return;
    }
    let mut item = node.get(b"First").ok().and_then(|o| dictionary(pdf, o));
    let mut seen = 0usize;
    while let Some(entry) = item {
        seen += 1;
        if seen > 8192 || out.len() >= 8192 {
            break;
        }
        let title = entry
            .get(b"Title")
            .ok()
            .and_then(|o| lopdf::decode_text_string(o).ok())
            .unwrap_or_default();
        let target = entry.get(b"Dest").ok().or_else(|| {
            entry
                .get(b"A")
                .ok()
                .and_then(|o| dictionary(pdf, o))
                .and_then(|a| a.get(b"D").ok())
        });
        let title = title.trim();
        if level <= 6
            && !title.is_empty()
            && let Some(page) = target.and_then(|t| destination_page(pdf, t))
        {
            out.push(Outline {
                page,
                level,
                title: title.to_string(),
            });
        }
        walk_outlines(pdf, entry, level + 1, out, depth + 1);
        item = entry.get(b"Next").ok().and_then(|o| dictionary(pdf, o));
    }
}

fn destination_page(pdf: &Pdf, dest: &Object) -> Option<lopdf::ObjectId> {
    let mut object = dest;
    for _ in 0..8 {
        object = match object {
            Object::Reference(id) => pdf.get_object(*id).ok()?,
            Object::Name(_) | Object::String(..) => named_destination(pdf, object)?,
            _ => break,
        };
    }
    match object.as_array().ok()?.first()? {
        Object::Reference(id) => Some(*id),
        _ => None,
    }
}

/// Resolves a named destination through the `/Dests` dictionary or the
/// `/Names` name tree. Only flat name trees are read; nested kids are rare in
/// practice and a missed bookmark only costs one heading level.
fn named_destination<'a>(pdf: &'a Pdf, name: &Object) -> Option<&'a Object> {
    let key = match name {
        Object::Name(name) => name.as_slice(),
        Object::String(name, _) => name.as_slice(),
        _ => return None,
    };
    let catalog = pdf.catalog().ok()?;
    if let Some(value) = catalog
        .get(b"Dests")
        .ok()
        .and_then(|o| dictionary(pdf, o))
        .and_then(|d| d.get(key).ok())
    {
        return Some(value);
    }
    let names = catalog
        .get(b"Names")
        .ok()
        .and_then(|o| dictionary(pdf, o))
        .and_then(|d| d.get(b"Dests").ok())
        .and_then(|o| dictionary(pdf, o))
        .and_then(|d| d.get(b"Names").ok())
        .and_then(|o| o.as_array().ok())?;
    names.chunks_exact(2).find_map(|pair| {
        let found = match &pair[0] {
            Object::Name(n) => n.as_slice() == key,
            Object::String(n, _) => n.as_slice() == key,
            _ => false,
        };
        found.then(|| &pair[1])
    })
}

fn page_outlines(pdf: &Pdf, pages: &[lopdf::ObjectId]) -> HashMap<usize, Vec<(u8, String)>> {
    let mut map: HashMap<usize, Vec<(u8, String)>> = HashMap::new();
    let index: HashMap<lopdf::ObjectId, usize> =
        pages.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    for entry in outlines(pdf) {
        if let Some(&page) = index.get(&entry.page) {
            map.entry(page)
                .or_default()
                .push((entry.level, entry.title));
        }
    }
    map
}

/// Promotes paragraphs that match a bookmark title so that the PDF output
/// carries the same `#` hierarchy as the source document.
///
/// A bookmark whose title the page never prints (exporters often keep
/// "Chapter 1." in the outline only) is inserted as a heading of its own
/// instead of being dropped from the structure.
fn apply_outlines(sec: &mut Section, entries: &[(u8, String)]) {
    let mut cursor = 0;
    for (level, title) in entries {
        let key = outline_key(title);
        if key.is_empty() {
            continue;
        }
        let mut found = None;
        let mut partial = None;
        for (index, block) in sec.blocks.iter().enumerate().skip(cursor) {
            let Block::Paragraph {
                heading: 0, runs, ..
            } = block
            else {
                continue;
            };
            let text: String = runs.iter().map(|r| r.text.as_str()).collect();
            let text = outline_key(&text);
            if text.is_empty() {
                continue;
            }
            if text == key {
                found = Some(index);
                break;
            }
            if partial.is_none()
                && (text.starts_with(&key) || key.starts_with(&text))
                && text.len().min(key.len()) >= 4
            {
                partial = Some(index);
            }
        }
        if let Some(index) = found.or(partial) {
            if let Block::Paragraph { heading, .. } = &mut sec.blocks[index] {
                *heading = (*level).clamp(1, 6);
            }
            cursor = index + 1;
        } else {
            sec.blocks.insert(
                cursor,
                Block::Paragraph {
                    heading: (*level).clamp(1, 6),
                    list: None,
                    runs: vec![Run::text(title.as_str())],
                    properties: Properties::new(),
                },
            );
            cursor += 1;
        }
    }
}

fn outline_key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
