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
        let Some(ext) = image_ext(b) else { continue };
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
