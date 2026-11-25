use crate::{formatting, model::*, package::*};
use std::collections::HashMap;
#[derive(Default, Clone)]
pub struct Rules {
    pub paragraph_styles: HashMap<String, Properties>,
    pub cell_styles: HashMap<String, Properties>,
    pub table_borders: HashMap<String, Properties>,
    pub table_cells: HashMap<String, Properties>,
    pub table_display: HashMap<String, String>,
    pub hwpx_borders: HashMap<String, Properties>,
    pub table_bytes: std::rc::Rc<std::cell::Cell<usize>>,
    pub objects: HashMap<String, Run>,
    pub styles: HashMap<String, Style>,
    pub headings: HashMap<String, u8>,
    pub numbers: HashMap<String, bool>,
    pub rels: HashMap<String, String>,
    pub images: HashMap<String, String>,
    pub css: HashMap<String, String>,
    /// Set when a tracked deletion is dropped, so the caller can warn that the
    /// extracted text no longer matches the printed document.
    pub deleted: std::rc::Rc<std::cell::Cell<bool>>,
}
fn apply_style(to: &mut Style, s: &Style) {
    to.bold |= s.bold;
    to.italic |= s.italic;
    to.underline |= s.underline;
    to.strike |= s.strike;
    if s.size.is_some() {
        to.size = s.size.clone()
    }
    if s.color.is_some() {
        to.color = s.color.clone()
    }
    if s.font.is_some() {
        to.font = s.font.clone()
    }
    if s.script.is_some() {
        to.script = s.script.clone()
    }
    if s.hidden.is_some() {
        to.hidden = s.hidden;
    }
}
pub fn css_style(s: &str, st: &mut Style) {
    for d in s.split(';') {
        if let Some((k, v)) = d.split_once(':') {
            match (k.trim(), v.trim()) {
                ("font-weight", "bold" | "700" | "800" | "900") => st.bold = true,
                ("font-style", "italic" | "oblique") => st.italic = true,
                ("text-decoration", v) => {
                    st.underline |= v.contains("underline");
                    st.strike |= v.contains("line-through")
                }
                ("vertical-align", "super") => st.script = Some("sup".into()),
                ("vertical-align", "sub") => st.script = Some("sub".into()),
                ("font-size", v) => st.size = Some(v.into()),
                ("font-family", v) => st.font = Some(v.into()),
                ("color", v) => st.color = formatting::color(v),
                ("visibility", "hidden" | "collapse") => st.hidden = Some(true),
                ("visibility", "visible") => st.hidden = Some(false),
                _ => {}
            }
        }
    }
}
pub fn style_props(n: N, st: &mut Style) {
    for a in n.attributes() {
        let v = a.value();
        match a.name() {
            "font-weight" | "b" if matches!(v, "bold" | "1" | "true") => st.bold = true,
            "font-style" | "i" if matches!(v, "italic" | "1" | "true") => st.italic = true,
            "text-underline-style" | "u" if !matches!(v, "none" | "0") => st.underline = true,
            "text-line-through-style" | "strike" if !matches!(v, "none" | "noStrike") => {
                st.strike = true
            }
            "font-name" | "font-family" | "typeface" | "ascii" => st.font = Some(v.into()),
            "font-size" => st.size = Some(v.into()),
            "sz" => st.size = v.parse::<f32>().ok().map(|n| format!("{}pt", n / 100.)),
            "color" | "textColor" => st.color = formatting::color(v),
            "text-position" if v.starts_with("super") => st.script = Some("sup".into()),
            "text-position" if v.starts_with("sub") => st.script = Some("sub".into()),
            "display" if v == "none" => st.hidden = Some(true),
            _ => {}
        }
    }
    for c in n.children().filter(|v| v.is_element()) {
        let v = attr(c, "val").unwrap_or("true");
        let on = !matches!(v, "0" | "false" | "none" | "off");
        match tag(c) {
            "b" | "bold" => st.bold = on,
            "italic" | "i" => st.italic = on,
            "underline" if attr(c, "type").is_some() => {
                st.underline = attr(c, "type") == Some("BOTTOM");
            }
            "u" | "underline" => st.underline = on,
            "strike" => st.strike = on,
            "vanish" | "specVanish" => st.hidden = Some(on),
            "vertAlign" => {
                st.script = match v {
                    "superscript" => Some("sup".into()),
                    "subscript" => Some("sub".into()),
                    _ => None,
                }
            }
            "sz" => st.size = v.parse::<f32>().ok().map(|n| format!("{}pt", n / 2.)),
            "color" => st.color = formatting::color(v),
            "rFonts" | "latin" => style_props(c, st),
            _ => {}
        }
    }
}
pub struct Flow<'a> {
    pub kind: &'a str,
    pub base: &'a str,
    pub rules: &'a Rules,
}
impl Flow<'_> {
    fn prose(&self) -> bool {
        matches!(self.kind, "epub" | "odt" | "odp" | "ods")
    }
    fn css(&self, n: N) -> String {
        let mut style = String::new();
        if self.kind == "epub" {
            let mut selectors = vec![tag(n).to_string()];
            if let Some(classes) = attr(n, "class") {
                for c in classes.split_whitespace() {
                    selectors.push(format!(".{c}"));
                    selectors.push(format!("{}.{c}", tag(n)))
                }
            }
            if let Some(id) = attr(n, "id") {
                selectors.push(format!("#{id}"))
            }
            for key in selectors {
                if let Some(s) = self.rules.css.get(&key) {
                    style.push_str(s);
                    style.push(';')
                }
            }
            style.push_str(attr(n, "style").unwrap_or(""));
        }
        style
    }
    fn inline(&self, n: N, state: &Run, out: &mut Vec<Run>) -> Result<()> {
        if n.is_text() {
            if self.prose()
                || n.parent_element()
                    .is_some_and(|p| matches!(tag(p), "t" | "instrText" | "m:t"))
            {
                let s = n.text().unwrap_or("");
                if !s.is_empty() {
                    let mut r = state.clone();
                    r.text = if self.kind == "epub" {
                        s.replace(['\n', '\r', '\t'], " ")
                    } else {
                        s.into()
                    };
                    push_run(out, r)
                }
            }
            return Ok(());
        }
        let name = tag(n);
        if matches!(name, "soft-page-break" | "lastRenderedPageBreak")
            || name == "br" && attr(n, "type") == Some("page")
        {
            out.push(Run {
                page_break: true,
                ..Run::default()
            });
            return Ok(());
        }
        if matches!(name, "math" | "oMath") {
            push_run(out, crate::math::formula(n));
            return Ok(());
        }
        if name == "object"
            && let Some(object) = attr(n, "href").and_then(|v| self.rules.objects.get(v))
        {
            push_run(out, object.clone());
            return Ok(());
        }
        if name == "equation"
            && self.kind == "hwpx"
            && let Some(script) = child(n, "script")
        {
            push_run(out, Run::text(all_text(script)));
            return Ok(());
        }
        if matches!(name,"pPr"|"rPr"|"script"|"style"|"delText"|"instrText"|"del" if self.kind=="docx")
        {
            if matches!(name, "del" | "delText") {
                self.rules.deleted.set(true);
            }
            return Ok(());
        }
        if matches!(
            name,
            "script"
                | "style"
                | "object"
                | "iframe"
                | "binary-data"
                | "event-listeners"
                | "annotationEnd"
                | "tblPr"
                | "tcPr"
                | "footnoteRef"
                | "endnoteRef"
        ) {
            return Ok(());
        }
        let mut st = state.clone();
        if let Some(s) = attr(n, "style-name")
            .or_else(|| attr(n, "charPrIDRef"))
            .and_then(|k| self.rules.styles.get(k))
        {
            apply_style(&mut st.style, s)
        }
        let css = self.css(n);
        if css.split(';').any(|d| {
            d.split_once(':')
                .is_some_and(|(k, v)| k.trim() == "display" && v.trim() == "none")
        }) {
            return Ok(());
        }
        css_style(&css, &mut st.style);
        if let Some(p) = child(n, "rPr") {
            if let Some(s) = child(p, "rStyle")
                .and_then(|n| attr(n, "val"))
                .and_then(|id| self.rules.styles.get(id))
            {
                apply_style(&mut st.style, s)
            }
            style_props(p, &mut st.style);
            if let Some(l) = child(p, "hlinkClick")
                .and_then(|n| attr(n, "id"))
                .and_then(|id| self.rules.rels.get(id))
            {
                st.link = Some(l.clone())
            }
        }
        match name {
            "strong" | "b" => st.style.bold = true,
            "em" | "i" => st.style.italic = true,
            "u" => st.style.underline = true,
            "s" if self.kind == "epub" => st.style.strike = true,
            "del" | "strike" => st.style.strike = true,
            "sup" | "sub" | "rt" => st.style.script = Some(name.into()),
            "a" | "hyperlink" => {
                st.link = attr(n, "href")
                    .map(str::to_owned)
                    .or_else(|| attr(n, "id").and_then(|id| self.rules.rels.get(id).cloned()))
                    .or_else(|| attr(n, "anchor").map(|s| format!("#{s}")))
            }
            "br" | "line-break" | "lineBreak" | "cr" => {
                st.text = "\n".into();
                push_run(out, st);
                return Ok(());
            }
            "tab" => {
                st.text = "\t".into();
                push_run(out, st);
                return Ok(());
            }
            "s" if self.prose() => {
                let count = attr(n, "c").unwrap_or("1").parse::<usize>()?;
                if count > 4096 {
                    return Err("space repeat limit exceeded".into());
                }
                st.text = " ".repeat(count);
                push_run(out, st);
                return Ok(());
            }
            "img" | "image" | "blip" => {
                let target =
                    if let Some(id) = attr(n, "embed").or_else(|| attr(n, "binaryItemIDRef")) {
                        self.rules
                            .rels
                            .get(id)
                            .or_else(|| self.rules.images.get(id))
                            .cloned()
                    } else {
                        attr(n, "src")
                            .or_else(|| attr(n, "href"))
                            .and_then(|v| resolve(self.base, v).ok())
                    };
                st.image = target;
                st.text = attr(n, "alt").unwrap_or("").into();
                st.image_geometry = self.image_geometry(n);
                push_run(out, st);
                return Ok(());
            }
            "footnoteReference" | "endnoteReference" => {
                if let Some(id) = attr(n, "id") {
                    st.text = id.into();
                    st.style.script = Some("sup".into());
                    push_run(out, st)
                }
                return Ok(());
            }
            _ => {}
        }
        for c in n.children() {
            self.inline(c, &st, out)?
        }
        Ok(())
    }
    fn image_geometry(&self, n: N) -> Option<ImageGeometry> {
        let mut geometry = ImageGeometry::default();
        match self.kind {
            "docx" | "pptx" => {
                let picture = n.ancestors().find(|p| tag(*p) == "pic")?;
                let transform = child(picture, "spPr").and_then(|s| child(s, "xfrm"));
                let frame = picture
                    .ancestors()
                    .find(|p| matches!(tag(*p), "inline" | "anchor"));
                let extent = frame
                    .and_then(|p| child(p, "extent"))
                    .or_else(|| transform.and_then(|p| child(p, "ext")));
                geometry.width = formatting::units(extent.and_then(|p| attr(p, "cx")), 12700.);
                geometry.height = formatting::units(extent.and_then(|p| attr(p, "cy")), 12700.);
                if let Some(xfrm) = transform {
                    geometry.rotation = formatting::units(attr(xfrm, "rot"), 60000.)
                        .filter(|v| v.is_finite())
                        .unwrap_or_default()
                        .rem_euclid(360.);
                    geometry.flip_x = matches!(attr(xfrm, "flipH"), Some("1" | "true"));
                    geometry.flip_y = matches!(attr(xfrm, "flipV"), Some("1" | "true"));
                }
                if let Some(rect) = n.parent().and_then(|p| child(p, "srcRect")) {
                    geometry.crop = ["l", "t", "r", "b"].map(|k| {
                        formatting::units(attr(rect, k), 100000.)
                            .filter(|v| (-1. ..1.).contains(v))
                            .unwrap_or_default()
                    });
                }
            }
            "hwpx" => {
                let picture = n.ancestors().find(|p| tag(*p) == "pic")?;
                let size = child(picture, "curSz")
                    .filter(|p| {
                        ["width", "height"]
                            .iter()
                            .all(|k| formatting::units(attr(*p, k), 100.).is_some_and(|v| v > 0.))
                    })
                    .or_else(|| {
                        child(picture, "sz").filter(|p| {
                            matches!(attr(*p, "widthRelTo"), Some("ABSOLUTE") | None)
                                && matches!(attr(*p, "heightRelTo"), Some("ABSOLUTE") | None)
                        })
                    })
                    .or_else(|| child(picture, "orgSz"))?;
                geometry.width = formatting::units(attr(size, "width"), 100.);
                geometry.height = formatting::units(attr(size, "height"), 100.);
                geometry.rotation = child(picture, "rotationInfo")
                    .and_then(|p| formatting::units(attr(p, "angle"), 1.))
                    .filter(|v| v.is_finite())
                    .unwrap_or_default()
                    .rem_euclid(360.);
                if let Some(flip) = child(picture, "flip") {
                    geometry.flip_x = matches!(attr(flip, "horizontal"), Some("1" | "true"));
                    geometry.flip_y = matches!(attr(flip, "vertical"), Some("1" | "true"));
                }
            }
            "odt" | "odp" | "ods" => {
                let frame = n.ancestors().find(|p| tag(*p) == "frame")?;
                geometry.width = attr(frame, "width").and_then(formatting::length_in_points);
                geometry.height = attr(frame, "height").and_then(formatting::length_in_points);
            }
            "epub" => {
                let dimension = |key| {
                    self.css(n)
                        .split(';')
                        .filter_map(|d| d.split_once(':'))
                        .filter(|(k, _)| k.trim() == key)
                        .filter_map(|(_, v)| formatting::length_in_points(v))
                        .next_back()
                        .or_else(|| formatting::units(attr(n, key), 4. / 3.))
                };
                geometry.width = dimension("width");
                geometry.height = dimension("height");
            }
            _ => return None,
        }
        let valid = |v: &f64| *v > 0. && *v <= 10000.;
        geometry.width = geometry.width.filter(valid);
        geometry.height = geometry.height.filter(valid);
        (geometry.width.is_some() || geometry.height.is_some()).then_some(geometry)
    }

    pub fn runs(&self, n: N) -> Result<Vec<Run>> {
        let mut out = Vec::new();
        self.inline(n, &Run::default(), &mut out)?;
        Ok(out)
    }
    pub fn paragraph(&self, n: N) -> Result<Block> {
        let ppr = child(n, "pPr");
        let style_id = ppr
            .and_then(|p| child(p, "pStyle"))
            .and_then(|s| attr(s, "val"))
            .or_else(|| attr(n, "style-name"))
            .unwrap_or("");
        let mut runs = Vec::new();
        let state = Run {
            style: self.rules.styles.get(style_id).cloned().unwrap_or_default(),
            ..Run::default()
        };
        self.inline(n, &state, &mut runs)?;
        let name = tag(n);
        let heading = if name == "h" {
            attr(n, "outline-level").unwrap_or("1").parse().unwrap_or(1)
        } else if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            name[1..].parse()?
        } else {
            ppr.and_then(|p| child(p, "outlineLvl"))
                .and_then(|s| attr(s, "val"))
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|x| *x < 6)
                .map(|x| x + 1)
                .or_else(|| self.rules.headings.get(style_id).copied())
                .or_else(|| {
                    style_id
                        .to_ascii_lowercase()
                        .strip_prefix("heading")
                        .and_then(|v| v.trim().parse().ok())
                })
                .unwrap_or(0)
        };
        let num = ppr.and_then(|p| child(p, "numPr"));
        let lists: Vec<_> = n
            .ancestors()
            .filter(|v| matches!(tag(*v), "ol" | "ul" | "list"))
            .collect();
        let list = if let Some(l) = lists.first() {
            Some(List {
                depth: lists.len() - 1,
                ordered: tag(*l) == "ol"
                    || attr(*l, "style-name")
                        .and_then(|s| self.rules.numbers.get(s))
                        .copied()
                        .unwrap_or(false),
            })
        } else if let Some(p) = num {
            Some(List {
                ordered: child(p, "numId")
                    .and_then(|n| attr(n, "val"))
                    .and_then(|v| self.rules.numbers.get(v))
                    .copied()
                    .unwrap_or(false),
                depth: child(p, "ilvl")
                    .and_then(|n| attr(n, "val"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            })
        } else if ppr
            .is_some_and(|p| child(p, "buChar").is_some() || child(p, "buAutoNum").is_some())
        {
            Some(List {
                ordered: ppr.and_then(|p| child(p, "buAutoNum")).is_some(),
                depth: ppr
                    .and_then(|p| attr(p, "lvl"))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            })
        } else {
            None
        };
        let mut props = self
            .rules
            .paragraph_styles
            .get(attr(n, "paraPrIDRef").unwrap_or(style_id))
            .cloned()
            .unwrap_or_default();
        if self.kind == "docx"
            && let Some(p) = ppr
        {
            formatting::word_paragraph(&mut props, p);
        }
        if attr(n, "pageBreak") == Some("1")
            || ppr
                .and_then(|p| child(p, "pageBreakBefore"))
                .is_some_and(|b| !matches!(attr(b, "val"), Some("false" | "0")))
        {
            props.insert("break-before".into(), "page".into());
        }
        if let Some(v) = ppr
            .and_then(|p| child(p, "jc"))
            .and_then(|n| attr(n, "val"))
            .or_else(|| ppr.and_then(|n| attr(n, "algn")))
        {
            props.insert("align".into(), v.into());
        }
        if n.ancestors().any(|n| attr(n, "dir") == Some("rtl"))
            || ppr.and_then(|p| child(p, "bidi")).is_some()
        {
            props.insert("dir".into(), "rtl".into());
        }
        if self.kind == "epub" {
            for a in n.ancestors().filter(|a| a.is_element()) {
                let css = self.css(a);
                for d in css.split(';') {
                    if let Some((k, v)) = d.split_once(':')
                        && matches!(k.trim(), "writing-mode" | "direction" | "text-align")
                    {
                        props
                            .entry(
                                match k.trim() {
                                    "direction" => "dir",
                                    "text-align" => "align",
                                    x => x,
                                }
                                .into(),
                            )
                            .or_insert_with(|| v.trim().into());
                    }
                }
            }
        }
        for a in n.attributes() {
            if matches!(a.name(), "id" | "styleIDRef" | "paraPrIDRef") {
                props.insert(a.name().into(), a.value().into());
            }
        }
        if matches!(self.kind, "pptx" | "odp") {
            for parent in n.ancestors() {
                if matches!(tag(parent), "sp" | "frame" | "graphicFrame") {
                    for a in parent.attributes().chain(
                        child(parent, "spPr")
                            .and_then(|v| child(v, "xfrm"))
                            .and_then(|v| child(v, "off"))
                            .into_iter()
                            .flat_map(|v| v.attributes()),
                    ) {
                        if matches!(a.name(), "x" | "y" | "width" | "height") {
                            props.insert(a.name().into(), a.value().into());
                        }
                    }
                    break;
                }
            }
        }
        Ok(Block::Paragraph {
            heading: heading.min(6),
            list,
            runs,
            properties: props,
        })
    }
    pub fn cell(&self, n: N) -> Result<Cell> {
        let mut c = Cell::default();
        let ps: Vec<_> = n
            .descendants()
            .filter(|v| {
                tag(*v) == "p"
                    && !v
                        .ancestors()
                        .skip(1)
                        .take_while(|a| *a != n)
                        .any(|a| tag(a) == "p")
            })
            .collect();
        if n.descendants()
            .skip(1)
            .any(|c| matches!(tag(c), "table" | "tbl"))
        {
            for node in n.children().filter(|c| c.is_element()) {
                self.blocks(node, &mut c.blocks)?
            }
        } else if ps.is_empty() {
            c.runs = self.runs(n)?
        } else {
            let mut common_align = None;
            let mut aligned = true;
            let mut first = true;
            for (i, p) in ps.into_iter().enumerate() {
                let Block::Paragraph {
                    runs, properties, ..
                } = self.paragraph(p)?
                else {
                    unreachable!()
                };
                let discard_spacers = matches!(self.kind, "hwpx" | "docx");
                if discard_spacers && !has_content(&runs) {
                    continue;
                }
                let align = properties
                    .get("align")
                    .and_then(|v| formatting::alignment(v));
                if first {
                    common_align = align;
                    first = false;
                } else {
                    aligned &= align == common_align;
                }
                if if discard_spacers {
                    !c.runs.is_empty()
                } else {
                    i > 0
                } {
                    push_run(&mut c.runs, Run::text("\n"))
                }
                for r in runs {
                    push_run(&mut c.runs, r)
                }
            }
            if aligned && let Some(align) = common_align {
                c.properties.insert("text-align".into(), align.into());
            }
        }
        let props = child(n, "tcPr");
        match self.kind {
            "hwpx" => {
                if let Some(fill) =
                    attr(n, "borderFillIDRef").and_then(|id| self.rules.hwpx_borders.get(id))
                {
                    c.properties.extend(fill.clone());
                }
                if let Some(size) = child(n, "cellSz") {
                    for key in ["width", "height"] {
                        formatting::points(
                            &mut c.properties,
                            key,
                            formatting::units(attr(size, key), 100.).filter(|n| *n > 0.),
                        );
                    }
                }
                if let Some(v) = child(n, "subList").and_then(|list| attr(list, "vertAlign")) {
                    formatting::box_property(&mut c.properties, "vertical-align", v);
                }
            }
            "docx" => {
                if let Some(p) = props {
                    formatting::word_cell(&mut c.properties, p);
                }
            }
            "pptx" => {
                if let Some(p) = props {
                    formatting::drawing_cell(&mut c.properties, p);
                }
            }
            "odt" | "ods" | "odp" => {
                if let Some(style) =
                    attr(n, "style-name").and_then(|id| self.rules.cell_styles.get(id))
                {
                    c.properties.extend(style.clone());
                }
            }
            "epub" => {
                for (key, value) in self.css(n).split(';').filter_map(|d| d.split_once(':')) {
                    formatting::box_property(&mut c.properties, key.trim(), value.trim());
                }
            }
            _ => {}
        }
        let span = child(n, "cellSpan");
        c.colspan = attr(n, "number-columns-spanned")
            .or_else(|| attr(n, "colspan"))
            .or_else(|| attr(n, "gridSpan"))
            .or_else(|| {
                props
                    .and_then(|p| child(p, "gridSpan"))
                    .and_then(|p| attr(p, "val"))
            })
            .or_else(|| span.and_then(|p| attr(p, "colSpan")))
            .and_then(|s| s.parse().ok())
            .filter(|v| *v > 1);
        c.rowspan = attr(n, "number-rows-spanned")
            .or_else(|| attr(n, "rowspan"))
            .or_else(|| attr(n, "rowSpan"))
            .or_else(|| span.and_then(|p| attr(p, "rowSpan")))
            .and_then(|s| s.parse().ok())
            .filter(|v| *v > 1);
        for a in n.attributes() {
            if matches!(
                a.name(),
                "formula"
                    | "value"
                    | "date-value"
                    | "time-value"
                    | "value-type"
                    | "currency"
                    | "boolean-value"
                    | "style-name"
            ) {
                c.properties.insert(a.name().into(), a.value().into());
            }
        }
        if c.runs.is_empty()
            && let Some(s) = [
                "string-value",
                "date-value",
                "time-value",
                "boolean-value",
                "value",
            ]
            .iter()
            .find_map(|key| attr(n, key))
        {
            c.runs.push(Run::text(s))
        }
        Ok(c)
    }
    pub fn table(&self, n: N) -> Result<Block> {
        self.table_with_hidden_columns(n, &SectionRange::default())
    }

    pub fn table_with_hidden_columns(&self, n: N, hidden: &SectionRange) -> Result<Block> {
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let mut merges: HashMap<usize, (usize, usize)> = HashMap::new();
        let mut cells = 0usize;
        let mut weight = 0usize;
        let word = (self.kind == "docx").then(|| word_table_format(n, self.rules));
        let grid_unit = match self.kind {
            "docx" => 20.,
            "pptx" => 12700.,
            _ => 0.,
        };
        let grid: Vec<_> = child(n, "tblGrid")
            .into_iter()
            .flat_map(|grid| grid.children())
            .filter(|n| tag(*n) == "gridCol")
            .take(16384)
            .map(|column| {
                formatting::units(attr(column, "w"), grid_unit).filter(|w| *w > 0. && *w <= 10000.)
            })
            .collect();
        let mut positions = Vec::new();
        let mut columns = 0;
        for row in n.descendants().skip(1).filter(|v| {
            matches!(tag(*v), "tr" | "table-row")
                && v.ancestors()
                    .skip(1)
                    .find(|a| matches!(tag(*a), "table" | "tbl"))
                    == Some(n)
        }) {
            let count = attr(row, "number-rows-repeated")
                .unwrap_or("1")
                .parse::<usize>()?;
            if count == 0 || count > CELL_LIMIT {
                return Err("invalid table row repeat count".into());
            }
            let mut line = Vec::new();
            let mut bytes = 0usize;
            let mut column = 0usize;
            let mut starts = Vec::new();
            let row_height = match self.kind {
                "docx" => child(row, "trPr")
                    .and_then(|p| child(p, "trHeight"))
                    .filter(|n| attr(*n, "hRule") != Some("auto"))
                    .and_then(|n| formatting::units(attr(n, "val"), 20.)),
                "pptx" => formatting::units(attr(row, "h"), 12700.),
                _ => None,
            }
            .filter(|height| *height > 0.);
            let physical_grid = self.kind == "pptx"
                && row.children().filter(|n| tag(*n) == "tc").count() == grid.len();
            for node in row.children().filter(|v| {
                matches!(
                    tag(*v),
                    "td" | "th" | "tc" | "table-cell" | "covered-table-cell"
                )
            }) {
                let repeat = attr(node, "number-columns-repeated")
                    .unwrap_or("1")
                    .parse::<usize>()?;
                if tag(node) == "covered-table-cell" {
                    if self.kind == "ods" {
                        column = column.checked_add(repeat).ok_or("invalid table column")?;
                    }
                    continue;
                }
                let mut c = self.cell(node)?;
                if grid_unit > 0. {
                    let span = c.colspan.unwrap_or(1).min(16384);
                    if !c.properties.contains_key("width")
                        && let Some(widths) = grid.get(column..column + span)
                        && let Some(width) = widths.iter().copied().sum::<Option<f64>>()
                    {
                        formatting::points(&mut c.properties, "width", Some(width));
                    }
                    if c.rowspan.is_none() {
                        formatting::points(&mut c.properties, "height", row_height);
                    }
                }
                if self.kind == "hwpx"
                    && let Some(margins) = child(node, "cellMargin")
                        .filter(|_| attr(node, "hasMargin") != Some("0"))
                        .or_else(|| child(n, "inMargin"))
                {
                    for (side, key) in formatting::SIDES.into_iter().zip([
                        "padding-left",
                        "padding-right",
                        "padding-top",
                        "padding-bottom",
                    ]) {
                        formatting::points(
                            &mut c.properties,
                            key,
                            formatting::units(attr(margins, side), 100.).filter(|n| *n >= 0.),
                        );
                    }
                }
                if let Some(v) = attr(row, "visibility") {
                    c.properties.insert("row_visibility".into(), v.into());
                }
                if self.kind == "ods" && crate::sheets::ods_axis_hidden(row, n) {
                    crate::sheets::hide_cell_axis(&mut c, "hidden_row");
                }
                if word.is_some() {
                    let col = column;
                    column += c.colspan.unwrap_or(1).min(16384);
                    match child(node, "tcPr").and_then(|p| child(p, "vMerge")) {
                        Some(v) if attr(v, "val") == Some("restart") => {
                            merges.insert(col, (rows.len(), line.len()));
                        }
                        Some(_) if merges.contains_key(&col) => {
                            let (r, j) = merges[&col];
                            if let Some(origin) = rows.get_mut(r).and_then(|row| row.get_mut(j)) {
                                origin.rowspan = Some(origin.rowspan.unwrap_or(1) + 1);
                                if let Some(bottom) = c.properties.remove("border-bottom") {
                                    origin.properties.insert("border-bottom".into(), bottom);
                                }
                                continue;
                            }
                        }
                        _ => {
                            merges.remove(&col);
                        }
                    }
                }
                if self.kind == "pptx"
                    && ["hMerge", "vMerge"]
                        .iter()
                        .any(|key| matches!(attr(node, key), Some("1" | "true")))
                {
                    if physical_grid {
                        column += 1;
                    }
                    continue;
                }
                if self.kind == "pptx" {
                    column += if physical_grid {
                        1
                    } else {
                        c.colspan.unwrap_or(1).min(16384)
                    };
                }
                let cost = if repeat > 1 || count > 1 {
                    serde_json::to_vec(&c)?.len() + 256 * (1 + c.runs.len())
                } else {
                    256 + c.runs.iter().map(|r| r.text.len() + 256).sum::<usize>()
                };
                bytes = bytes.saturating_add(repeat.saturating_mul(cost));
                if bytes.saturating_mul(count) > PART_LIMIT
                    || line.len().saturating_add(repeat) > CELL_LIMIT
                {
                    return Err("table column expansion limit exceeded".into());
                }
                if word.is_some() {
                    starts.extend(std::iter::repeat_n(
                        column - c.colspan.unwrap_or(1).min(16384),
                        repeat,
                    ));
                }
                let first = line.len();
                line.extend(std::iter::repeat_n(c, repeat));
                if self.kind == "ods" && repeat != 0 {
                    let end = column.checked_add(repeat).ok_or("invalid table column")?;
                    for (cell, col) in line[first..].iter_mut().zip(column + 1..=end) {
                        if hidden.contains(col) {
                            crate::sheets::hide_cell_axis(cell, "hidden_column");
                        }
                    }
                    column = end;
                }
            }
            cells = cells.saturating_add(line.len().max(1).saturating_mul(count));
            weight = weight.saturating_add(bytes.saturating_mul(count));
            if cells > CELL_LIMIT
                || weight > PART_LIMIT
                || self.rules.table_bytes.get().saturating_add(weight) > EXPANSION_LIMIT
            {
                return Err("table row expansion limit exceeded".into());
            }
            columns = columns.max(column);
            if word.is_some() {
                positions.extend(std::iter::repeat_n(starts, count));
            }
            rows.extend(std::iter::repeat_n(line, count));
        }
        if let Some((edges, defaults)) = word {
            let height = rows.len();
            for (y, (row, starts)) in rows.iter_mut().zip(positions).enumerate() {
                for (cell, x) in row.iter_mut().zip(starts) {
                    for (key, value) in &defaults {
                        cell.properties
                            .entry(key.clone())
                            .or_insert_with(|| value.clone());
                    }
                    let outer = [
                        x == 0,
                        x + cell.colspan.unwrap_or(1).min(16384) >= columns,
                        y == 0,
                        y + cell.rowspan.unwrap_or(1) >= height,
                    ];
                    for ((key, is_outer), inner) in formatting::BORDERS
                        .into_iter()
                        .zip(outer)
                        .zip(["insideV", "insideV", "insideH", "insideH"])
                    {
                        if let Some(value) = edges.get(if is_outer { key } else { inner }) {
                            cell.properties
                                .entry(key.into())
                                .or_insert_with(|| value.clone());
                        }
                    }
                }
            }
        }
        self.rules
            .table_bytes
            .set(self.rules.table_bytes.get() + weight);
        Ok(Block::Table { rows })
    }
    pub fn blocks(&self, n: N, out: &mut Vec<Block>) -> Result<()> {
        let name = tag(n);
        if matches!(
            name,
            "script"
                | "style"
                | "head"
                | "del"
                | "event-listeners"
                | "tracked-changes"
                | "binary-data"
                | "settings"
                | "automatic-styles"
        ) {
            return Ok(());
        }
        if matches!(name, "table" | "tbl") {
            let table = self.table(n)?;
            if self.kind != "docx" || !word_spacer_table(n, &table) {
                out.push(table);
            }
            return Ok(());
        }
        if matches!(
            name,
            "p" | "h" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre"
        ) {
            if self.kind == "hwpx" && desc(n, "tbl").next().is_some() {
                let mut runs = Vec::new();
                self.hwpx_mixed(n, &Run::default(), &mut runs, out)?;
                if has_content(&runs) {
                    out.push(Block::flow(runs));
                }
            } else {
                let block = self.paragraph(n)?;
                if !matches!(self.kind, "hwpx" | "docx")
                    || matches!(&block, Block::Paragraph { runs, .. } if has_content(runs))
                {
                    out.push(block);
                }
            }
            return Ok(());
        }
        if name == "li" {
            let mut inline = Vec::new();
            for c in n.children() {
                if matches!(tag(c), "ol" | "ul" | "p") {
                    if !inline.is_empty() {
                        self.list_inline(n, &mut inline, out)?
                    }
                    self.blocks(c, out)?
                } else {
                    self.inline(c, &Run::default(), &mut inline)?
                }
            }
            if !inline.is_empty() {
                self.list_inline(n, &mut inline, out)?
            }
            return Ok(());
        }
        if matches!(
            name,
            "img" | "image" | "blip" | "math" | "oMath" | "equation" | "object"
        ) {
            out.push(Block::flow(self.runs(n)?));
            return Ok(());
        }
        if self.kind == "epub" && n.is_text() {
            if let Some(t) = n.text().filter(|v| !v.trim().is_empty()) {
                out.push(Block::text(t))
            }
            return Ok(());
        }
        if self.kind == "epub"
            && matches!(name, "div" | "section" | "aside" | "blockquote")
            && !n.children().any(|c| {
                matches!(
                    tag(c),
                    "div" | "section" | "p" | "h1" | "h2" | "h3" | "ol" | "ul" | "table"
                )
            })
        {
            out.push(self.paragraph(n)?);
            return Ok(());
        }
        for c in n.children() {
            self.blocks(c, out)?
        }
        Ok(())
    }
    fn list_inline(&self, n: N, runs: &mut Vec<Run>, out: &mut Vec<Block>) -> Result<()> {
        let lists: Vec<_> = n
            .ancestors()
            .filter(|a| matches!(tag(*a), "ol" | "ul"))
            .collect();
        out.push(Block::Paragraph {
            heading: 0,
            list: Some(List {
                ordered: lists.first().is_some_and(|a| tag(*a) == "ol"),
                depth: lists.len().saturating_sub(1),
            }),
            runs: std::mem::take(runs),
            properties: Properties::new(),
        });
        Ok(())
    }
    fn hwpx_mixed(
        &self,
        n: N,
        state: &Run,
        runs: &mut Vec<Run>,
        out: &mut Vec<Block>,
    ) -> Result<()> {
        if tag(n) == "tbl" {
            let preceding = std::mem::take(runs);
            if has_content(&preceding) {
                out.push(Block::flow(preceding));
            }
            out.push(self.table(n)?)
        } else if desc(n, "tbl").next().is_none() {
            self.inline(n, state, runs)?;
        } else {
            let mut state = state.clone();
            if let Some(style) = attr(n, "charPrIDRef").and_then(|id| self.rules.styles.get(id)) {
                apply_style(&mut state.style, style);
            }
            for c in n.children() {
                self.hwpx_mixed(c, &state, runs, out)?
            }
        }
        Ok(())
    }
}

