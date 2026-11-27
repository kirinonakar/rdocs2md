use super::{
    u16le, u32le, utf16, word_color, word_fib_table, word_fkp_pages, word_palette, word_shd,
    word_sprm_operand,
};
use crate::model::Style;

pub(super) struct WordFormatting<'a> {
    chpx: Vec<(usize, usize, &'a [u8])>,
    styles: Vec<Option<WordStyle<'a>>>,
    fonts: Vec<String>,
    default_fonts: [Option<usize>; 4],
    pub(super) piece_modifiers: Vec<&'a [u8]>,
}

struct WordStyle<'a> {
    base: usize,
    chpx: &'a [u8],
}

#[derive(Default, Clone)]
struct WordChars {
    style: Style,
    fonts: [Option<usize>; 4],
    complex_bold: bool,
    complex_italic: bool,
    complex_size: Option<String>,
    complex: bool,
    east_asian_hint: bool,
    highlight: Option<String>,
}

pub(super) struct WordRunStyles {
    styles: [Style; 4],
    complex: bool,
    east_asian_hint: bool,
}

impl WordRunStyles {
    pub(super) fn for_char(&self, c: char) -> &Style {
        let index = match c as u32 {
            _ if self.complex => 3,
            0x0000..=0x007f => 0,
            0x0590..=0x109f | 0x1780..=0x17ff | 0xfb1d..=0xfdff | 0xfe70..=0xfefc => 3,
            0x1100..=0x11ff
            | 0x2e80..=0xa4cf
            | 0xa960..=0xa97f
            | 0xac00..=0xd7ff
            | 0xf900..=0xfaff
            | 0xfe30..=0xfe4f
            | 0xff00..=0xffef
            | 0x20000..=0x323af => 1,
            _ if self.east_asian_hint && !c.is_alphabetic() => 1,
            _ => 2,
        };
        &self.styles[index]
    }
}

impl<'a> WordFormatting<'a> {
    pub(super) fn new(data: &'a [u8], table: &'a [u8], base: usize) -> Self {
        let mut chpx = Vec::new();
        for page in word_fkp_pages(data, table, base, 12) {
            let count = usize::from(page[511]);
            let offsets = 4 * (count + 1);
            if offsets + count > 511 {
                continue;
            }
            for (index, &offset) in page[offsets..offsets + count].iter().enumerate() {
                let start = u32le(page, index * 4).unwrap_or_default() as usize;
                let end = u32le(page, (index + 1) * 4).unwrap_or_default() as usize;
                let offset = usize::from(offset) * 2;
                if offset < offsets + count || offset >= 511 || start >= end {
                    continue;
                }
                if let Some(record) =
                    page[..511].get(offset + 1..offset + 1 + usize::from(page[offset]))
                {
                    chpx.push((start, end, record));
                }
            }
        }
        chpx.sort_unstable_by_key(|&(_, end, _)| end);

        let sheet = word_fib_table(data, table, base, 1).unwrap_or_default();
        let (styles, default_fonts) = word_styles(sheet);
        let fonts = word_fonts(word_fib_table(data, table, base, 15).unwrap_or_default());
        Self {
            chpx,
            styles,
            fonts,
            default_fonts,
            piece_modifiers: Vec::new(),
        }
    }

    pub(super) fn index_at(&self, fc: usize) -> Option<usize> {
        let index = self.chpx.partition_point(|&(_, end, _)| end <= fc);
        self.chpx.get(index).filter(|&&(start, _, _)| start <= fc)?;
        Some(index)
    }

    pub(super) fn resolve(&self, paragraph: u16, index: Option<usize>, prm: u16) -> WordRunStyles {
        let mut base = WordChars {
            fonts: self.default_fonts,
            ..WordChars::default()
        };
        self.apply_style(&mut base, usize::from(paragraph), 0);
        let mut chars = base.clone();
        let direct = index.map_or(&[][..], |i| self.chpx[i].2);
        let mut short = [0; 3];
        let piece = if prm & 1 != 0 {
            self.piece_modifiers
                .get(usize::from(prm >> 1))
                .copied()
                .unwrap_or_default()
        } else {
            short_grpprl(prm, &mut short)
        };
        let edits = word_prls(direct).chain(word_prls(piece));
        if let Some(id) = edits
            .clone()
            .filter_map(|(code, operand)| {
                if code == 0x4a30 {
                    u16le(operand, 0).ok()
                } else {
                    None
                }
            })
            .last()
        {
            self.apply_style(&mut chars, usize::from(id), 0);
        }
        let styled = chars.clone();
        for (code, operand) in edits {
            if code == 0x2a33 {
                let highlight = chars.highlight.take();
                chars = base.clone();
                chars.highlight = highlight;
            } else {
                chars.apply(code, operand, &styled);
            }
        }
        let styles = std::array::from_fn(|i| {
            let mut style = chars.style.clone();
            style.font = chars.fonts[i]
                .or(chars.fonts[0])
                .and_then(|id| self.fonts.get(id))
                .cloned();
            if i == 3 {
                style.bold = chars.complex_bold;
                style.italic = chars.complex_italic;
                style.size = chars.complex_size.clone().or(style.size);
            }
            if chars.highlight.is_some() {
                style.background = chars.highlight.clone();
            }
            style
        });
        WordRunStyles {
            styles,
            complex: chars.complex,
            east_asian_hint: chars.east_asian_hint,
        }
    }

    fn apply_style(&self, chars: &mut WordChars, index: usize, depth: usize) {
        let Some(Some(style)) = self.styles.get(index).filter(|_| depth < 32) else {
            return;
        };
        self.apply_style(chars, style.base, depth + 1);
        let base = chars.clone();
        for (code, operand) in word_prls(style.chpx) {
            chars.apply(code, operand, &base);
        }
    }
}

