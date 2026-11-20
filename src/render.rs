use crate::{formatting, model::*};
use std::fmt::Write;
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn literal_angle_open(tail: &str) -> bool {
    let mut chars = tail.chars();
    match chars.next() {
        Some(first) if first.is_whitespace() => true,
        // These labels cannot start a Markdown HTML tag or autolink.
        Some(first) if !first.is_ascii() && first.is_alphabetic() => {
            chars.find(|c| !(c.is_alphanumeric() || matches!(c, ' ' | '-' | '·'))) == Some('>')
        }
        _ => false,
    }
}

fn markdown(s: &str) -> String {
    let mut out = String::new();
    for line in s.split_inclusive('\n') {
        let mut margin = true;
        for (i, c) in line.char_indices() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' if !literal_angle_open(&line[i + 1..]) => out.push_str("\\<"),
                '>' if margin => out.push_str("\\>"),
                '\\' | '*' | '_' | '[' | ']' | '|' | '~' | '\u{60}' => {
                    out.push('\\');
                    out.push(c)
                }
                '#' | '+' | '-' if margin => {
                    out.push('\\');
                    out.push(c)
                }
                _ => out.push(c),
            }
            if !c.is_whitespace() {
                margin = false
            }
        }
    }
    out
}
fn safe_link(link: &str) -> bool {
    !link.contains(['\\', '\r', '\n', '\0'])
        && !link.chars().any(char::is_control)
        && link.trim() == link
        && (link.starts_with('#')
            || link.starts_with("https://")
            || link.starts_with("http://")
            || link.starts_with("mailto:")
            || (!link.contains(':') && !link.starts_with('/')))
}
fn uri(s: &str) -> String {
    s.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('"', "%22")
}
fn runs(items: &[Run], html: bool, links: &Properties) -> String {
    let mut out = String::new();
    for r in items {
        let mut t = if html {
            escape(&r.text).replace('\n', "<br>")
        } else {
            markdown(&r.text).replace('\n', "  \n")
        };
        if let Some(math) = &r.mathml {
            t = math.clone()
        }
        if let Some(img) = &r.image
            && let Some(link) = links.get(img)
        {
            t = if let Some(layout) = &r.image_layout {
                format!(
                    "<img alt=\"{}\" src=\"{}\" style=\"width:{}pt;height:{}pt;transform:rotate({}deg)\">",
                    escape(&r.text),
                    escape(&uri(link)),
                    f64::from(layout.width) / 100.,
                    f64::from(layout.height) / 100.,
                    layout.rotation_deg
                )
            } else if html {
                format!(
                    "<img alt=\"{}\" src=\"{}\">",
                    escape(&r.text),
                    escape(&uri(link))
                )
            } else {
                format!("![{}](<{}>)", t, uri(link))
            };
        }
        for (enabled, tag, md) in [
            (r.style.bold, "strong", "**"),
            (r.style.italic, "em", "*"),
            (r.style.strike, "del", "~~"),
            (r.style.underline, "u", ""),
        ] {
            if enabled && !t.trim().is_empty() {
                if html || md.is_empty() {
                    t = format!("<{tag}>{t}</{tag}>")
                } else {
                    let inner = t.trim();
                    let begin = t.len() - t.trim_start().len();
                    let end = begin + inner.len();
                    t = format!("{}{md}{inner}{md}{}", &t[..begin], &t[end..]);
                }
            }
        }
        if let Some(s) = &r.style.script
            && matches!(s.as_str(), "sub" | "sup" | "rt")
        {
            t = format!("<{s}>{t}</{s}>")
        }
        if let Some(l) = &r.link
            && safe_link(l)
        {
            t = if html {
                format!("<a href=\"{}\">{t}</a>", escape(&uri(l)))
            } else {
                format!("[{t}](<{}>)", escape(&uri(l)))
            }
        }
        if html {
            let mut css = String::new();
            if let Some(font) = &r.style.font
                && font.len() < 128
                && font
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
            {
                let _ = write!(css, "font-family:'{font}';");
            }
            if let Some(size) = &r.style.size
                && ["pt", "px", "em", "%"].iter().any(|u| {
                    size.strip_suffix(u)
                        .is_some_and(|s| s.parse::<f32>().is_ok_and(|n| n > 0. && n < 1000.))
                })
            {
                let _ = write!(css, "font-size:{size};");
            }
            if let Some(color) = r.style.color.as_deref().and_then(formatting::color) {
                let _ = write!(css, "color:{color};");
            }
            if !css.is_empty() {
                t = format!("<span style=\"{css}\">{t}</span>")
            }
        }
        out += &t;
    }
    out
}
fn cell_attributes(out: &mut String, props: &Properties) -> Result<()> {
    let mut css = Properties::new();
    for (key, value) in props {
        if key != "border" {
            formatting::box_property(&mut css, key, value);
        }
    }
    if !css.is_empty() {
        out.push_str(" style=\"");
        for (key, value) in css {
            write!(out, "{key}:{value};")?;
        }
        out.push('"');
    }
    Ok(())
}

