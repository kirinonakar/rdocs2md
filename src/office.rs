use crate::{
    flow::{Flow, Rules, style_props},
    formatting,
    model::*,
    package::*,
    sheets::record_hidden_axis,
    visibility::flag_text,
};
use std::collections::{HashMap, HashSet};
mod slides;
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
    read_styles(x.root(), rules, odf)
}

fn read_styles(root: N, rules: &mut Rules, odf: bool) -> Result<()> {
    let mut raw = HashMap::new();
    for n in desc(root, "style") {
        if let Some(id) = attr(n, if odf { "name" } else { "styleId" }) {
            let parent = attr(n, "parent-style-name")
                .or_else(|| child(n, "basedOn").and_then(|c| attr(c, "val")));
            raw.insert(id, (parent, n));
            if let Some(h) = attr(n, "default-outline-level")
                .and_then(|level| level.parse::<u8>().ok())
                .filter(|&level| level > 0)
                .or_else(|| {
                    desc(n, "outlineLvl")
                        .next()
                        .and_then(|c| attr(c, "val"))
                        .and_then(|v| v.parse::<u8>().ok())
                        .map(|level| if level < 9 { level + 1 } else { 0 })
                })
            {
                rules.headings.insert(id.into(), h.min(6));
            }
        }
    }
    for id in raw.keys() {
        let mut chain = Vec::new();
        let mut key = Some(*id);
        let mut st = Style::default();
        let mut paragraph = Properties::new();
        let mut cell = Properties::new();
        let mut table_borders = Properties::new();
        let mut table_cells = Properties::new();
        let mut table_display = None;
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
                paragraph = rules.paragraph_styles.get(k).cloned().unwrap_or_default();
                cell = rules.cell_styles.get(k).cloned().unwrap_or_default();
                table_borders = rules.table_borders.get(k).cloned().unwrap_or_default();
                table_cells = rules.table_cells.get(k).cloned().unwrap_or_default();
                table_display = rules.table_display.get(k).cloned();
                break;
            }
        }
        for node in chain.into_iter().rev() {
            if let Some(props) = child(node, if odf { "text-properties" } else { "rPr" }) {
                style_props(props, &mut st);
            }
            if odf {
                if let Some(value) =
                    child(node, "table-properties").and_then(|n| attr(n, "display"))
                {
                    table_display = Some(value.into());
                }
                if let Some(p) = child(node, "paragraph-properties") {
                    for a in p.attributes() {
                        let k = match a.name() {
                            "break-before" | "margin-left" | "margin-right" | "margin-top"
                            | "margin-bottom" | "text-indent" | "line-height" => a.name(),
                            "text-align" => "align",
                            _ => continue,
                        };
                        paragraph.insert(k.into(), a.value().into());
                    }
                }
                if let Some(p) = child(node, "table-cell-properties") {
                    for a in p.attributes() {
                        if a.name() == "padding" {
                            for key in [
                                "padding-left",
                                "padding-right",
                                "padding-top",
                                "padding-bottom",
                            ] {
                                formatting::box_property(&mut cell, key, a.value());
                            }
                        } else {
                            formatting::box_property(&mut cell, a.name(), a.value());
                        }
                    }
                }
            } else {
                if let Some(p) = child(node, "pPr") {
                    formatting::word_paragraph(&mut paragraph, p);
                }
                if let Some(p) = child(node, "tblPr") {
                    if let Some(edges) = child(p, "tblBorders") {
                        table_borders.extend(formatting::word_borders(edges));
                    }
                    formatting::word_cell(&mut table_cells, p);
                }
                if let Some(p) = child(node, "tcPr") {
                    formatting::word_cell(&mut table_cells, p);
                }
            }
        }
        rules.styles.insert(id.to_string(), st);
        if let Some(display) = table_display {
            rules.table_display.insert(id.to_string(), display);
        }
        for (map, props) in [
            (&mut rules.paragraph_styles, paragraph),
            (&mut rules.cell_styles, cell),
            (&mut rules.table_borders, table_borders),
            (&mut rules.table_cells, table_cells),
        ] {
            if !props.is_empty() {
                map.insert(id.to_string(), props);
            }
        }
    }
    for n in desc(root, "list-style") {
        if let Some(id) = attr(n, "name") {
            let levels = n.children().filter_map(|definition| {
                let ordered = match tag(definition) {
                    "list-level-style-number" => true,
                    "list-level-style-bullet" | "list-level-style-image" => false,
                    _ => return None,
                };
                let level = attr(definition, "level")
                    .and_then(|level| level.parse::<usize>().ok())
                    .unwrap_or(1);
                Some((level.saturating_sub(1), Some(ordered)))
            });
            rules.numbers.insert(id.into(), levels.collect());
        }
    }
    Ok(())
}
fn word_number_level(node: N) -> Option<(usize, Option<bool>)> {
    if tag(node) != "lvl" {
        return None;
    }
    let level = attr(node, "ilvl")?
        .parse::<usize>()
        .ok()
        .filter(|&n| n < 9)?;
    let kind = attr(child(node, "numFmt")?, "val")?;
    Some((level, (kind != "none").then_some(kind != "bullet")))
}

