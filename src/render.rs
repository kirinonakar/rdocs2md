use crate::{formatting, model::*};
use std::fmt::Write;
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A `<` needs escaping only when it can start an HTML tag or an autolink, which
/// always continues with an ASCII letter, digit or tag punctuation. Labels such
/// as `<보기>`, `<보 기>` and `<A>` are plain text in Korean exam papers, and font
/// changes split one label into several runs, so a trailing `<` is also plain.
fn potential_markup(tail: &str) -> bool {
    let mut chars = tail.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphanumeric() || matches!(first, '/' | '!' | '?')) {
        return false;
    }
    // A short all-uppercase label cannot be a tag name.
    let mut inside = String::from(first);
    for c in chars {
        if c == '>' {
            let label = inside.trim();
            return !(label.chars().count() <= 4 && label.chars().all(|c| c.is_uppercase()));
        }
        if inside.chars().count() > 8 {
            break;
        }
        inside.push(c);
    }
    true
}

/// Byte offsets of the brackets that must stay escaped because they start a
/// Markdown link (`[text](target)` or `[text][label]`).
///
/// Annotations such as `[2점]`, `[A]` or `[1～3]` are plain text and are written
/// without backslashes. Font changes split one annotation into several runs, so
/// the rule only escapes a pair that is complete inside the same text fragment.
fn link_brackets(text: &str) -> Vec<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut escaped = Vec::new();
    for (i, (start, c)) in chars.iter().enumerate() {
        if *c != '[' {
            continue;
        }
        let Some(j) = (i + 1..chars.len()).find(|&j| chars[j].1 == ']') else {
            continue;
        };
        if matches!(chars.get(j + 1).map(|(_, c)| *c), Some('(') | Some('[')) {
            escaped.push(*start);
            escaped.push(chars[j].0);
        }
    }
    escaped
}

fn markdown(s: &str) -> String {
    let mut out = String::new();
    for line in s.split_inclusive('\n') {
        let mut margin = true;
        let link = link_brackets(line);
        for (i, c) in line.char_indices() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' if potential_markup(&line[i + 1..]) => out.push_str("\\<"),
                // `>` is never escaped: font changes often start a run with the
                // closing bracket of `<보기>` or with the arrow of `고개 > 고개`,
                // and both are plain text.
                '[' | ']' if !link.contains(&i) => out.push(c),
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

fn pdf_font_size(runs: &[Run]) -> f32 {
    runs.iter()
        .filter_map(|run| run.style.size.as_deref())
        .find_map(|size| size.strip_suffix("pt")?.parse::<f32>().ok())
        .filter(|size| size.is_finite() && *size > 0.)
        .unwrap_or(12.)
}

#[cfg(test)]
mod tests {
    use super::markdown;

    #[test]
    fn annotations_keep_literal_brackets() {
        assert_eq!(markdown("값은? [2점]"), "값은? [2점]");
        assert_eq!(markdown("보기는 [1～3] 이다"), "보기는 [1～3] 이다");
        assert_eq!(markdown("[A] 대응된다"), "[A] 대응된다");
        assert_eq!(markdown("[보 기]"), "[보 기]");
    }

    #[test]
    fn link_syntax_stays_escaped() {
        assert_eq!(markdown("[text](target)"), "\\[text\\](target)");
        assert_eq!(markdown("[ref][label]"), "\\[ref\\][label]");
    }

    #[test]
    fn labels_keep_literal_angle_brackets() {
        assert_eq!(markdown("<보기>는"), "<보기>는");
        assert_eq!(markdown("<"), "<");
        assert_eq!(markdown(">"), ">");
        assert_eq!(markdown("고개 > 고개"), "고개 > 고개");
        assert_eq!(markdown("<A> 참조"), "<A> 참조");
    }

    #[test]
    fn markup_is_still_escaped() {
        assert_eq!(markdown("<div>"), "\\<div>");
        assert_eq!(markdown("<https://example.com>"), "\\<https://example.com>");
    }
}

fn pdf_baseline(properties: &Properties) -> Option<f32> {
    properties
        .get("y_pt")?
        .parse::<f32>()
        .ok()
        .filter(|y| y.is_finite())
}

fn pdf_last_baseline(properties: &Properties) -> Option<f32> {
    properties
        .get("last_y_pt")
        .or_else(|| properties.get("y_pt"))?
        .parse::<f32>()
        .ok()
        .filter(|y| y.is_finite())
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
        if let Some(latex) = r
            .latex
            .clone()
            .or_else(|| r.mathml.as_deref().and_then(crate::latex::from_mathml))
        {
            t = if html {
                format!("<span class=\"math\">\\({latex}\\)</span>")
            } else {
                format!("${latex}$")
            };
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
            "article { box-sizing: border-box; max-width: 76rem; margin-inline: auto; padding: 1.25rem; font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; font-size: 16px; line-height: 1.5; overflow-wrap: break-word; print-color-adjust: exact; -webkit-print-color-adjust: exact; }\n",
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
        let mut previous_pdf_line: Option<(f32, f32)> = None;
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
                        previous_pdf_line = None;
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
                        previous_pdf_line = None;
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
                        if doc.format == "pdf" && *heading == 0 {
                            let y = pdf_baseline(properties);
                            let size = pdf_font_size(items);
                            let adjacent = y.is_some_and(|y| {
                                previous_pdf_line.is_some_and(|(previous_y, previous_size)| {
                                    // Only a gap within normal leading continues the
                                    // same text block; paragraph spacing is larger.
                                    previous_y - y <= previous_size.max(size) * 1.5
                                })
                            });
                            if adjacent {
                                write!(out, "{val}  \n")?;
                            } else {
                                writeln!(out, "{val}\n")?;
                            }
                            previous_pdf_line = pdf_last_baseline(properties).map(|y| (y, size));
                        } else {
                            previous_pdf_line = None;
                            writeln!(out, "{val}\n")?
                        }
                    }
                    in_list = list.is_some();
                }
                Block::Table { rows } => {
                    previous_pdf_line = None;
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
