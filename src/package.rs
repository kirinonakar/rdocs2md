use crate::model::*;
use roxmltree::{Document as Xml, Node, ParsingOptions};
use std::{
    collections::HashSet,
    io::{Cursor, Read},
};
use zip::ZipArchive;

pub type N<'a> = Node<'a, 'a>;
pub fn tag<'a>(n: N<'a>) -> &'a str {
    n.tag_name().name()
}
pub fn attr<'a>(n: N<'a>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}
pub fn child<'a>(n: N<'a>, name: &str) -> Option<N<'a>> {
    n.children().find(|v| v.is_element() && tag(*v) == name)
}
pub fn desc<'a>(n: N<'a>, name: &str) -> impl Iterator<Item = N<'a>> {
    n.descendants()
        .filter(move |v| v.is_element() && tag(*v) == name)
}
pub fn all_text(n: N) -> String {
    n.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}
pub fn xml(s: &str) -> Result<Xml<'_>> {
    let doc = Xml::parse_with_options(
        s,
        ParsingOptions {
            nodes_limit: 1_500_000,
            ..ParsingOptions::default()
        },
    )?;
    fn check(n: N, depth: usize) -> Result<()> {
        if depth > 128 {
            return Err("XML depth exceeds 128".into());
        }
        for child in n.children().filter(|c| c.is_element()) {
            check(child, depth + 1)?
        }
        Ok(())
    }
    check(doc.root(), 0)?;
    Ok(doc)
}
pub fn resolve(base: &str, target: &str) -> Result<String> {
    if target.contains(['\\', ':', '\0']) || target.starts_with("//") {
        return Err("invalid package reference".into());
    }
    let parent = base.rsplit_once('/').map(|x| x.0).unwrap_or("");
    let mut parts: Vec<_> = if target.starts_with('/') || parent.is_empty() {
        Vec::new()
    } else {
        parent.split('/').collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("path escapes package")?;
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}
pub struct Package<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    read: usize,
}
impl<'a> Package<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let zip = ZipArchive::new(Cursor::new(data))?;
        if zip.len() > 32768 {
            return Err("ZIP entry limit exceeded".into());
        }
        let mut total = 0u64;
        let mut seen = HashSet::new();
        let mut zip = zip;
        for i in 0..zip.len() {
            let e = zip.by_index_raw(i)?;
            if e.encrypted() {
                return Err("encrypted ZIP package is unsupported".into());
            }
            if !seen.insert(e.name().to_owned()) || e.enclosed_name().is_none() {
                return Err("ambiguous or unsafe ZIP entry".into());
            }
            total = total.checked_add(e.size()).ok_or("invalid ZIP sizes")?;
            if total > 512 * MIB as u64 || e.size() > FILE_LIMIT as u64 {
                return Err("ZIP expansion limit exceeded".into());
            }
        }
        Ok(Self { zip, read: 0 })
    }
    pub fn has(&self, name: &str) -> bool {
        self.zip.index_for_name(name).is_some()
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.zip.file_names()
    }
    pub fn bytes(&mut self, name: &str) -> Result<Vec<u8>> {
        let e = self
            .zip
            .by_name(name)
            .map_err(|err| format!("{name}: {err}"))?;
        let n = usize::try_from(e.size())?;
        if n > PART_LIMIT || self.read + n > 512 * MIB {
            return Err("package read limit exceeded".into());
        }
        let mut out = Vec::new();
        e.take(n as u64 + 1).read_to_end(&mut out)?;
        if out.len() != n {
            return Err("ZIP entry size mismatch".into());
        }
        self.read += n;
        Ok(out)
    }
    pub fn text(&mut self, name: &str) -> Result<String> {
        let b = self.bytes(name)?;
        if b.starts_with(&[0xff, 0xfe]) || b.starts_with(&[0xfe, 0xff]) {
            let enc = if b[0] == 0xff {
                encoding_rs::UTF_16LE
            } else {
                encoding_rs::UTF_16BE
            };
            return Ok(enc
                .decode_without_bom_handling_and_without_replacement(&b[2..])
                .ok_or("invalid UTF-16 XML")?
                .into_owned());
        }
        Ok(String::from_utf8(b)?)
    }
}
pub fn metadata(pkg: &mut Package, doc: &mut Document, path: &str) -> Result<()> {
    if pkg.has(path) {
        let s = pkg.text(path)?;
        let x = xml(&s)?;
        for n in x.root().descendants().filter(Node::is_element) {
            if matches!(
                tag(n),
                "title"
                    | "creator"
                    | "subject"
                    | "description"
                    | "language"
                    | "created"
                    | "modified"
                    | "date"
                    | "generator"
                    | "keywords"
                    | "identifier"
            ) && let Some(v) = n.text()
            {
                doc.metadata.insert(tag(n).into(), v.into());
            }
        }
    }
    Ok(())
}