fn paragraph_css(props: &Properties, preserve_space: bool, left: Option<f32>) -> Result<String> {
    let mut css = String::new();
    if preserve_space {
        css.push_str("white-space:pre-wrap;");
    }
    if let Some(left) = left {
        let indent = props
            .get("left_indent_twips")
            .and_then(|v| v.parse::<f32>().ok())
            .map(|v| v / 20.)
            .or_else(|| props.get("x_pt")?.parse::<f32>().ok().map(|v| v - left));
        if let Some(x) = indent.filter(|x| x.is_finite() && x.abs() < 10000.) {
            write!(css, "margin-left:{x}pt;")?;
        }
    }
    for key in [
        "margin-left",
        "margin-right",
        "margin-top",
        "margin-bottom",
        "text-indent",
        "line-height",
    ] {
        if let Some(value) = props
            .get(key)
            .and_then(|v| formatting::length(v, key != "line-height"))
        {
            write!(css, "{key}:{value};")?;
        }
    }
    if let Some(align) = props.get("align").and_then(|v| formatting::alignment(v)) {
        write!(css, "text-align:{align};")?;
    }
    if let Some(writing_mode) = props.get("writing-mode")
        && matches!(writing_mode.as_str(), "vertical-rl" | "vertical-lr")
    {
        write!(css, "writing-mode:{writing_mode};")?;
    }
    Ok(css)
}

fn direction(props: &Properties) -> &'static str {
    if props.get("dir").is_some_and(|v| v == "rtl") {
        " dir=\"rtl\""
    } else {
        ""
    }
}