fn ooxml(
    pkg: &mut Package,
    main: &str,
    kind: &str,
    range: Option<&SectionRange>,
    images: bool,
) -> Result<Document> {
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
                let defs: HashMap<_, HashMap<_, _>> = desc(x.root(), "abstractNum")
                    .filter_map(|v| {
                        Some((
                            attr(v, "abstractNumId")?,
                            v.children().filter_map(word_number_level).collect(),
                        ))
                    })
                    .collect();
                for v in desc(x.root(), "num") {
                    if let Some(id) = attr(v, "numId") {
                        let mut levels = child(v, "abstractNumId")
                            .and_then(|c| attr(c, "val"))
                            .and_then(|k| defs.get(k))
                            .cloned()
                            .unwrap_or_default();
                        for override_level in v.children().filter(|n| tag(*n) == "lvlOverride") {
                            if let Some((level, kind)) =
                                child(override_level, "lvl").and_then(word_number_level)
                            {
                                levels.insert(level, kind);
                            }
                        }
                        rules.numbers.insert(id.into(), levels);
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
            if rules.deleted.get() {
                doc.warn("The document has tracked deletions. The deleted text was omitted, so the extracted text reflects only the current content.");
            }
            // Review comments carry the reviewer's intent, which is often the
            // reason the document is being read at all, so they get their own
            // section with the author and date kept as properties.
            if let Some(p) = types.get("comments") {
                let s = pkg.text(p)?;
                let n = xml(&s)?;
                let mut section = Section::new("comments", p);
                let mut comment_rules = rules.clone();
                comment_rules.rels = relationships(pkg, p)?.0;
                let flow = Flow {
                    kind,
                    base: p,
                    rules: &comment_rules,
                };
                for comment in n
                    .root_element()
                    .children()
                    .filter(|c| c.is_element() && tag(*c) == "comment")
                {
                    let begin = section.blocks.len();
                    flow.blocks(comment, &mut section.blocks)?;
                    let Some(Block::Paragraph {
                        runs, properties, ..
                    }) = section.blocks.get_mut(begin)
                    else {
                        continue;
                    };
                    let mut label = String::from("[comment");
                    if let Some(id) = attr(comment, "id") {
                        label.push(' ');
                        label.push_str(id);
                        properties.insert("comment_id".into(), id.into());
                    }
                    if let Some(author) = attr(comment, "author").filter(|a| !a.is_empty()) {
                        label.push_str(" by ");
                        label.push_str(author);
                        properties.insert("author".into(), author.into());
                    }
                    if let Some(date) = attr(comment, "date").filter(|d| !d.is_empty()) {
                        label.push_str(", ");
                        label.push_str(date);
                        properties.insert("date".into(), date.into());
                    }
                    label.push_str("] ");
                    properties.insert("role".into(), "comment".into());
                    runs.insert(0, Run::text(label));
                }
                if !section.blocks.is_empty() {
                    doc.sections.push(section)
                }
            }
        }
        "pptx" => {
            if tag(root) != "presentation" {
                return Err("invalid presentation main part".into());
            }
            let slides: Vec<_> = desc(root, "sldId").collect();
            let slide_size = child(root, "sldSz")
                .and_then(|n| dimensions(n, "cx", "cy"))
                .filter(|&(w, h)| w > 0. && h > 0.);
            let positioned_size =
                slide_size.filter(|&(w, h)| images && w <= 190_500_000. && h <= 190_500_000.);
            let palette = if positioned_size.is_some() {
                slides::palette(pkg, types.get("theme")).unwrap_or_default()
            } else {
                slides::Palette::new()
            };
            let mut layouts = HashMap::new();
            let empty_template = slides::Template::default();
            for i in section_indices(range, slides.len())? {
                let slide = slides[i];
                let id = slide
                    .attributes()
                    .find(|a| a.name() == "id" && a.namespace().is_some())
                    .ok_or("slide relationship missing")?
                    .value();
                let p = rules.rels.get(id).ok_or("unknown slide relationship")?;
                let s = pkg.text(p)?;
                let x = xml(&s)?;
                let mut sec = Section::new("slide", p).indexed(i);
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
                let template = if positioned_size.is_some()
                    && let Some(path) = types.get("slideLayout")
                {
                    layouts.entry(path.clone()).or_insert_with(|| {
                        slides::Template::read(pkg, path, &palette).unwrap_or_default()
                    })
                } else {
                    &empty_template
                };
                let reader = slides::SlideReader {
                    flow: &flow,
                    template,
                    palette: &palette,
                };
                let mut frames = Vec::new();
                for shape in tree.children().filter(|c| c.is_element()) {
                    let from = sec.blocks.len();
                    if positioned_size.is_some() {
                        reader.collect(shape, slides::IDENTITY, &mut sec.blocks, &mut frames)?;
                    } else {
                        flow.blocks(shape, &mut sec.blocks)?;
                    }
                    if let Some(bounds) = slide_size
                        && off_slide(shape, bounds)
                    {
                        flag_text(&mut sec.blocks[from..], VisibilityFlag::OffPage);
                    }
                    if desc(shape, "ph")
                        .any(|p| matches!(attr(p, "type"), Some("title" | "ctrTitle")))
                        && let Some(Block::Paragraph { heading, .. }) = sec.blocks.get_mut(from)
                    {
                        *heading = 1
                    }
                }
                if let Some((w, h)) = positioned_size {
                    let background = slides::background(x.root_element(), &palette)
                        .or_else(|| template.background.clone());
                    if !frames.is_empty() || background.is_some() {
                        sec.slide_layout = Some(SlideLayout {
                            size: [w / 9525., h / 9525.],
                            background,
                            frames,
                        });
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
        "xlsx" => return crate::sheets::xlsx(pkg, doc, root, &rules.rels, &types, range),
        _ => return Err("unknown office format".into()),
    }
    Ok(doc)
}
fn odf(pkg: &mut Package, kind: &str, range: Option<&SectionRange>) -> Result<Document> {
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
    let s = pkg.text("content.xml")?;
    let x = xml(&s)?;
    read_styles(x.root(), &mut rules, true)?;
    let body = desc(x.root(), "body").next().ok_or("ODF body missing")?;
    let (typ, section_tag) = match kind {
        "odt" => ("text", ""),
        "ods" => ("spreadsheet", "table"),
        _ => ("presentation", "page"),
    };
    let root = child(body, typ).ok_or("ODF body and MIME type disagree")?;
    let sources: Vec<_> = root.children().filter(|n| tag(*n) == section_tag).collect();
    let selected: Vec<_> = if kind == "odt" {
        vec![(0, root)]
    } else {
        section_indices(range, sources.len())?
            .map(|i| (i, sources[i]))
            .collect()
    };
    for object in selected.iter().flat_map(|&(_, n)| desc(n, "object")) {
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
    match kind {
        "ods" => {
            for (i, n) in selected {
                let mut sec = Section::new("sheet", "content.xml").indexed(i);
                sec.title = attr(n, "name").unwrap_or("").into();
                if let Some(v) = attr(n, "display").or_else(|| {
                    attr(n, "style-name")
                        .and_then(|id| rules.table_display.get(id))
                        .map(String::as_str)
                }) {
                    sec.properties.insert("display".into(), v.into());
                    if matches!(v, "false" | "0") {
                        sec.properties.insert("hidden".into(), "true".into());
                    }
                }
                let columns = ods_hidden_axis(n, "table-column", "number-columns-repeated")?;
                let hidden_rows = ods_hidden_axis(n, "table-row", "number-rows-repeated")?;
                record_hidden_axis(&mut sec, "hidden_columns", &columns);
                record_hidden_axis(&mut sec, "hidden_rows", &hidden_rows);
                let Block::Table { rows } = f.table_with_hidden_columns(n, &columns)? else {
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
            for (i, n) in selected {
                let mut sec = Section::new("slide", "content.xml").indexed(i);
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

fn dimensions(node: N, x: &str, y: &str) -> Option<(f64, f64)> {
    let x = attr(node, x)?.parse::<i64>().ok()? as f64;
    let y = attr(node, y)?.parse::<i64>().ok()? as f64;
    Some((x, y))
}

fn off_slide(shape: N, (width, height): (f64, f64)) -> bool {
    let transform = child(shape, "spPr")
        .and_then(|p| child(p, "xfrm"))
        .or_else(|| child(shape, "xfrm"));
    let Some((x, y, w, h, angle)) = transform.and_then(|n| {
        let (x, y) = dimensions(child(n, "off")?, "x", "y")?;
        let (w, h) = dimensions(child(n, "ext")?, "cx", "cy")?;
        let angle = attr(n, "rot")
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0);
        (w >= 0. && h >= 0.).then_some((x, y, w, h, angle))
    }) else {
        return false;
    };
    let (sin, cos) = (f64::from(angle) / 60000.).to_radians().sin_cos();
    let (x, y) = (x + w / 2., y + h / 2.);
    let (rx, ry) = (
        (w * cos.abs() + h * sin.abs()) / 2.,
        (w * sin.abs() + h * cos.abs()) / 2.,
    );
    x + rx < 0. || y + ry < 0. || x - rx > width || y - ry > height
}

fn ods_hidden_axis(sheet: N, tag_name: &str, repeat_attr: &str) -> Result<SectionRange> {
    let mut spans = Vec::new();
    let mut offset = 0usize;
    for node in desc(sheet, tag_name)
        .filter(|n| n.ancestors().skip(1).find(|a| tag(*a) == "table") == Some(sheet))
    {
        let count = attr(node, repeat_attr).unwrap_or("1").parse::<usize>()?;
        let end = offset
            .checked_add(count)
            .ok_or("invalid sheet repeat count")?;
        if count != 0 && crate::sheets::ods_axis_hidden(node, sheet) {
            spans.push((offset + 1, end));
        }
        offset = end;
    }
    Ok(SectionRange::from_spans(spans))
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
fn hwpx(pkg: &mut Package, range: Option<&SectionRange>) -> Result<Document> {
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
        let fonts: HashMap<_, _> = desc(x.root(), "fontface")
            .filter(|n| attr(*n, "lang") == Some("HANGUL"))
            .flat_map(|n| n.children().filter(|c| tag(*c) == "font"))
            .filter_map(|n| Some((attr(n, "id")?, attr(n, "face")?)))
            .collect();
        for n in desc(x.root(), "borderFill") {
            if let Some(id) = attr(n, "id") {
                rules
                    .hwpx_borders
                    .insert(id.into(), formatting::hwpx_border_fill(n));
            }
        }
        for n in desc(x.root(), "paraPr") {
            if let Some(id) = attr(n, "id") {
                let mut props = Properties::new();
                if let Some(align) = child(n, "align").and_then(|n| attr(n, "horizontal")) {
                    props.insert("align".into(), align.into());
                }
                if let Some(head) = child(n, "heading").or_else(|| {
                    child(n, "switch")
                        .and_then(|switch| {
                            child(switch, "case").or_else(|| child(switch, "default"))
                        })
                        .and_then(|branch| child(branch, "heading"))
                }) {
                    let level = attr(head, "level")
                        .and_then(|level| level.parse::<u8>().ok())
                        .unwrap_or(0)
                        .min(7);
                    match attr(head, "type") {
                        Some("OUTLINE") => {
                            rules.headings.insert(id.into(), (level + 1).min(6));
                        }
                        Some(kind @ ("NUMBER" | "BULLET")) => {
                            rules.hwpx_lists.insert(
                                id.into(),
                                List {
                                    ordered: kind == "NUMBER",
                                    depth: usize::from(level),
                                },
                            );
                        }
                        _ => {}
                    }
                }
                rules.paragraph_styles.insert(id.into(), props);
            }
        }
        for n in desc(x.root(), "charPr") {
            if let Some(id) = attr(n, "id") {
                let mut style = Style::default();
                style_props(n, &mut style);
                if let Some(font) = child(n, "fontRef")
                    .and_then(|font| attr(font, "hangul"))
                    .and_then(|id| fonts.get(id))
                {
                    style.font = Some((*font).into());
                }
                style.size = attr(n, "height")
                    .and_then(|v| v.parse::<f32>().ok())
                    .map(|n| format!("{}pt", n / 100.));
                rules.styles.insert(id.into(), style);
            }
        }
    }
    metadata(pkg, &mut doc, "Contents/content.hpf")?;
    let mut pages = Vec::new();
    let mut index = 0;
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
        sec.append_explicit_pages(&mut pages);
        for page in pages.drain(..) {
            index += 1;
            if range.is_none_or(|r| r.contains(index)) {
                doc.sections.push(page.indexed(index - 1));
            }
            if range.is_some_and(|r| r.ends_at(index)) {
                return Ok(doc);
            }
        }
    }
    if let Some(range) = range {
        range.validate(index)?;
    }
    Ok(doc)
}
pub fn package(data: &[u8], images: bool, range: Option<&SectionRange>) -> Result<Document> {
    let mut pkg = Package::new(data)?;
    let mime = if pkg.has("mimetype") {
        pkg.text("mimetype")?
    } else {
        String::new()
    };
    let mut d = match mime.trim() {
        "application/epub+zip" => crate::epub::parse(&mut pkg, range)?,
        "application/vnd.oasis.opendocument.text" => odf(&mut pkg, "odt", range)?,
        "application/vnd.oasis.opendocument.spreadsheet" => odf(&mut pkg, "ods", range)?,
        "application/vnd.oasis.opendocument.presentation" => odf(&mut pkg, "odp", range)?,
        _ if pkg.has("Contents/content.hpf") => hwpx(&mut pkg, range)?,
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
            ooxml(&mut pkg, main, kind, range, images)?
        }
        _ => return Err("unrecognized ZIP document structure".into()),
    };
    if matches!(d.format.as_str(), "docx" | "odt") {
        d.explicit_pages();
        d.retain_sections(range)?;
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
