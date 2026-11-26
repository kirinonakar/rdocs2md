use crate::model::Result;
use std::io::Write;

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