impl WordChars {
    fn apply(&mut self, code: u16, operand: &[u8], base: &Self) {
        fn toggle(v: u8, base: bool, current: bool) -> bool {
            match v {
                0 => false,
                1 => true,
                0x80 => base,
                0x81 => !base,
                _ => current,
            }
        }

        let Some(&value) = operand.first() else {
            return;
        };
        let number = || u16le(operand, 0).ok();
        let size = || {
            number()
                .filter(|&n| n > 0)
                .map(|n| format!("{}pt", f32::from(n) / 2.))
        };
        match code {
            0x0835 => self.style.bold = toggle(value, base.style.bold, self.style.bold),
            0x0836 => self.style.italic = toggle(value, base.style.italic, self.style.italic),
            0x0837 | 0x2a53 => {
                self.style.strike = toggle(value, base.style.strike, self.style.strike)
            }
            0x083c => {
                self.style.hidden = Some(toggle(
                    value,
                    base.style.hidden.unwrap_or(false),
                    self.style.hidden.unwrap_or(false),
                ))
            }
            0x085a | 0x0882 => self.complex = toggle(value, base.complex, self.complex),
            0x085c => self.complex_bold = toggle(value, base.complex_bold, self.complex_bold),
            0x085d => self.complex_italic = toggle(value, base.complex_italic, self.complex_italic),
            0x2a0c => self.highlight = word_palette(value).map(str::to_owned),
            0x2a3e => self.style.underline = value != 0,
            0x2a42 => self.style.color = word_palette(value).map(str::to_owned),
            0x2a48 => {
                self.style.script = match value {
                    1 => Some("sup".into()),
                    2 => Some("sub".into()),
                    _ => None,
                }
            }
            0x286f => self.east_asian_hint = value == 1,
            0x4a43 => self.style.size = size(),
            0x4a61 => self.complex_size = size(),
            0x4a4f..=0x4a51 => self.fonts[usize::from(code - 0x4a4f)] = number().map(usize::from),
            0x4a5e => self.fonts[3] = number().map(usize::from),
            0x6870 => self.style.color = word_color(operand),
            0x4866 | 0xca71 => self.style.background = word_shd(operand),
            _ => {}
        }
    }
}

fn short_grpprl(prm: u16, out: &mut [u8; 3]) -> &[u8] {
    let code: u16 = match (prm >> 1) & 0x7f {
        0x4d => 0x2a0c,
        0x53 => 0x2a33,
        0x55 => 0x0835,
        0x56 => 0x0836,
        0x57 => 0x0837,
        0x5c => 0x083c,
        0x5e => 0x2a3e,
        0x62 => 0x2a42,
        0x68 => 0x2a48,
        0x73 => 0x2a53,
        _ => return &[],
    };
    let [lo, hi] = code.to_le_bytes();
    *out = [lo, hi, (prm >> 8) as u8];
    out
}

fn word_prls(mut bytes: &[u8]) -> impl Iterator<Item = (u16, &[u8])> + Clone {
    std::iter::from_fn(move || {
        let code = u16le(bytes, 0).ok()?;
        bytes = &bytes[2..];
        let (operand, rest) = word_sprm_operand(code, bytes)?;
        bytes = rest;
        Some((code, operand))
    })
}

fn word_styles(sheet: &[u8]) -> (Vec<Option<WordStyle<'_>>>, [Option<usize>; 4]) {
    let header_len = usize::from(u16le(sheet, 0).unwrap_or_default());
    let Some(header) = sheet.get(2..2 + header_len) else {
        return (Vec::new(), [None; 4]);
    };
    let count = usize::from(u16le(header, 0).unwrap_or_default());
    let base_len = usize::from(u16le(header, 2).unwrap_or_default());
    let fonts = [12, 14, 16, 18].map(|at| u16le(header, at).ok().map(usize::from));
    let mut records = &sheet[2 + header_len..];
    let mut styles = Vec::new();
    for _ in 0..count {
        let Ok(len) = u16le(records, 0) else { break };
        let Some((record, rest)) = records[2..].split_at_checked(usize::from(len)) else {
            break;
        };
        records = rest;
        styles.push(read_style(record, base_len));
    }
    (styles, fonts)
}

fn read_style(record: &[u8], base_len: usize) -> Option<WordStyle<'_>> {
    let flags = u16le(record, 2).ok()?;
    let name_len = usize::from(u16le(record, base_len).ok()?);
    let mut upx = record.get(base_len + 2 + (name_len + 1) * 2..)?;
    if flags & 0xf == 1 {
        let papx_len = usize::from(u16le(upx, 0).ok()?);
        upx = upx.get(2 + papx_len.next_multiple_of(2)..)?;
    } else if flags & 0xf != 2 {
        return None;
    }
    let chpx_len = usize::from(u16le(upx, 0).ok()?);
    Some(WordStyle {
        base: usize::from(flags >> 4),
        chpx: upx.get(2..2 + chpx_len)?,
    })
}

fn word_fonts(table: &[u8]) -> Vec<String> {
    let count = u16le(table, 0).unwrap_or_default();
    let mut records = table.get(4..).unwrap_or_default();
    let mut fonts = Vec::new();
    for _ in 0..count {
        let Some(&len) = records.first() else { break };
        let Some((record, rest)) = records.split_at_checked(usize::from(len) + 1) else {
            break;
        };
        records = rest;
        let name = record
            .get(40..)
            .and_then(|b| utf16(b).ok())
            .unwrap_or_default();
        fonts.push(name.split('\0').next().unwrap_or_default().into());
    }
    fonts
}
