use super::{Cursor, read_u16_at};
use crate::{model::PART_LIMIT, raster};

pub(super) fn name(bytes: &[u8]) -> &[u8] {
    bytes.split(|&byte| byte == 0).next().unwrap_or_default()
}

const STRETCH_DIB: u16 = 0x0f43;
const SOURCE_COPY: u32 = 0x00cc_0020;
const SOURCE_PAINT: u32 = 0x00ee_0086;
const SOURCE_AND: u32 = 0x0088_00c6;

/// HWP files can wrap a bitmap in a Windows metafile. Read the bitmap records
/// directly, including the two-pass monochrome mask used by imported pictures.
pub(super) fn wmf_raster(bytes: &[u8]) -> Option<Vec<u8>> {
    let bytes = if bytes.starts_with(&[0xd7, 0xcd, 0xc6, 0x9a]) {
        bytes.get(22..)?
    } else {
        bytes
    };
    if !matches!(read_u16_at(bytes, 0).ok()?, 1 | 2) || read_u16_at(bytes, 2).ok()? != 9 {
        return None;
    }
    let mut cursor = Cursor::new(bytes);
    cursor.skip(18).ok()?;
    let mut mask = None;
    let mut image = None;
    loop {
        crate::limits::check_request().ok()?;
        let record_size = (cursor.u32().ok()? as usize).checked_mul(2)?;
        let kind = cursor.u16().ok()?;
        let record = cursor.take(record_size.checked_sub(6)?).ok()?;
        match kind {
            0 if record.is_empty() => break,
            STRETCH_DIB if image.is_none() => {
                let operation = dword(record, 0)?;
                let bitmap = Bitmap::read(record)?;
                match operation {
                    SOURCE_COPY if mask.is_none() => image = Some(bitmap),
                    SOURCE_PAINT if mask.is_none() && bitmap.bpp == 1 => mask = Some(bitmap),
                    SOURCE_AND if mask.is_some() => image = Some(bitmap),
                    _ => return None,
                }
            }
            // Mapping, stock objects and comments carry no bitmap pixels.
            // Escape records are skipped as data.
            0x0102 | 0x0104 | 0x0107 | 0x012d | 0x01f0 | 0x0209 | 0x020b | 0x020c | 0x02fa
            | 0x02fb | 0x02fc | 0x0626 => {}
            // Word sometimes precedes the bitmap with a clipped space.
            0x0a32 if read_u16_at(record, 4).ok()? == 1 && record.get(16) == Some(&b' ') => {}
            _ => return None,
        }
    }
    let image = image?;
    if let Some(mask) = &mask
        && (mask.width != image.width || mask.height != image.height || mask.dest != image.dest)
    {
        return None;
    }
    let channels = if mask.is_some() { 4 } else { 3 };
    let len = image
        .width
        .checked_mul(image.height)?
        .checked_mul(channels)?;
    if len > PART_LIMIT / 4 {
        return None;
    }
    let mut rgb = Vec::with_capacity(len);
    for y in 0..image.height {
        crate::limits::check_request().ok()?;
        for x in 0..image.width {
            let color = image.rgb(x, y)?;
            rgb.extend(color);
            if let Some(mask) = &mask {
                let alpha = match (mask.rgb(x, y)?, color) {
                    ([0, 0, 0], [255, 255, 255]) => 0,
                    ([0, 0, 0] | [255, 255, 255], _) => 255,
                    _ => return None,
                };
                rgb.push(alpha);
            }
        }
    }
    raster::png(image.width, image.height, channels, &rgb).ok()
}

fn dword(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

struct Bitmap<'a> {
    width: usize,
    height: usize,
    dest: [u8; 8],
    top_down: bool,
    bpp: u16,
    stride: usize,
    palette: &'a [[u8; 4]],
    pixels: &'a [u8],
}

impl<'a> Bitmap<'a> {
    fn read(record: &'a [u8]) -> Option<Self> {
        let word = |offset| read_u16_at(record, offset).ok();
        if word(4)? != 0 || word(10)? != 0 || word(12)? != 0 {
            return None;
        }
        let dest = record.get(14..22)?.try_into().ok()?;
        let dib = record.get(22..)?;
        let width = usize::try_from(dword(dib, 4)? as i32).ok()?;
        let signed_height = dword(dib, 8)? as i32;
        let height = signed_height.unsigned_abs() as usize;
        let bpp = read_u16_at(dib, 14).ok()?;
        if dword(dib, 0)? != 40
            || read_u16_at(dib, 12).ok()? != 1
            || dword(dib, 16)? != 0
            || !matches!(bpp, 1 | 4 | 8 | 24)
            || width == 0
            || height == 0
            || usize::from(word(6)?) != height
            || usize::from(word(8)?) != width
            || word(14)? as i16 <= 0
            || word(16)? as i16 <= 0
        {
            return None;
        }
        let max_colors = if bpp <= 8 { 1 << bpp } else { 0 };
        let colors = match dword(dib, 32)? as usize {
            0 => max_colors,
            count if count <= max_colors => count,
            _ => return None,
        };
        let pixel_start = 40 + colors * 4;
        let stride = width.checked_mul(usize::from(bpp))?.div_ceil(32) * 4;
        let bytes = stride.checked_mul(height)?;
        Some(Self {
            width,
            height,
            dest,
            top_down: signed_height < 0,
            bpp,
            stride,
            palette: dib.get(40..pixel_start)?.as_chunks().0,
            pixels: dib.get(pixel_start..pixel_start.checked_add(bytes)?)?,
        })
    }

    fn rgb(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        let y = if self.top_down {
            y
        } else {
            self.height - 1 - y
        };
        let row = &self.pixels[y * self.stride..][..self.stride];
        let index = match self.bpp {
            1 => row[x / 8] >> (7 - x % 8) & 1,
            4 => row[x / 2] >> (4 - 4 * (x % 2)) & 15,
            8 => row[x],
            _ => {
                let offset = x * 3;
                return Some([row[offset + 2], row[offset + 1], row[offset]]);
            }
        };
        let &[b, g, r, _] = self.palette.get(usize::from(index))?;
        Some([r, g, b])
    }
}
