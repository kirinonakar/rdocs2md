use crate::model::*;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub fn parent(p: &Path) -> &Path {
    p.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
pub fn absolute(p: &Path) -> Result<PathBuf> {
    if p.exists() {
        return Ok(p.canonicalize()?);
    }
    Ok(parent(p)
        .canonicalize()?
        .join(p.file_name().ok_or("expected filename")?))
}
// Publish a completed file without replacing a concurrent writer's output.
#[cfg(windows)]
fn publish_new(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileW(existing: *const u16, new: *const u16) -> i32;
    }
    let encode = |p: &Path| -> std::io::Result<Vec<u16>> {
        let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
        if v.contains(&0) {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        v.push(0);
        Ok(v)
    };
    let (src, dst) = (encode(from)?, encode(to)?);
    // Both buffers are live and NUL-terminated for the duration of the system call.
    if unsafe { MoveFileW(src.as_ptr(), dst.as_ptr()) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn publish_new(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::hard_link(from, to)
}
pub fn save(path: &Path, bytes: &[u8], exclusive: bool) -> Result<()> {
    let tmp = parent(path).join(format!(
        ".rdocs2md-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.flush()?;
        drop(file);
        if exclusive {
            publish_new(&tmp, path)?;
        } else {
            fs::rename(&tmp, path)?;
        }
        Ok(())
    })();
    if (exclusive && !cfg!(windows)) || result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
fn relative(from: &Path, to: &Path) -> String {
    let a: Vec<_> = from.components().collect();
    let b: Vec<_> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return to
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches("//?/")
            .into();
    }
    let mut p = PathBuf::new();
    for _ in common..a.len() {
        p.push("..")
    }
    for part in &b[common..] {
        p.push(part.as_os_str())
    }
    p.to_string_lossy().replace('\\', "/")
}
pub fn extract(d: &Document, dir: &Path, base: &Path) -> Result<Properties> {
    fs::create_dir_all(dir)?;
    let dir = dir.canonicalize()?;
    let base = base.canonicalize()?;
    let mut links = Properties::new();
    for (i, key) in image_sources(d).iter().enumerate() {
        let Some(b) = d.assets.get(key) else { continue };
        let Some(ext) = asset_ext(key, b) else {
            continue;
        };
        let mut suffix = 0;
        loop {
            let stem = if suffix == 0 {
                format!("image-{}", i + 1)
            } else {
                format!("image-{}-{suffix}", i + 1)
            };
            let path = dir.join(format!("{stem}.{ext}"));
            match fs::symlink_metadata(&path) {
                Ok(m) => {
                    if m.file_type().is_file() && m.len() == b.len() as u64 {
                        let mut existing = Vec::new();
                        fs::File::open(&path)?
                            .take(b.len() as u64 + 1)
                            .read_to_end(&mut existing)?;
                        if existing == *b {
                            links.insert(key.clone(), relative(&base, &path));
                            break;
                        }
                    }
                    suffix += 1;
                    if suffix > 10000 {
                        return Err("too many image name collisions".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    save(&path, b, true)?;
                    links.insert(key.clone(), relative(&base, &path));
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(links)
}

/// Key that carries an inlined `data:` URI for the image source named by `key`.
/// The two entries never collide because an asset name cannot contain `\u{1}`.
pub fn inline_key(key: &str) -> String {
    format!("{key}\u{1}data")
}

fn asset_ext(key: &str, bytes: &[u8]) -> Option<&'static str> {
    if key.starts_with("hwp3/drawing/")
        && bytes.starts_with(crate::hwp3::GENERATED_SVG_START.as_bytes())
    {
        Some("svg")
    } else {
        image_ext(bytes)
    }
}

fn mime(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" => "image/tiff",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// Standard base64 with padding. A data URI takes no line breaks, so the
/// characters are emitted in one run.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let tail = [
            *chunk.first().unwrap_or(&0),
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let bits = u32::from(tail[0]) << 16 | u32::from(tail[1]) << 8 | u32::from(tail[2]);
        out.push(ALPHABET[(bits >> 18) as usize & 63] as char);
        out.push(ALPHABET[(bits >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(bits >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[bits as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Add data URIs for the displayed images. With no extracted paths this makes
/// a single-file HTML document. Extracted paths, when present, remain available
/// to the renderer as `data-path`.
pub fn inline(d: &Document, links: &Properties) -> Result<Properties> {
    let mut out = links.clone();
    let mut encoded_len = 0usize;
    for key in image_sources(d) {
        if !links.is_empty() && !links.contains_key(&key) {
            continue;
        }
        let Some(bytes) = d.assets.get(&key) else {
            continue;
        };
        let Some(ext) = asset_ext(&key, bytes) else {
            continue;
        };
        encoded_len = encoded_len.saturating_add(bytes.len().div_ceil(3).saturating_mul(4));
        if encoded_len > EXPANSION_LIMIT {
            return Err(
                "embedded images exceed the HTML output limit; extract them as separate files"
                    .into(),
            );
        }
        out.insert(
            inline_key(&key),
            format!("data:{};base64,{}", mime(ext), base64(bytes)),
        );
    }
    Ok(out)
}
