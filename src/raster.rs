use crate::model::Result;
use std::io::Write;

pub(crate) fn compact_bmp(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    // Only transcode the uncompressed 24-bit BITMAPINFOHEADER form. Other
    // bitmap formats keep their original bytes and metadata.
    let Some(header) = bytes.get(..54).filter(|b| {
        b.starts_with(b"BM")
            && b[14..18] == 40_u32.to_le_bytes()
            && b[26..34] == [1, 0, 24, 0, 0, 0, 0, 0]
    }) else {
        return Ok(None);
    };
    let width = i32::from_le_bytes(header[18..22].try_into()?);
    let height = i32::from_le_bytes(header[22..26].try_into()?);
    if width <= 0 || height == 0 {
        return Ok(None);
    }
    let image = (|| {
        let row = (width as usize).checked_mul(3)?;
        let stride = row.checked_add(3)? & !3;
        let offset = u32::from_le_bytes(header[10..14].try_into().ok()?) as usize;
        if offset < header.len() {
            return None;
        }
        let end = offset.checked_add(stride.checked_mul(height.unsigned_abs() as usize)?)?;
        Some((row, stride, bytes.get(offset..end)?))
    })();
    let Some((row, stride, pixels)) = image else {
        return Ok(None);
    };
    let mut rgb = Vec::with_capacity(pixels.len());
    let height_abs = height.unsigned_abs() as usize;
    for y in 0..height_abs {
        crate::limits::check_request()?;
        let start = if height < 0 { y } else { height_abs - 1 - y } * stride;
        for &[b, g, r] in pixels[start..start + row].as_chunks::<3>().0 {
            rgb.extend([r, g, b]);
        }
    }
    let png = png(width as usize, height_abs, 3, &rgb)?;
    Ok((png.len() < bytes.len()).then_some(png))
}

pub(crate) fn png(width: usize, height: usize, channels: usize, pixels: &[u8]) -> Result<Vec<u8>> {
    let color_type = match channels {
        1 => 0,
        3 => 2,
        4 => 6,
        _ => return Err("unsupported PNG color type".into()),
    };
    let mut out = Vec::with_capacity(pixels.len() / 2 + 1024);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, color_type, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &header);
    let stride = width * channels;
    let mut rows = Vec::with_capacity((stride + 1) * height);
    for line in pixels.chunks_exact(stride) {
        rows.push(0);
        rows.extend_from_slice(line);
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&rows)?;
    let data = encoder.finish()?;
    png_chunk(&mut out, b"IDAT", &data);
    png_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn compact_bitmap_preserves_color_order_and_orientation() {
        let width = 31_u32;
        let row = width as usize * 3;
        let stride = (row + 3) & !3;
        let mut bmp = vec![0; 54 + stride * 2];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10..14].copy_from_slice(&54_u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40_u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&width.to_le_bytes());
        bmp[22..26].copy_from_slice(&2_i32.to_le_bytes());
        bmp[26] = 1;
        bmp[28] = 24;
        for (line, bgr) in bmp[54..]
            .chunks_exact_mut(stride)
            .zip([[255, 0, 0], [0, 0, 255]])
        {
            for pixel in line[..row].chunks_exact_mut(3) {
                pixel.copy_from_slice(&bgr);
            }
        }
        let png = compact_bmp(&bmp).unwrap().unwrap();
        assert_eq!(&png[16..24], &[0, 0, 0, 31, 0, 0, 0, 2]);
        let compressed = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        let mut pixels = Vec::new();
        flate2::read::ZlibDecoder::new(&png[41..41 + compressed])
            .read_to_end(&mut pixels)
            .unwrap();
        assert_eq!(pixels.len(), (row + 1) * 2);
        assert_eq!(&pixels[1..row + 1], [255, 0, 0].repeat(width as usize));
        assert_eq!(&pixels[row + 2..], [0, 0, 255].repeat(width as usize));
    }
}
