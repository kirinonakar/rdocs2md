use crate::{
    flow::{Flow, Rules},
    model::*,
    package::*,
};
use std::collections::HashMap;
// EPUB 2 commonly declares a public XHTML or NCX DTD. Remove only the
// declaration; never load the external subset or accept entity definitions.
fn markup(pkg: &mut Package, path: &str) -> Result<String> {
    let mut s = pkg.text(path)?;
    if let Some(start) = s.find("<!DOCTYPE") {
        let mut quote = None;
        let mut end = None;
        for (i, c) in s[start + 9..].char_indices() {
            if let Some(q) = quote {
                if c == q {
                    quote = None
                }
                continue;
            }
            match c {
                '"' | '\'' => quote = Some(c),
                '[' => return Err("EPUB internal DTD subsets are unsupported".into()),
                '>' => {
                    end = Some(start + 9 + i + 1);
                    break;
                }
                _ => {}
            }
        }
        s.replace_range(start..end.ok_or("unterminated EPUB doctype")?, "");
    }
    for (entity, ch) in [
        ("&nbsp;", "\u{a0}"),
        ("&copy;", "©"),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
    ] {
        if s.contains(entity) {
            s = s.replace(entity, ch)
        }
    }
    Ok(s)
}
fn add_css(css: &str, r: &mut Rules) {
    let mut clean = String::new();
    let mut s = css;
    while let Some((before, rem)) = s.split_once("/*") {
        clean.push_str(before);
        s = rem.split_once("*/").map(|v| v.1).unwrap_or("")
    }
    clean.push_str(s);
    for rule in clean.split('}') {
        if let Some((names, body)) = rule.split_once('{') {
            for sel in names.split(',').map(str::trim) {
                if sel.len() <= 128 && !sel.contains([' ', '@', '>', '[', ':']) {
                    r.css
                        .entry(sel.into())
                        .or_default()
                        .push_str(&format!(";{body}"))
                }
            }
        }
    }
}
pub fn parse(pkg: &mut Package) -> Result<Document> {
    if pkg.has("META-INF/encryption.xml") {
        return Err("encrypted/DRM EPUB resources are unsupported".into());
    }
    let container = pkg.text("META-INF/container.xml")?;
    let x = xml(&container)?;
    let path = desc(x.root(), "rootfile")
        .find_map(|r| attr(r, "full-path"))
        .ok_or("EPUB rootfile missing")?;
    let s = pkg.text(path)?;
    let x = xml(&s)?;
    let root = x.root_element();
    if tag(root) != "package" {
        return Err("invalid EPUB package".into());
    }
    let manifest = child(root, "manifest").ok_or("manifest missing")?;
    let spine = child(root, "spine").ok_or("spine missing")?;
    let mut doc = Document::new("epub");
    metadata(pkg, &mut doc, path)?;
    let mut items = HashMap::new();
    let mut ncx = Vec::new();
    let mut nav_entries = Vec::new();
    for item in manifest.children().filter(|n| tag(*n) == "item") {
        if let (Some(id), Some(href), Some(mime)) = (
            attr(item, "id"),
            attr(item, "href"),
            attr(item, "media-type"),
        ) {
            if href.contains("://") {
                continue;
            }
            let p = resolve(path, href)?;
            if mime == "application/x-dtbncx+xml"
                || attr(item, "properties")
                    .is_some_and(|v| v.split_whitespace().any(|v| v == "nav"))
            {
                let t = markup(pkg, &p)?;
                let nx = xml(&t)?;
                for pt in desc(nx.root(), "navPoint") {
                    if let (Some(src), Some(label)) = (
                        child(pt, "content").and_then(|c| attr(c, "src")),
                        child(pt, "navLabel"),
                    ) {
                        ncx.push(serde_json::json!({"target":resolve(&p,src)?,"label":all_text(label),"depth":pt.ancestors().skip(1).filter(|a|tag(*a)=="navPoint").count()}));
                    }
                }
                for nav in desc(nx.root(), "nav").filter(|n| {
                    attr(*n, "type").is_some_and(|t| t.split_whitespace().any(|v| v == "toc"))
                }) {
                    for a in desc(nav, "a") {
                        if let Some(href) = attr(a, "href")
                            && let Ok(target) = resolve(&p, href)
                        {
                            nav_entries.push(serde_json::json!({"target":target,"label":all_text(a),"depth":a.ancestors().filter(|n|tag(*n)=="ol").count().saturating_sub(1)}));
                        }
                    }
                }
            }
            items.insert(id, (p, mime));
        }
    }
    let toc = if nav_entries.is_empty() {
        ncx
    } else {
        nav_entries
    };
    if !toc.is_empty() {
        doc.metadata
            .insert("toc".into(), serde_json::to_string(&toc)?);
    }
    let mut css_cache = HashMap::new();
    for item in spine.children().filter(|n| tag(*n) == "itemref") {
        let id = attr(item, "idref").ok_or("spine idref missing")?;
        let (p, kind) = items.get(id).ok_or("spine item missing from manifest")?;
        let mut sec = Section::new("chapter", p);
        if attr(item, "linear") == Some("no") {
            sec.properties.insert("linear".into(), "no".into());
        }
        let s = markup(pkg, p)?;
        let x = xml(&s)?;
        let mut rules = Rules::default();
        if *kind == "image/svg+xml" {
            for n in desc(x.root(), "text") {
                sec.paragraph(all_text(n))
            }
            doc.warn("SVG chapter graphics are represented by their text nodes.");
        } else {
            for n in x.root().descendants().filter(|n| n.is_element()) {
                if tag(n) == "link"
                    && attr(n, "rel") == Some("stylesheet")
                    && let Some(href) = attr(n, "href")
                    && let Ok(css_path) = resolve(p, href)
                    && pkg.has(&css_path)
                {
                    if !css_cache.contains_key(&css_path) {
                        let text = pkg.text(&css_path)?;
                        css_cache.insert(css_path.clone(), text);
                    }
                    add_css(&css_cache[&css_path], &mut rules)
                } else if tag(n) == "style" {
                    add_css(&all_text(n), &mut rules)
                }
            }
            let body = desc(x.root(), "body")
                .next()
                .ok_or("EPUB XHTML body missing")?;
            Flow {
                kind: "epub",
                base: p,
                rules: &rules,
            }
            .blocks(body, &mut sec.blocks)?;
        }
        doc.sections.push(sec)
    }
    link_chapters(&mut doc);
    Ok(doc)
}
fn link_chapters(doc: &mut Document) {
    let mut targets = HashMap::new();
    for (i, s) in doc.sections.iter().enumerate() {
        targets
            .entry(s.source.clone())
            .or_insert_with(|| format!("chapter-{}", i + 1));
        for (j, b) in s.blocks.iter().enumerate() {
            if let Block::Paragraph { properties, .. } = b
                && let Some(id) = properties.get("id")
            {
                targets
                    .entry(format!("{}#{id}", s.source))
                    .or_insert_with(|| format!("chapter-{}-item-{}", i + 1, j + 1));
            }
        }
    }
    fn rewrite(
        runs: &mut [Run],
        base: &str,
        targets: &HashMap<String, String>,
        used: &mut std::collections::HashSet<String>,
    ) {
        for r in runs {
            if let Some(href) = &r.link {
                let key = if href.starts_with('#') {
                    format!("{base}{href}")
                } else if let Ok(p) = resolve(base, href) {
                    p
                } else {
                    continue;
                };
                if let Some(anchor) = targets.get(&key) {
                    used.insert(anchor.clone());
                    r.link = Some(format!("#{anchor}"))
                }
            }
        }
    }
    let mut used = std::collections::HashSet::new();
    for s in &mut doc.sections {
        for b in &mut s.blocks {
            match b {
                Block::Paragraph { runs, .. } => rewrite(runs, &s.source, &targets, &mut used),
                Block::Table { rows } => {
                    for c in rows.iter_mut().flatten() {
                        rewrite(&mut c.runs, &s.source, &targets, &mut used)
                    }
                }
            }
        }
    }
    for s in &mut doc.sections {
        if let Some(anchor) = targets.get(&s.source).filter(|a| used.contains(*a)) {
            s.properties.insert("anchor".into(), anchor.clone());
        }
        for b in &mut s.blocks {
            if let Block::Paragraph { properties, .. } = b
                && let Some(id) = properties.get("id")
                && let Some(a) = targets
                    .get(&format!("{}#{id}", s.source))
                    .filter(|a| used.contains(*a))
            {
                properties.insert("anchor".into(), a.clone());
            }
        }
    }
}
