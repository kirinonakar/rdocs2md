use crate::{
    flow::{Flow, Rules, style_props},
    model::*,
    package::*,
};
use std::collections::{HashMap, HashSet};
pub fn relationships(
    pkg: &mut Package,
    part: &str,
) -> Result<(HashMap<String, String>, HashMap<String, String>)> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let rp = format!(
        "{}_rels/{file}.rels",
        if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        }
    );
    let mut ids = HashMap::new();
    let mut types = HashMap::new();
    if pkg.has(&rp) {
        let s = pkg.text(&rp)?;
        let x = xml(&s)?;
        for r in desc(x.root(), "Relationship") {
            if let (Some(id), Some(raw), Some(t)) =
                (attr(r, "Id"), attr(r, "Target"), attr(r, "Type"))
            {
                let external = attr(r, "TargetMode") == Some("External");
                let path = if external {
                    raw.into()
                } else {
                    resolve(part, raw)?
                };
                if !external {
                    types.insert(t.rsplit('/').next().unwrap_or(t).into(), path.clone());
                }
                ids.insert(id.into(), path);
            }
        }
    }
    Ok((ids, types))
}
fn styles(pkg: &mut Package, path: &str, rules: &mut Rules, odf: bool) -> Result<()> {
    if !pkg.has(path) {
        return Ok(());
    }
    let s = pkg.text(path)?;
    let x = xml(&s)?;
    let mut raw = HashMap::new();
    for n in desc(x.root(), "style") {
        if let Some(id) = attr(n, if odf { "name" } else { "styleId" }) {
            let parent = attr(n, "parent-style-name")
                .or_else(|| child(n, "basedOn").and_then(|c| attr(c, "val")));
            let props = child(n, if odf { "text-properties" } else { "rPr" });
            raw.insert(id, (parent, props));
            if let Some(p) = child(n, "paragraph-properties") {
                let mut props = Properties::new();
                for a in p.attributes() {
                    let k = match a.name() {
                        "break-before" | "margin-left" | "text-indent" => a.name(),
                        "text-align" => "align",
                        _ => continue,
                    };
                    props.insert(k.into(), a.value().into());
                }
                rules.paragraph_styles.insert(id.into(), props);
            }
            if let Some(h) = desc(n, "outlineLvl")
                .next()
                .and_then(|c| attr(c, "val"))
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|v| *v < 6)
            {
                rules.headings.insert(id.into(), h + 1);
            }
        }
    }
    for id in raw.keys() {
        let mut chain = Vec::new();
        let mut key = Some(*id);
        let mut st = Style::default();
        while let Some(k) = key {
            if chain.len() >= 64 {
                return Err("cyclic or excessive style inheritance".into());
            }
            if let Some((p, c)) = raw.get(k) {
                chain.push(*c);
                key = *p
            } else {
                if let Some(parent) = rules.styles.get(k) {
                    st = parent.clone()
                }
                break;
            }
        }
        for props in chain.into_iter().rev().flatten() {
            style_props(props, &mut st)
        }
        rules.styles.insert(id.to_string(), st);
    }
    for n in desc(x.root(), "list-style") {
        if let Some(id) = attr(n, "name") {
            rules
                .numbers
                .insert(id.into(), child(n, "list-level-style-number").is_some());
        }
    }
    Ok(())
}
pub fn ooxml(pkg: &mut Package, main: &str, kind: &str) -> Result<Document> {
    let mut doc = Document::new(kind);
    metadata(pkg, &mut doc, "docProps/core.xml")?;
    let (rels, types) = relationships(pkg, main)?;
    let mut rules = Rules {
        rels,
        ..Rules::default()
    };
    let s = pkg.text(main)?;
    let x = xml(&s)?;
    let root = x.root_element();
    match kind {
        "docx" => {
            if tag(root) != "document" {
                return Err("invalid Word main part".into());
            }
            if let Some(p) = types.get("styles") {
                styles(pkg, p, &mut rules, false)?
            }
            if let Some(p) = types.get("numbering") {
                let s = pkg.text(p)?;
                let x = xml(&s)?;
                let defs: HashMap<_, _> = desc(x.root(), "abstractNum")
                    .filter_map(|v| {
                        Some((
                            attr(v, "abstractNumId")?,
                            attr(desc(v, "numFmt").next()?, "val")? != "bullet",
                        ))
                    })
                    .collect();
                for v in desc(x.root(), "num") {
                    if let (Some(id), Some(val)) = (
                        attr(v, "numId"),
                        child(v, "abstractNumId")
                            .and_then(|c| attr(c, "val"))
                            .and_then(|k| defs.get(k)),
                    ) {
                        rules.numbers.insert(id.into(), *val);
                    }
                }
            }
            let body = child(root, "body").ok_or("Word body missing")?;
            let flow = Flow {
                kind,
                base: main,
                rules: &rules,
            };
            let mut seen_parts = HashSet::new();
            let mut sec = Section::new("section", main);
            for n in body.children().filter(|n| n.is_element()) {
                flow.blocks(n, &mut sec.blocks)?;
                let sect = if tag(n) == "sectPr" {
                    Some(n)
                } else {
                    child(n, "pPr").and_then(|p| child(p, "sectPr"))
                };
                if let Some(settings) = sect {
                    let mut headers = Vec::new();
                    for reference in settings
                        .children()
                        .filter(|r| matches!(tag(*r), "headerReference" | "footerReference"))
                    {
                        let Some(path) = attr(reference, "id").and_then(|key| rules.rels.get(key))
                        else {
                            continue;
                        };
                        if !seen_parts.insert(path.clone()) {
                            continue;
                        }
                        let part = pkg.text(path)?;
                        let tree = xml(&part)?;
                        let mut rr = rules.clone();
                        rr.rels = relationships(pkg, path)?.0;
                        let header = tag(reference) == "headerReference";
                        let mut blocks = Vec::new();
                        Flow {
                            kind,
                            base: path,
                            rules: &rr,
                        }
                        .blocks(tree.root(), &mut blocks)?;
                        for b in &mut blocks {
                            if let Block::Paragraph { properties, .. } = b {
                                properties.insert(
                                    "role".into(),
                                    if header { "header" } else { "footer" }.into(),
                                );
                                properties.insert("source".into(), path.clone());
                            }
                        }
                        if header {
                            headers.extend(blocks)
                        } else {
                            sec.blocks.extend(blocks)
                        }
                    }
                    headers.append(&mut sec.blocks);
                    sec.blocks = headers;
                }
                if sect.is_some() && tag(n) != "sectPr" {
                    doc.sections.push(sec);
                    sec = Section::new("section", main)
                }
            }
            if !sec.blocks.is_empty() || doc.sections.is_empty() {
                doc.sections.push(sec)
            }
            for typ in ["footnotes", "endnotes"] {
                if let Some(p) = types.get(typ) {
                    let s = pkg.text(p)?;
                    let n = xml(&s)?;
                    let mut section = Section::new(typ, p);
                    let mut note_rules = rules.clone();
                    note_rules.rels = relationships(pkg, p)?.0;
                    let flow = Flow {
                        kind,
                        base: p,
                        rules: &note_rules,
                    };
                    for note in n
                        .root_element()
                        .children()
                        .filter(|n| n.is_element() && attr(*n, "type").is_none())
                    {
                        let begin = section.blocks.len();
                        flow.blocks(note, &mut section.blocks)?;
                        if let Some(Block::Paragraph { runs, .. }) = section.blocks.get_mut(begin)
                            && let Some(id) = attr(note, "id")
                        {
                            runs.insert(
                                0,
                                Run {
                                    text: id.into(),
                                    style: Style {
                                        script: Some("sup".into()),
                                        ..Style::default()
                                    },
                                    ..Run::default()
                                },
                            );
                        }
                    }
                    if !section.blocks.is_empty() {
                        doc.sections.push(section)
                    }
                }
            }
        }
        "pptx" => {
            if tag(root) != "presentation" {
                return Err("invalid presentation main part".into());
            }
            for slide in desc(root, "sldId") {
                let id = slide
                    .attributes()
                    .find(|a| a.name() == "id" && a.namespace().is_some())
                    .ok_or("slide relationship missing")?
                    .value();
                let p = rules.rels.get(id).ok_or("unknown slide relationship")?;
                let s = pkg.text(p)?;
                let x = xml(&s)?;
                let mut sec = Section::new("slide", p);
                let (r, types) = relationships(pkg, p)?;
                let slide_rules = Rules {
                    rels: r,
                    ..Rules::default()
                };
                let flow = Flow {
                    kind,
                    base: p,
                    rules: &slide_rules,
                };
                let tree = desc(x.root(), "spTree")
                    .next()
                    .ok_or("slide shape tree missing")?;
                for shape in tree.children().filter(|c| c.is_element()) {
                    let from = sec.blocks.len();
                    flow.blocks(shape, &mut sec.blocks)?;
                    if desc(shape, "ph")
                        .any(|p| matches!(attr(p, "type"), Some("title" | "ctrTitle")))
                        && let Some(Block::Paragraph { heading, .. }) = sec.blocks.get_mut(from)
                    {
                        *heading = 1
                    }
                }
                if let Some(p) = types.get("notesSlide") {
                    let s = pkg.text(p)?;
                    let x = xml(&s)?;
                    for sp in desc(x.root(), "sp") {
                        if desc(sp, "ph").any(|p| attr(p, "type") == Some("body")) {
                            let start = sec.blocks.len();
                            flow.blocks(sp, &mut sec.blocks)?;
                            for b in &mut sec.blocks[start..] {
                                if let Block::Paragraph { properties, .. } = b {
                                    properties.insert("role".into(), "speaker-notes".into());
                                }
                            }
                        }
                    }
                }
                doc.sections.push(sec);
            }
        }
        "xlsx" => return crate::sheets::xlsx(pkg, doc, root, &rules.rels, &types),
        _ => return Err("unknown office format".into()),
    }
    Ok(doc)
}
pub fn odf(pkg: &mut Package, kind: &str) -> Result<Document> {
    let mut doc = Document::new(kind);
    metadata(pkg, &mut doc, "meta.xml")?;
    if pkg.has("META-INF/manifest.xml") {
        let s = pkg.text("META-INF/manifest.xml")?;
        if desc(xml(&s)?.root(), "encryption-data").next().is_some() {
            return Err("encrypted OpenDocument is unsupported".into());
        }
    }
    if pkg.has("settings.xml") {
        let s = pkg.text("settings.xml")?;
        let x = xml(&s)?;
        for v in desc(x.root(), "config-item") {
            if let Some(key) = attr(v, "name").filter(|k| {
                matches!(
                    *k,
                    "ShowGrid" | "ShowNotes" | "ViewAreaTop" | "ViewAreaLeft"
                )
            }) {
                doc.metadata.insert(key.into(), all_text(v));
            }
        }
    }
    let mut rules = Rules::default();
    styles(pkg, "styles.xml", &mut rules, true)?;
    styles(pkg, "content.xml", &mut rules, true)?;
    let s = pkg.text("content.xml")?;
    let x = xml(&s)?;
    let body = desc(x.root(), "body").next().ok_or("ODF body missing")?;
    for object in desc(body, "object") {
        if let Some(href) = attr(object, "href")
            && !rules.objects.contains_key(href)
            && let Ok(p) = resolve("content.xml", &format!("{href}/content.xml"))
            && pkg.has(&p)
        {
            let part = pkg.text(&p)?;
            let tree = xml(&part)?;
            if let Some(n) = desc(tree.root(), "math").next() {
                rules.objects.insert(href.into(), crate::math::formula(n));
            }
        }
    }
    let f = Flow {
        kind,
        base: "content.xml",
        rules: &rules,
    };
    let typ = match kind {
        "odt" => "text",
        "ods" => "spreadsheet",
        _ => "presentation",
    };
    let root = child(body, typ).ok_or("ODF body and MIME type disagree")?;
    match kind {
        "ods" => {
            for n in root.children().filter(|n| tag(*n) == "table") {
                let mut sec = Section::new("sheet", "content.xml");
                sec.title = attr(n, "name").unwrap_or("").into();
                if let Some(v) = attr(n, "display") {
                    sec.properties.insert("display".into(), v.into());
                }
                let Block::Table { rows } = f.table(n)? else {
                    unreachable!()
                };
                let mut area = Vec::new();
                for row in rows {
                    if row.iter().all(|c| {
                        c.runs
                            .iter()
                            .all(|r| r.text.is_empty() && r.image.is_none())
                            && c.blocks.is_empty()
                            && c.properties.is_empty()
                            && c.colspan.is_none()
                            && c.rowspan.is_none()
                    }) {
                        if !area.is_empty() {
                            sec.blocks.push(Block::Table {
                                rows: std::mem::take(&mut area),
                            })
                        }
                    } else {
                        area.push(row)
                    }
                }
                if !area.is_empty() {
                    sec.blocks.push(Block::Table { rows: area })
                }
                doc.sections.push(sec)
            }
        }
        "odp" => {
            for n in root.children().filter(|n| tag(*n) == "page") {
                let mut sec = Section::new("slide", "content.xml");
                sec.title = attr(n, "name").unwrap_or("").into();
                let mut shapes: Vec<_> = n.children().filter(|n| n.is_element()).collect();
                shapes.sort_by_key(|n| {
                    (
                        tag(*n) == "notes",
                        length(attr(*n, "y")),
                        length(attr(*n, "x")),
                    )
                });
                for shape in shapes {
                    let start = sec.blocks.len();
                    f.blocks(shape, &mut sec.blocks)?;
                    for b in &mut sec.blocks[start..] {
                        if let Block::Paragraph {
                            heading,
                            properties,
                            ..
                        } = b
                        {
                            if attr(shape, "class") == Some("title") {
                                *heading = 1
                            }
                            if tag(shape) == "notes" {
                                properties.insert("role".into(), "speaker-notes".into());
                            }
                        }
                    }
                }
                doc.sections.push(sec)
            }
        }
        _ => {
            let mut sec = Section::new("section", "content.xml");
            f.blocks(root, &mut sec.blocks)?;
            doc.sections.push(sec)
        }
    }
    Ok(doc)
}
fn length(v: Option<&str>) -> i64 {
    let s = v.unwrap_or("");
    let cut = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let scale = match &s[cut..] {
        "in" => 25400.,
        "cm" => 10000.,
        "mm" => 1000.,
        "pt" => 25400. / 72.,
        _ => 1.,
    };
    (s[..cut].parse::<f64>().unwrap_or(0.) * scale) as i64
}
pub fn hwpx(pkg: &mut Package) -> Result<Document> {
    let mut doc = Document::new("hwpx");
    let mut rules = Rules::default();
    let mut sections = Vec::new();
    let s = pkg.text("Contents/content.hpf")?;
    let x = xml(&s)?;
    let mut ids = HashMap::new();
    for n in desc(x.root(), "item") {
        if let (Some(i), Some(h)) = (attr(n, "id"), attr(n, "href")) {
            let p = if pkg.has(h) {
                h.into()
            } else {
                resolve("Contents/content.hpf", h)?
            };
            if attr(n, "media-type").is_some_and(|m| m.starts_with("image/")) {
                rules.images.insert(i.into(), p.clone());
            }
            ids.insert(i, p);
        }
    }
    for n in desc(x.root(), "itemref") {
        if let Some(p) = attr(n, "idref").and_then(|i| ids.get(i))
            && p.rsplit('/')
                .next()
                .is_some_and(|v| v.starts_with("section"))
        {
            sections.push(p.clone())
        }
    }
    if sections.is_empty() {
        sections = pkg
            .names()
            .filter(|p| p.starts_with("Contents/section") && p.ends_with(".xml"))
            .map(str::to_owned)
            .collect();
        sections.sort_by_key(|p| {
            p.trim_start_matches("Contents/section")
                .trim_end_matches(".xml")
                .parse::<u32>()
                .unwrap_or(u32::MAX)
        });
    }
    if pkg.has("Contents/header.xml") {
        let s = pkg.text("Contents/header.xml")?;
        let x = xml(&s)?;
        for n in desc(x.root(), "charPr") {
            if let Some(id) = attr(n, "id") {
                let mut style = Style::default();
                style_props(n, &mut style);
                style.size = attr(n, "height")
                    .and_then(|v| v.parse::<f32>().ok())
                    .map(|n| format!("{}pt", n / 100.));
                rules.styles.insert(id.into(), style);
            }
        }
    }
    metadata(pkg, &mut doc, "Contents/content.hpf")?;
    for p in sections {
        let s = pkg.text(&p)?;
        let x = xml(&s)?;
        let mut sec = Section::new("section", &p);
        Flow {
            kind: "hwpx",
            base: &p,
            rules: &rules,
        }
        .blocks(x.root(), &mut sec.blocks)?;
        doc.sections.push(sec)
    }
    Ok(doc)
}
pub fn package(data: &[u8], images: bool) -> Result<Document> {
    let mut pkg = Package::new(data)?;
    let mime = if pkg.has("mimetype") {
        pkg.text("mimetype")?
    } else {
        String::new()
    };
    let mut d = match mime.trim() {
        "application/epub+zip" => crate::epub::parse(&mut pkg)?,
        "application/vnd.oasis.opendocument.text" => odf(&mut pkg, "odt")?,
        "application/vnd.oasis.opendocument.spreadsheet" => odf(&mut pkg, "ods")?,
        "application/vnd.oasis.opendocument.presentation" => odf(&mut pkg, "odp")?,
        _ if pkg.has("Contents/content.hpf") => hwpx(&mut pkg)?,
        _ if pkg.has("[Content_Types].xml") => {
            let (_, ts) = relationships(&mut pkg, "")?;
            let main = ts
                .get("officeDocument")
                .ok_or("OPC officeDocument relationship missing")?;
            let s = pkg.text("[Content_Types].xml")?;
            let x = xml(&s)?;
            let part = format!("/{main}");
            let mime = desc(x.root(), "Override")
                .find(|n| attr(*n, "PartName") == Some(&part))
                .and_then(|n| attr(n, "ContentType"))
                .ok_or("main part content type missing")?;
            let kind = if mime.contains("wordprocessingml.document.main") {
                "docx"
            } else if mime.contains("spreadsheetml.sheet.main") {
                "xlsx"
            } else if mime.contains("presentationml.presentation.main") {
                "pptx"
            } else {
                return Err("unsupported Office content type".into());
            };
            ooxml(&mut pkg, main, kind)?
        }
        _ => return Err("unrecognized ZIP document structure".into()),
    };
    if matches!(d.format.as_str(), "docx" | "odt" | "hwpx") {
        d.explicit_pages()
    }
    if images {
        let mut missing = HashSet::new();
        for i in image_sources(&d) {
            if pkg.has(&i) {
                let b = pkg.bytes(&i)?;
                if image_ext(&b).is_some() {
                    d.assets.insert(i, b);
                } else {
                    missing.insert(i);
                }
            } else {
                missing.insert(i);
            }
        }
        if !missing.is_empty() {
            d.warn("Some images were missing, external or in unsupported encodings.")
        }
    }
    Ok(d)
}
