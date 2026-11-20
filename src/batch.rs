use crate::{FORMATS, model::Result};
use glob::{MatchOptions, Pattern};
use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    path::{Component, Path, PathBuf},
};

enum Segment {
    Descendants,
    Name(Pattern),
}

pub struct Expansion {
    pub files: Vec<PathBuf>,
    pub scanned: bool,
    pub errors: Vec<(PathBuf, String)>,
}

fn wildcard(part: &OsStr) -> bool {
    part.to_str().is_some_and(|s| s.contains(['*', '?', '[']))
}

fn pattern_component(component: Component<'_>) -> bool {
    matches!(component, Component::Normal(part) if wildcard(part))
}

fn document_extension(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        FORMATS
            .iter()
            .any(|(format, ..)| ext.eq_ignore_ascii_case(format))
    })
}

/// Expand one input. The resolver is applied before visiting any directory or accepting a file.
pub fn expand(
    spec: &Path,
    recursive: bool,
    literal: bool,
    resolve: impl Fn(&Path) -> Result<PathBuf>,
) -> Result<Expansion> {
    let direct = resolve(spec);
    let pattern = !literal && direct.is_err() && spec.components().any(pattern_component);
    let (root, mut segments) = if pattern {
        let mut prefix = PathBuf::new();
        let mut parts = Vec::new();
        for component in spec.components() {
            let part = component.as_os_str();
            if parts.is_empty() && !pattern_component(component) {
                prefix.push(part);
                continue;
            }
            if !matches!(component, Component::Normal(_)) {
                return Err("expected ordinary path components after a wildcard".into());
            }
            let part = part.to_str().ok_or("invalid wildcard text")?;
            parts.push(if part == "**" {
                Segment::Descendants
            } else {
                Segment::Name(Pattern::new(part)?)
            });
        }
        if prefix.as_os_str().is_empty() {
            prefix.push(".");
        }
        if matches!(parts.last(), Some(Segment::Descendants)) {
            parts.push(Segment::Name(Pattern::new("*")?));
        }
        if recursive && !parts.iter().any(|p| matches!(p, Segment::Descendants)) {
            parts.insert(parts.len() - 1, Segment::Descendants);
        }
        (resolve(&prefix)?, parts)
    } else {
        let path = direct?;
        if path.is_file() {
            return Ok(Expansion {
                files: vec![path],
                scanned: false,
                errors: Vec::new(),
            });
        }
        (path, vec![Segment::Name(Pattern::new("*")?)])
    };
    if !pattern && recursive {
        segments.insert(0, Segment::Descendants);
    }

    let options = MatchOptions {
        case_sensitive: !cfg!(windows),
        ..MatchOptions::default()
    };
    let mut pending = vec![(root, 0)];
    let mut visited = HashSet::new();
    let mut files = HashSet::new();
    let mut errors = Vec::new();
    while let Some((dir, i)) = pending.pop() {
        if !visited.insert((dir.clone(), i)) {
            continue;
        }
        let segment = &segments[i];
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                errors.push((dir, e.to_string()));
                continue;
            }
        };
        let next = match segment {
            Segment::Descendants => {
                pending.push((dir.clone(), i + 1));
                i
            }
            Segment::Name(_) => i + 1,
        };
        let leaf = next == segments.len();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    errors.push((dir.clone(), e.to_string()));
                    continue;
                }
            };
            if pattern
                && let Segment::Name(name) = segment
                && !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|s| name.matches_with(s, options))
            {
                continue;
            }
            let path = entry.path();
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(e) => {
                    errors.push((path, e.to_string()));
                    continue;
                }
            };
            let matches = if leaf {
                kind.is_file() && (pattern || document_extension(&path))
            } else {
                kind.is_dir()
            };
            if !matches {
                continue;
            }
            match resolve(&path) {
                Ok(path) if leaf => {
                    files.insert(path);
                }
                Ok(path) => pending.push((path, next)),
                Err(e) => errors.push((path, e.to_string())),
            }
        }
    }
    if files.is_empty() && errors.is_empty() {
        return Err("no matching document files".into());
    }
    let mut files: Vec<_> = files.into_iter().collect();
    files.sort_unstable();
    Ok(Expansion {
        files,
        scanned: true,
        errors,
    })
}

#[derive(Default)]
pub struct OutputNames {
    used: HashSet<OsString>,
    next_suffix: HashMap<OsString, usize>,
}

fn name_key(name: &OsStr) -> OsString {
    if cfg!(windows) {
        name.to_string_lossy().to_lowercase().into()
    } else {
        name.to_os_string()
    }
}

impl OutputNames {
    pub fn next(&mut self, input: &Path) -> OsString {
        let stem = input.file_stem().unwrap_or_else(|| OsStr::new("document"));
        let suffix = self.next_suffix.entry(name_key(stem)).or_insert(1);
        loop {
            let mut name = stem.to_os_string();
            if *suffix > 1 {
                name.push(format!("-{suffix}"));
            }
            *suffix += 1;
            if self.used.insert(name_key(&name)) {
                return name;
            }
        }
    }
}

pub fn output_name(stem: &OsStr, format: &str) -> OsString {
    let mut name = stem.to_os_string();
    name.push(".");
    name.push(format);
    name
}