fn word_table_format(n: N, rules: &Rules) -> (Properties, Properties) {
    let props = child(n, "tblPr");
    let style = props
        .and_then(|p| child(p, "tblStyle"))
        .and_then(|s| attr(s, "val"));
    let mut edges = style
        .and_then(|id| rules.table_borders.get(id))
        .cloned()
        .unwrap_or_default();
    let mut defaults = style
        .and_then(|id| rules.table_cells.get(id))
        .cloned()
        .unwrap_or_default();
    if let Some(props) = props {
        if let Some(node) = child(props, "tblBorders") {
            edges.extend(formatting::word_borders(node));
        }
        formatting::word_cell(&mut defaults, props);
    }
    (edges, defaults)
}

fn has_content(runs: &[Run]) -> bool {
    runs.iter().any(|r| {
        !r.text.trim().is_empty() || r.image.is_some() || r.mathml.is_some() || r.page_break
    })
}

fn word_spacer_table(n: N, table: &Block) -> bool {
    let Block::Table { rows } = table else {
        return false;
    };
    let [row] = rows.as_slice() else {
        return false;
    };
    let [cell] = row.as_slice() else {
        return false;
    };
    if has_content(&cell.runs) || !cell.blocks.is_empty() {
        return false;
    }
    !n.descendants().any(|item| match tag(item) {
        "tblStyle" => true,
        "tblBorders" | "tcBorders" => item.children().filter(|c| c.is_element()).any(|border| {
            attr(border, "val").is_some_and(|style| !matches!(style, "none" | "nil"))
        }),
        "shd" => {
            attr(item, "fill").is_some_and(|fill| fill != "auto")
                || attr(item, "themeFill").is_some()
        }
        _ => false,
    })
}