fn html_table(
    out: &mut String,
    rows: &[Vec<Cell>],
    links: &Properties,
    styled: bool,
) -> Result<()> {
    let source_borders = styled
        && rows.iter().flatten().any(|c| {
            formatting::BORDERS
                .iter()
                .any(|key| c.properties.contains_key(*key))
        });
    out.push_str(if source_borders {
        "<table class=\"source-borders\">\n"
    } else {
        "<table>\n"
    });
    for row in rows {
        out.push_str("<tr>");
        for c in row {
            out.push_str("<td");
            if let Some(n) = c.colspan {
                write!(out, " colspan=\"{n}\"")?
            }
            if let Some(n) = c.rowspan {
                write!(out, " rowspan=\"{n}\"")?
            }
            if styled {
                cell_attributes(out, &c.properties)?;
            }
            out.push('>');
            out.push_str(&runs(&c.runs, true, links));
            for b in &c.blocks {
                match b {
                    Block::Paragraph {
                        heading,
                        runs: items,
                        properties,
                        ..
                    } if styled => {
                        let elem = if *heading == 0 {
                            "p".into()
                        } else {
                            format!("h{}", (*heading).min(6))
                        };
                        let css = paragraph_css(properties, false, None)?;
                        writeln!(
                            out,
                            "<{elem}{} style=\"{css}\">{}</{elem}>",
                            direction(properties),
                            runs(items, true, links)
                        )?;
                    }
                    Block::Paragraph { runs: items, .. } => {
                        writeln!(out, "<p>{}</p>", runs(items, true, links))?
                    }
                    Block::Table { rows } => html_table(out, rows, links, styled)?,
                }
            }
            out.push_str("</td>");
        }
        out.push_str("</tr>\n")
    }
    out.push_str("</table>\n");
    Ok(())
}
pub fn render(doc: &Document, format: &str, mode: &str, links: &Properties) -> Result<String> {
    if !matches!(mode, "semantic" | "layout" | "hybrid") {
        return Err("mode must be semantic, layout or hybrid".into());
    }
    if format == "json" {
        return Ok(serde_json::to_string_pretty(doc)? + "\n");
    }
    let html = format == "html";
    if !html && format != "md" {
        return Err("format must be md, html or json".into());
    }
    let mut out = String::new();
    if html {
        out.push_str(concat!(
            "<!doctype html>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<style>\n",
            "article { box-sizing: border-box; max-width: 76rem; margin-inline: auto; padding: 1.25rem; font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; font-size: 18px; line-height: 1.65; overflow-wrap: break-word; print-color-adjust: exact; -webkit-print-color-adjust: exact; }\n",
            "article span[style*=\"font-size\"] { zoom: 1.2; }\n",
            "article table { border-collapse: collapse; max-width: 100%; margin-block: 0.5em; font: inherit; }\n",
            "article td, article th { border: 1px solid #a0a0a0; padding: 0.3em 0.5em; vertical-align: top; white-space: pre-wrap; }\n",
            "article .source-borders > tbody > tr > :is(td, th) { border: none; }\n",
            "article td > :first-child { margin-top: 0; }\n",
            "article td > :last-child { margin-bottom: 0; }\n",
            "article img { max-width: 100%; }\n</style>\n<article>\n",
        ));
    }
    let mut next_slide_number = 0;
    for (i, s) in doc.sections.iter().enumerate() {
        let slide_number = if s.kind == "slide" {
            next_slide_number += 1;
            Some(s.slide_number.unwrap_or(next_slide_number))
        } else {
            None
        };
        if i > 0 && !out.is_empty() {
            if html {
                out.push_str("\n<hr>\n")
            } else {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                if !out.ends_with("\n\n") {
                    out.push('\n');
                }
                out.push_str("---\n\n");
            }
        }
        if let Some(anchor) = s.properties.get("anchor") {
            writeln!(out, "<a id=\"{}\"></a>\n", escape(anchor))?;
        }
        if let Some(number) = slide_number {
            if html {
                writeln!(
                    out,
                    "<p class=\"slide-number\"><strong>Slide {number}</strong></p>"
                )?;
            } else {
                writeln!(out, "**Slide {number}**\n")?;
            }
        }
        if !s.title.is_empty() {
            if html {
                writeln!(out, "<h1>{}</h1>", escape(&s.title))?
            } else {
                writeln!(out, "# {}\n", markdown(&s.title))?
            }
        }
        let left = s
            .blocks
            .iter()
            .filter_map(|b| {
                if let Block::Paragraph { properties, .. } = b {
                    properties.get("x_pt")?.parse::<f32>().ok()
                } else {
                    None
                }
            })
            .min_by(f32::total_cmp)
            .unwrap_or(0.);
        let mut html_list = None;
        let mut in_list = false;
        for block in &s.blocks {
            if matches!(doc.format.as_str(), "pdf" | "docx" | "hwpx")
                && let Block::Paragraph {
                    runs, properties, ..
                } = block
                && !properties.contains_key("anchor")
                && runs.iter().all(|r| {
                    r.text.trim().is_empty()
                        && r.mathml.is_none()
                        && r.image
                            .as_ref()
                            .is_none_or(|image| !links.contains_key(image))
                })
            {
                continue;
            }
            let list_type = if html
                && let Block::Paragraph {
                    list: Some(list), ..
                } = block
            {
                Some(if list.ordered { "ol" } else { "ul" })
            } else {
                None
            };
            if list_type != html_list {
                if let Some(old) = html_list {
                    writeln!(out, "</{old}>")?
                }
                if let Some(new) = list_type {
                    writeln!(out, "<{new}>")?
                }
                html_list = list_type;
            }
            match block {
                Block::Paragraph {
                    heading,
                    list,
                    runs: items,
                    properties,
                } => {
                    if let Some(anchor) = properties.get("anchor") {
                        writeln!(out, "<a id=\"{}\"></a>", escape(anchor))?
                    }
                    let use_html = html
                        || (mode == "layout" && list.is_none() && *heading == 0)
                        || (mode == "hybrid"
                            && (properties.contains_key("dir")
                                || properties.contains_key("writing-mode")));
                    let val = runs(items, use_html, links);
                    if use_html {
                        let elem = if *heading > 0 {
                            format!("h{}", heading.clamp(&1, &6))
                        } else {
                            "p".into()
                        };
                        let attrs = paragraph_css(
                            properties,
                            mode == "layout" || html,
                            (mode == "layout").then_some(left),
                        )?;
                        let dir = direction(properties);
                        if let Some(l) = list {
                            let t = if l.ordered { "ol" } else { "ul" };
                            if html {
                                writeln!(out, "<li{dir} style=\"{attrs}\">{val}</li>")?
                            } else {
                                writeln!(out, "<{t}><li>{val}</li></{t}>")?
                            }
                        } else {
                            writeln!(out, "<{elem}{dir} style=\"{attrs}\">{val}</{elem}>\n")?
                        }
                    } else if let Some(l) = list {
                        writeln!(
                            out,
                            "{}{} {val}",
                            "  ".repeat(l.depth.min(32)),
                            if l.ordered { "1." } else { "-" }
                        )?
                    } else {
                        if in_list {
                            out.push('\n')
                        }
                        if *heading > 0 {
                            write!(out, "{} ", "#".repeat((*heading).min(6) as usize))?
                        }
                        writeln!(out, "{val}\n")?
                    }
                    in_list = list.is_some();
                }
                Block::Table { rows } => {
                    in_list = false;
                    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                    if width == 0 {
                        continue;
                    }
                    let merged = rows.iter().flatten().any(|c| {
                        c.colspan.is_some() || c.rowspan.is_some() || !c.blocks.is_empty()
                    });
                    if html
                        || mode == "layout"
                        || merged
                        || rows.len().saturating_mul(width) > CELL_LIMIT
                    {
                        html_table(&mut out, rows, links, html)?;
                        // A blank line ends the Markdown HTML block, so only
                        // add one after the outermost table.
                        out.push('\n');
                    } else {
                        for (n, row) in rows.iter().enumerate() {
                            out.push('|');
                            for j in 0..width {
                                let t = row
                                    .get(j)
                                    .map(|c| runs(&c.runs, false, links))
                                    .unwrap_or_default()
                                    .replace("  \n", "<br>");
                                write!(out, " {t} |")?;
                            }
                            out.push('\n');
                            if n == 0 {
                                writeln!(out, "|{}", " --- |".repeat(width))?
                            }
                        }
                        out.push('\n')
                    }
                }
            }
            if out.len() > FILE_LIMIT {
                return Err("rendered document exceeds output limit".into());
            }
        }
        if let Some(old) = html_list {
            writeln!(out, "</{old}>")?
        }
    }
    if html {
        out.push_str("</article>\n")
    }
    Ok(out)
}
