use crate::{formatting, model::*, output, structure};
use std::{borrow::Cow, collections::HashSet, fmt::Write, ops::Range};
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Escape markup while preserving literal labels such as `<보기>` and `<100점>`.
/// A trailing `<` is plain text when a style change splits a label across runs.
fn potential_markup(tail: &str) -> bool {
    let mut chars = tail.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first.is_ascii_digit() {
        // Only an email autolink can begin with a digit.
        return tail
            .split(|c: char| !c.is_ascii_graphic() || c == '<')
            .next()
            .and_then(|token| token.split_once('>'))
            .is_some_and(|(address, _)| address.contains('@'));
    }
    if !(first.is_ascii_alphabetic() || matches!(first, '/' | '!' | '?')) {
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

/// A semicolon-terminated character reference could be decoded by Markdown.
fn potential_entity(tail: &str) -> bool {
    let (body, valid_char, min_len): (&str, fn(char) -> bool, usize) = match tail.as_bytes() {
        [b'#', b'x' | b'X', ..] => (&tail[2..], |c| c.is_ascii_hexdigit(), 1),
        [b'#', ..] => (&tail[1..], |c| c.is_ascii_digit(), 1),
        [first, ..] if first.is_ascii_alphabetic() => (tail, |c| c.is_ascii_alphanumeric(), 2),
        _ => return false,
    };
    let rest = body.trim_start_matches(valid_char);
    body.len() - rest.len() >= min_len && rest.starts_with(';')
}

/// Byte offsets of the brackets that must stay escaped because they start a
/// Markdown link (`[text](target)` or `[text][label]`).
///
/// Annotations such as `[2점]`, `[A]` or `[1～3]` are plain text and are written
/// without backslashes. Font changes split one annotation into several runs, so
/// the rule only escapes a pair that is complete inside the same text fragment.
fn link_brackets(text: &str) -> Vec<usize> {
    let mut escaped = Vec::new();
    let mut openings = Vec::new();
    for (i, bracket) in text.match_indices(['[', ']']) {
        if bracket == "[" {
            openings.push(i);
        } else {
            if !openings.is_empty() && matches!(text.as_bytes().get(i + 1), Some(b'(' | b'[')) {
                escaped.append(&mut openings);
                escaped.push(i);
            }
            openings.clear();
        }
    }
    escaped
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkdownContext {
    Block,
    Heading,
    Table,
}

fn block_marker(line: &str) -> bool {
    let line = line.trim_end_matches([' ', '\t', '\r', '\n']);
    let Some(marker) = line.chars().next() else {
        return false;
    };
    let rest = &line[marker.len_utf8()..];
    let list = rest.is_empty() || rest.starts_with([' ', '\t']);
    let rule = || {
        line.chars().filter(|&c| c == marker).count() >= 3
            && rest.chars().all(|c| c == marker || matches!(c, ' ' | '\t'))
    };
    match marker {
        '-' => list || rest.chars().all(|c| c == '-') || rule(),
        '*' => list || rule(),
        '+' => list,
        '_' => rule(),
        '#' => {
            let tail = rest.trim_start_matches('#');
            rest.len() - tail.len() < 6 && (tail.is_empty() || tail.starts_with([' ', '\t']))
        }
        '~' => rest.starts_with("~~"),
        '`' => rest.starts_with("``") && !rest.trim_start_matches('`').contains('`'),
        _ => false,
    }
}

/// Mark literal delimiter runs that could open emphasis, strikethrough or code.
/// An underscore cannot open after a letter or digit or close before one.
fn inline_delimiters(
    text: &str,
    markup_edges: &[usize],
    styled_stars: bool,
    styled_tildes: bool,
) -> Vec<Range<usize>> {
    let mut escaped = Vec::new();
    let mut chars = text.char_indices().rev().peekable();
    let mut after = ' ';
    let mut closers = [false; 4];
    let mut code_lengths = HashSet::new();
    while let Some((mut start, marker)) = chars.next() {
        if !matches!(marker, '*' | '_' | '~' | '`') {
            after = marker;
            continue;
        }
        let end = start + 1;
        while chars.peek().is_some_and(|&(_, c)| c == marker)
            && markup_edges.binary_search(&start).is_err()
        {
            start = chars.next().unwrap().0;
        }
        // Generated formatting separates literal characters at the run boundary.
        let left_edge = markup_edges.binary_search(&start).is_ok();
        let right_edge = markup_edges.binary_search(&end).is_ok();
        let before = if left_edge {
            '!'
        } else {
            chars.peek().map_or(' ', |&(_, c)| c)
        };
        let next = if right_edge { '!' } else { after };
        let length = end - start;
        let escape = match marker {
            '`' => !code_lengths.insert(length),
            '~' if length > 2 => styled_tildes && (left_edge || right_edge),
            _ => {
                let index = match marker {
                    '*' => 0,
                    '_' => 1,
                    _ => 1 + length,
                };
                let opens = !next.is_whitespace() && (marker != '_' || !before.is_alphanumeric());
                let closes = !before.is_whitespace() && (marker != '_' || !next.is_alphanumeric());
                let generated = (marker == '*' && styled_stars) || (marker == '~' && styled_tildes);
                let escape = (opens && closers[index]) || (generated && (opens || closes));
                closers[index] |= closes;
                escape
            }
        };
        if escape {
            escaped.push(start..end);
        }
        after = marker;
    }
    escaped
}

struct MarkdownEscaper<'a> {
    source: &'a str,
    delimiters: Vec<Range<usize>>,
    context: MarkdownContext,
    heading_hash: Option<usize>,
}

impl<'a> MarkdownEscaper<'a> {
    fn new(source: &'a str, context: MarkdownContext, runs: &[Run]) -> Self {
        let mut markup_edges = Vec::new();
        let mut offset = 0;
        for group in runs.chunk_by(same_markdown_style) {
            let plain = markdown_plain(&group[0]);
            if !plain {
                markup_edges.push(offset);
            }
            offset += group.iter().map(|run| run.text.len()).sum::<usize>();
            if !plain {
                markup_edges.push(offset);
            }
        }
        let heading_hash = if context == MarkdownContext::Heading {
            let line = source
                .split('\n')
                .next()
                .unwrap()
                .trim_end_matches([' ', '\t', '\r']);
            let prefix = line.trim_end_matches('#');
            (prefix.len() < line.len() && (prefix.is_empty() || prefix.ends_with([' ', '\t'])))
                .then_some(prefix.len())
        } else {
            None
        };
        Self {
            source,
            delimiters: inline_delimiters(
                source,
                &markup_edges,
                runs.iter().any(|r| r.style.bold || r.style.italic),
                runs.iter().any(|r| r.style.strike),
            ),
            context,
            heading_hash,
        }
    }

    fn escape(&mut self, range: Range<usize>, starts_line: bool) -> String {
        let mut out = String::with_capacity(range.len());
        let mut offset = range.start;
        for (n, line) in self.source[range].split_inclusive('\n').enumerate() {
            let mut block = None;
            let mut table_pipe = None;
            if (n > 0 || starts_line)
                && self.context != MarkdownContext::Table
                && (self.context != MarkdownContext::Heading || n > 0)
                && line
                    .trim_start_matches([' ', '\t'])
                    .starts_with(['-', '+', '*', '_', '#', '~', '`', '|', ':'])
            {
                let full_line = self.source[offset..].split('\n').next().unwrap();
                let trimmed = full_line.trim_start_matches([' ', '\t']);
                if block_marker(trimmed) {
                    block = Some(full_line.len() - trimmed.len());
                }
                if full_line.contains('-')
                    && full_line
                        .bytes()
                        .all(|b| matches!(b, b'|' | b'-' | b':' | b' ' | b'\t' | b'\r'))
                {
                    table_pipe = full_line.find('|');
                }
            }
            let link = link_brackets(line);
            for (i, c) in line.char_indices() {
                while self
                    .delimiters
                    .last()
                    .is_some_and(|range| range.end <= offset + i)
                {
                    self.delimiters.pop();
                }
                let inline = self
                    .delimiters
                    .last()
                    .is_some_and(|range| range.contains(&(offset + i)));
                let escape = inline
                    || block == Some(i)
                    || match c {
                        '<' => potential_markup(&line[i + 1..]),
                        '[' | ']' => link.binary_search(&i).is_ok(),
                        '|' => self.context == MarkdownContext::Table || table_pipe == Some(i),
                        '#' => self.heading_hash == Some(offset + i),
                        '\\' => line
                            .as_bytes()
                            .get(i + 1)
                            .is_none_or(|b| b.is_ascii_punctuation() || matches!(b, b'\r' | b'\n')),
                        _ => false,
                    };
                if escape {
                    out.push('\\');
                }
                if c == '&' && potential_entity(&line[i + 1..]) {
                    out.push_str("&amp;");
                } else {
                    out.push(c);
                }
            }
            offset += line.len();
        }
        out
    }
}

fn pdf_font_size(runs: &[Run]) -> f32 {
    runs.iter()
        .filter_map(|run| run.style.size.as_deref())
        .find_map(|size| size.strip_suffix("pt")?.parse::<f32>().ok())
        .filter(|size| size.is_finite() && *size > 0.)
        .unwrap_or(12.)
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

fn html_image(alt: &str, src: &str, path: &str, layout: &ImageGeometry) -> String {
    let mut style = String::new();
    if let Some(w) = layout.width {
        let _ = write!(style, "width:{w:.4}pt;");
    }
    if let Some(h) = layout.height {
        if let Some(w) = layout.width {
            let _ = write!(style, "aspect-ratio:{w:.4}/{h:.4};");
        } else {
            let _ = write!(style, "height:{h:.4}pt;");
        }
    }
    if layout.rotation != 0. || layout.flip_x || layout.flip_y {
        let _ = write!(
            style,
            "transform:rotate({:.4}deg) scale({},{});",
            layout.rotation,
            if layout.flip_x { -1 } else { 1 },
            if layout.flip_y { -1 } else { 1 }
        );
    }
    let [left, top, right, bottom] = layout.crop;
    let (x, y) = (1. - left - right, 1. - top - bottom);
    if layout.width.is_some()
        && layout.height.is_some()
        && layout.crop != [0.; 4]
        && x >= 0.001
        && y >= 0.001
    {
        format!(
            "<span class=\"source-crop\" style=\"{style}\"><img alt=\"{alt}\" src=\"{src}\"{path} style=\"left:{:.5}%;top:{:.5}%;width:{:.5}%;height:{:.5}%\"></span>",
            -100. * left / x,
            -100. * top / y,
            100. / x,
            100. / y
        )
    } else {
        format!("<img alt=\"{alt}\" src=\"{src}\"{path} style=\"{style}\">")
    }
}

fn markdown_text(run: &Run) -> bool {
    run.link.is_none() && run.image.is_none() && run.latex.is_none() && run.mathml.is_none()
}

fn markdown_plain(run: &Run) -> bool {
    !run.style.bold
        && !run.style.italic
        && !run.style.strike
        && !run.style.underline
        && run.style.script.is_none()
        && markdown_text(run)
}

fn same_markdown_style(a: &Run, b: &Run) -> bool {
    markdown_text(a)
        && markdown_text(b)
        && a.style.bold == b.style.bold
        && a.style.italic == b.style.italic
        && a.style.strike == b.style.strike
        && a.style.underline == b.style.underline
        && a.style.script == b.style.script
}

fn runs(
    items: &[Run],
    context: Option<MarkdownContext>,
    links: &Properties,
    geometry: bool,
) -> String {
    let html = context.is_none();
    let source = context.map(|_| match items {
        [r] => Cow::Borrowed(r.text.as_str()),
        _ => Cow::Owned(items.iter().map(|r| r.text.as_str()).collect::<String>()),
    });
    let mut markdown = context
        .zip(source.as_deref())
        .map(|(context, source)| MarkdownEscaper::new(source, context, items));
    let mut out = String::new();
    let mut at_margin = true;
    let mut offset = 0;
    for group in items.chunk_by(|a, b| !html && same_markdown_style(a, b)) {
        let r = &group[0];
        let mut t = match &mut markdown {
            None => escape(&r.text).replace('\n', "<br>"),
            Some(escape) => {
                let end = offset + group.iter().map(|r| r.text.len()).sum::<usize>();
                let text = escape.escape(offset..end, at_margin);
                offset = end;
                text.replace('\n', "  \n")
            }
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
            && let Some(src) = links
                .get(&output::inline_key(img))
                .or_else(|| links.get(img))
        {
            let alt = r.image_alt.as_deref().unwrap_or(&r.text);
            // An inlined image keeps the extracted file path as `data-path`, so
            // the result still shows where the file itself was written.
            let path = if let Some(link) = links.get(img).filter(|link| *link != src) {
                format!(" data-path=\"{}\"", escape(&uri(link)))
            } else {
                String::new()
            };
            t = if let Some(geometry) = r.image_geometry.as_ref().filter(|_| geometry) {
                html_image(&escape(alt), &escape(&uri(src)), &path, geometry)
            } else if let Some(layout) = &r.image_layout {
                format!(
                    "<img alt=\"{}\" src=\"{}\"{path} style=\"width:{}pt;height:{}pt;transform:rotate({}deg)\">",
                    escape(alt),
                    escape(&uri(src)),
                    f64::from(layout.width) / 100.,
                    f64::from(layout.height) / 100.,
                    layout.rotation_deg
                )
            } else if html {
                format!(
                    "<img alt=\"{}\" src=\"{}\"{path}>",
                    escape(alt),
                    escape(&uri(src))
                )
            } else {
                format!("![{}](<{}>)", t, uri(src))
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
        if !html {
            if let Some((_, tail)) = t.rsplit_once('\n') {
                at_margin = tail.bytes().all(|b| matches!(b, b' ' | b'\t'));
            } else {
                at_margin &= t.bytes().all(|b| matches!(b, b' ' | b'\t'));
            }
        }
        out += &t;
    }
    out
}
fn hwp3_page(out: &mut String, paper: [i32; 2], page: &Hwp3Page, links: &Properties) -> Result<()> {
    let [w, h] = paper;
    writeln!(
        out,
        "<svg class=\"source-page\" xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" style=\"width:{}pt\" aria-label=\"Page {}\">",
        f64::from(w) / 25.,
        page.number + 1
    )?;
    for graphic in &page.graphics {
        let file = links.get(&graphic.asset);
        let embedded = links.get(&output::inline_key(&graphic.asset));
        let Some(src) = embedded.or(file) else {
            continue;
        };
        let [x, y, w, h] = graphic.rect;
        write!(
            out,
            "<image x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" href=\"{}\"",
            escape(&uri(src))
        )?;
        if let Some(path) = file.filter(|_| embedded.is_some()) {
            write!(out, " data-path=\"{}\"", escape(&uri(path)))?;
        }
        out.push_str("/>\n");
        if out.len() > EXPANSION_LIMIT {
            return Err("rendered document exceeds output limit".into());
        }
    }
    for line in &page.lines {
        if line.parts.iter().all(|p| match p {
            Hwp3Text::Text(run) => run.text.trim().is_empty(),
            Hwp3Text::Tab(_) => true,
        }) {
            continue;
        }
        let Hwp3Line { x, y, size, parts } = line;
        let width = (w - x).max(1);
        let height = i32::from(*size) * 2;
        write!(
            out,
            "<foreignObject x=\"{x}\" y=\"{y}\" width=\"{width}\" height=\"{height}\"><div xmlns=\"http://www.w3.org/1999/xhtml\" class=\"source-line\" style=\"font-size:{size}px\">"
        )?;
        for part in parts {
            match part {
                Hwp3Text::Text(run) => {
                    let mut run = (**run).clone();
                    run.style.size = run
                        .style
                        .size
                        .as_deref()
                        .and_then(formatting::length_in_points)
                        .map(|pt| pt * 25. / f64::from(*size))
                        .filter(|scale| (*scale - 1.).abs() > f64::EPSILON)
                        .map(|scale| format!("{scale}em"));
                    out.push_str(&runs(std::slice::from_ref(&run), None, links, true));
                }
                Hwp3Text::Tab(width) => write!(
                    out,
                    "<span style=\"display:inline-block;width:{width}px\"></span>"
                )?,
            }
        }
        out.push_str("</div></foreignObject>\n");
    }
    out.push_str("</svg>\n");
    Ok(())
}

fn pptx_slide(
    out: &mut String,
    slide: &SlideLayout,
    blocks: &[Block],
    links: &Properties,
) -> Result<()> {
    let [width, height] = slide.size;
    let background = slide.background.as_deref().unwrap_or("#ffffff");
    writeln!(
        out,
        "<svg class=\"source-page source-slide\" xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\" style=\"width:{width}px;background:{background}\">"
    )?;
    for frame in &slide.frames {
        let [a, b, c, d, e, f] = frame.transform;
        let [w, h] = frame.size;
        writeln!(
            out,
            "<g transform=\"matrix({a:.6} {b:.6} {c:.6} {d:.6} {e:.6} {f:.6})\">"
        )?;
        if let Some(shape) = &frame.shape {
            pptx_shape(out, shape, w, h)?;
        }
        let content = &blocks[frame.blocks.clone()];
        if !content.is_empty() && w > 0. && h > 0. {
            let SlideText {
                size,
                padding: [l, t, r, b],
                vertical,
            } = frame.text;
            writeln!(
                out,
                "<foreignObject width=\"{w}\" height=\"{h}\" overflow=\"visible\"><div xmlns=\"http://www.w3.org/1999/xhtml\" class=\"source-frame\" style=\"font-size:{size}px;padding:{t}px {r}px {b}px {l}px;justify-content:{vertical}\">"
            )?;
            for block in content {
                match block {
                    Block::Paragraph {
                        runs: items,
                        properties,
                        heading,
                        list,
                        ..
                    } => {
                        let css = paragraph_css(properties, true, None)?;
                        let elem = if *heading > 0 { "h2" } else { "p" };
                        let marker = match list {
                            Some(l) if l.ordered => "1. ",
                            Some(_) => "• ",
                            None => "",
                        };
                        let text = runs(items, None, links, true);
                        writeln!(
                            out,
                            "<{elem}{} style=\"{css}\">{marker}{text}</{elem}>",
                            direction(properties)
                        )?;
                    }
                    Block::Table { rows } => html_table(out, rows, links, TableStyle::Html)?,
                }
            }
            out.push_str("</div></foreignObject>\n");
        }
        out.push_str("</g>\n");
        if out.len() > EXPANSION_LIMIT {
            return Err("rendered document exceeds output limit".into());
        }
    }
    out.push_str("</svg>\n");
    Ok(())
}

fn pptx_shape(out: &mut String, shape: &SlideShape, w: f64, h: f64) -> Result<()> {
    let (x, y) = (w / 2., h / 2.);
    let attrs = match shape.kind.as_str() {
        "rect" => format!("<rect width=\"{w}\" height=\"{h}\""),
        "roundRect" => format!(
            "<rect width=\"{w}\" height=\"{h}\" rx=\"{}\"",
            w.min(h) / 6.
        ),
        "ellipse" => format!("<ellipse cx=\"{x}\" cy=\"{y}\" rx=\"{x}\" ry=\"{y}\""),
        "line" => format!("<line x2=\"{w}\" y2=\"{h}\""),
        "triangle" => format!("<polygon points=\"{x},0 {w},{h} 0,{h}\""),
        "rtTriangle" => format!("<polygon points=\"0,0 {w},{h} 0,{h}\""),
        "diamond" => format!("<polygon points=\"{x},0 {w},{y} {x},{h} 0,{y}\""),
        "chevron" => {
            let s = w.min(h) / 2.;
            format!(
                "<polygon points=\"0,0 {},0 {w},{y} {},{h} 0,{h} {s},{y}\"",
                w - s,
                w - s
            )
        }
        _ => return Ok(()),
    };
    let fill = shape.fill.as_deref().unwrap_or("none");
    write!(out, "{attrs} fill=\"{fill}\"")?;
    if let Some((color, width)) = &shape.stroke {
        write!(out, " stroke=\"{color}\" stroke-width=\"{width}\"")?;
    }
    out.push_str("/>\n");
    Ok(())
}

fn cell_attributes(out: &mut String, props: &Properties, content_height: bool) -> Result<()> {
    let mut css = Properties::new();
    for (key, value) in props {
        // HWP cell heights come from the paginated layout. In HTML a row
        // should grow with the text and nested tables inside it.
        if key != "border" && !(content_height && key == "height") {
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
        "font-size",
    ] {
        if let Some(value) = props
            .get(key)
            .and_then(|v| formatting::length(v, !matches!(key, "line-height" | "font-size")))
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
    if let Some(marker) = props.get("list-marker")
        && let Some(c) = marker.chars().next()
        && marker.len() == c.len_utf8()
        && structure::is_bullet(c)
    {
        write!(css, "list-style-type:'{c} ';")?;
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

fn cell_width(cell: &Cell) -> Option<f64> {
    cell.properties
        .get("width")
        .and_then(|v| formatting::length_in_points(v))
        .filter(|v| *v > 0. && *v <= 10000.)
}

fn table_widths(rows: &[Vec<Cell>]) -> Option<Vec<f64>> {
    if !rows
        .iter()
        .flatten()
        .any(|cell| cell.properties.contains_key("width"))
    {
        return None;
    }
    let mut columns: Vec<(usize, f64)> = Vec::new();
    let mut visited = 0;
    for (y, row) in rows.iter().enumerate() {
        let mut x = 0;
        for cell in row {
            while columns.get(x).is_some_and(|&(end, _)| end > y) {
                x += 1;
                visited += 1;
                if visited > CELL_LIMIT * 2 {
                    return None;
                }
            }
            let span = cell.colspan.unwrap_or(1);
            let end = x.checked_add(span)?;
            if span == 0 || end > 16384 {
                return None;
            }
            visited = visited.saturating_add(span);
            if visited > CELL_LIMIT * 2 {
                return None;
            }
            if columns.len() < end {
                columns.resize(end, (0, 0.));
            }
            for column in &mut columns[x..end] {
                if column.0 > y {
                    return None;
                }
                column.0 = y.saturating_add(cell.rowspan.unwrap_or(1));
            }
            if span == 1
                && let Some(width) = cell_width(cell)
            {
                columns[x].1 = columns[x].1.max(width);
            }
            x = end;
        }
    }
    columns
        .iter()
        .all(|&(_, width)| width > 0.)
        .then(|| columns.into_iter().map(|(_, width)| width).collect())
}

fn hwp_merged_widths(rows: &[Vec<Cell>]) -> Option<Vec<f64>> {
    let columns = rows.first()?.iter().try_fold(0usize, |total, cell| {
        total.checked_add(cell.colspan.unwrap_or(1))
    })?;
    if columns == 0 || columns > 16384 || rows.len().saturating_mul(columns) > CELL_LIMIT {
        return None;
    }

    // Record the right edge of each cell in rows with usable widths. A cell
    // spanning multiple rows still contributes its width on each of those rows.
    let mut edges = vec![None; columns + 1];
    edges[0] = Some(0.);
    let mut spans = vec![(0, 0, None); columns];
    for (y, row) in rows.iter().enumerate() {
        let mut cells = row.iter();
        let mut x = 0;
        let mut right = Some(0.);
        while x < columns {
            let span = &mut spans[x];
            if span.0 <= y {
                let cell = cells.next()?;
                *span = (
                    y.saturating_add(cell.rowspan.unwrap_or(1)),
                    x.checked_add(cell.colspan.unwrap_or(1))?,
                    cell_width(cell),
                );
            }
            let (_, end, width) = *span;
            if end <= x || end > columns {
                return None;
            }
            x = end;
            right = right.zip(width).map(|(left, width)| left + width);
            edges[x] = edges[x].or(right);
        }
        if cells.next().is_some() {
            return None;
        }
    }
    edges[columns]?;

    // An edge may be internal to a merged cell in every row. Divide the width
    // evenly between known edges so the merged cell keeps its source width.
    let mut widths = Vec::with_capacity(columns);
    let mut left_column = 0;
    let mut left = 0.;
    for (column, right) in edges.into_iter().enumerate().skip(1) {
        if let Some(right) = right {
            if right <= left {
                return None;
            }
            let width = (right - left) / (column - left_column) as f64;
            widths.resize(column, width);
            left_column = column;
            left = right;
        }
    }
    Some(widths)
}

fn markdown_cell(out: &mut String, cell: &Cell, links: &Properties) {
    let paragraphs =
        std::iter::once(cell.runs.as_slice()).chain(cell.blocks.iter().filter_map(|block| {
            match block {
                Block::Paragraph { runs, .. } => Some(runs.as_slice()),
                Block::Table { .. } => None,
            }
        }));
    let start = out.len();
    for items in paragraphs.filter(|items| !items.is_empty()) {
        let text = runs(items, Some(MarkdownContext::Table), links, false);
        let text = text.trim_matches([' ', '\t', '\n', '\r']);
        if text.is_empty() {
            continue;
        }
        if out.len() > start {
            out.push_str("<br>");
        }
        // Each Markdown table row occupies one source line. The text renderer's
        // hard-break spaces and any preceding CR are removed at the boundary.
        let lines = text
            .lines()
            .flat_map(|line| line.trim_end_matches([' ', '\t', '\r']).split('\r'));
        for (index, line) in lines.enumerate() {
            if index > 0 {
                out.push_str("<br>");
            }
            table_inline(out, line.trim_start_matches([' ', '\t']));
        }
    }
}

fn table_inline(out: &mut String, text: &str) {
    // Literal text is already escaped by the Markdown escaper. Pipes generated
    // in inline content, such as link destinations, also need protection.
    for fragment in text.split_inclusive('|') {
        if let Some(content) = fragment.strip_suffix('|')
            && content.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 0
        {
            out.push_str(content);
            out.push_str("\\|");
        } else {
            out.push_str(fragment);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TableStyle {
    Markdown,
    Html,
    Hwp5,
}

fn html_table(
    out: &mut String,
    rows: &[Vec<Cell>],
    links: &Properties,
    style: TableStyle,
) -> Result<()> {
    let styled = style != TableStyle::Markdown;
    let hwp = style == TableStyle::Hwp5;
    let source_borders = styled
        && rows.iter().flatten().any(|c| {
            formatting::BORDERS
                .iter()
                .any(|key| c.properties.contains_key(*key))
        });
    out.push_str("<table");
    match (source_borders, hwp) {
        (true, true) => out.push_str(" class=\"source-borders hwp-table\""),
        (true, false) => out.push_str(" class=\"source-borders\""),
        (false, true) => out.push_str(" class=\"hwp-table\""),
        (false, false) => {}
    }
    let widths = if styled {
        table_widths(rows).or_else(|| if hwp { hwp_merged_widths(rows) } else { None })
    } else {
        None
    };
    if let Some(widths) = widths.filter(|v| !v.is_empty()) {
        let total = widths.iter().sum::<f64>();
        let layout = if hwp { "auto" } else { "fixed" };
        writeln!(
            out,
            " style=\"width:{total:.4}pt;table-layout:{layout}\"><colgroup>"
        )?;
        for width in widths {
            let (width, unit) = if hwp {
                (width, "pt")
            } else {
                (width / total * 100., "%")
            };
            writeln!(out, "<col style=\"width:{width:.5}{unit}\">")?;
        }
        out.push_str("</colgroup>\n");
    } else {
        out.push_str(">\n");
    }
    for row in rows {
        out.push_str("<tr>");
        for c in row {
            let tag = if c.properties.contains_key("table_header") {
                "th"
            } else {
                "td"
            };
            write!(out, "<{tag}")?;
            if let Some(n) = c.colspan {
                write!(out, " colspan=\"{n}\"")?
            }
            if let Some(n) = c.rowspan {
                write!(out, " rowspan=\"{n}\"")?
            }
            if styled {
                cell_attributes(out, &c.properties, hwp)?;
            }
            out.push('>');
            out.push_str(&runs(&c.runs, None, links, styled));
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
                        write!(
                            out,
                            "<{elem}{} style=\"{css}\">{}</{elem}>",
                            direction(properties),
                            runs(items, None, links, true)
                        )?;
                    }
                    Block::Paragraph { runs: items, .. } => {
                        writeln!(out, "<p>{}</p>", runs(items, None, links, false))?
                    }
                    Block::Table { rows } => html_table(out, rows, links, style)?,
                }
            }
            write!(out, "</{tag}>")?;
        }
        out.push_str("</tr>\n")
    }
    out.push_str(if styled { "</table>" } else { "</table>\n" });
    Ok(())
}

/// Quote one RFC 4180 field. A field that contains the delimiter, a quote or a
/// line break is wrapped in quotes and its own quotes are doubled, so `A, B`
/// and `그는 "좋다"` stay inside one cell when the CSV is read back.
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.into()
    }
}

fn csv_cell(cell: &Cell) -> String {
    let mut text = String::new();
    for run in &cell.runs {
        text.push_str(&run.text);
    }
    for block in &cell.blocks {
        let Block::Paragraph { runs, .. } = block else {
            continue;
        };
        if !text.is_empty() {
            text.push('\n');
        }
        for run in runs {
            text.push_str(&run.text);
        }
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Workbook sheets as CSV. Every selected sheet is written in order; when more
/// than one remains, a `# Sheet: NAME` line marks the sheet so that joining
/// sheets into one file loses no information.
fn csv(doc: &Document) -> Result<String> {
    if !matches!(doc.format.as_str(), "xls" | "xlsx" | "ods") {
        return Err(format!(
            "csv output supports xls, xlsx and ods workbooks, not {}",
            doc.format
        )
        .into());
    }
    let marked = doc.sections.len() > 1;
    let mut out = String::new();
    for (i, section) in doc.sections.iter().enumerate() {
        crate::limits::check_request()?;
        if marked {
            if i > 0 {
                out.push('\n');
            }
            let title: String = if section.title.trim().is_empty() {
                (i + 1).to_string()
            } else {
                section
                    .title
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>()
                    .trim()
                    .to_owned()
            };
            writeln!(out, "# Sheet: {title}")?;
        }
        for block in &section.blocks {
            crate::limits::check_request()?;
            let Block::Table { rows } = block else {
                continue;
            };
            let width = rows.iter().map(Vec::len).max().unwrap_or(0);
            for row in rows {
                for column in 0..width {
                    if column > 0 {
                        out.push(',');
                    }
                    out.push_str(&csv_field(
                        &row.get(column).map(csv_cell).unwrap_or_default(),
                    ));
                }
                out.push('\n');
                if out.len() > EXPANSION_LIMIT {
                    return Err("rendered document exceeds output limit".into());
                }
            }
        }
    }
    Ok(out)
}

fn markdown_bullet(blocks: &[Block], index: usize, runs: &[Run]) -> Option<usize> {
    let (marker, prefix) = structure::bullet_prefix(runs)?;
    if structure::is_bullet(marker) {
        return Some(prefix);
    }
    // Matching adjacent items distinguish a typed marker from punctuation.
    [index.wrapping_sub(1), index + 1]
        .into_iter()
        .any(|neighbor| {
            matches!(blocks.get(neighbor), Some(Block::Paragraph {
                heading: 0, list: None, runs, ..
            }) if structure::bullet_prefix(runs).is_some_and(|(c, _)| c == marker))
        })
        .then_some(prefix)
}

fn markdown_blank_lines(out: &mut String, count: usize) {
    if count > 0 {
        // The newline ending the last content line is separate from the
        // blank lines. Paragraph and table separators can already supply one.
        let existing = out.bytes().rev().take_while(|&b| b == b'\n').count();
        let needed = count + usize::from(!out.is_empty());
        out.extend(std::iter::repeat_n('\n', needed.saturating_sub(existing)));
    }
}

fn blank_paragraph(block: &Block) -> bool {
    matches!(block, Block::Paragraph {
        heading: 0, list: None, runs, properties
    } if !properties.contains_key("anchor") && runs.iter().all(|run| {
        run.text.trim().is_empty()
            && run.image.is_none()
            && run.latex.is_none()
            && run.mathml.is_none()
            && !run.page_break
    }))
}

fn markdown_page_padding(
    blocks: &[Block],
    has_previous_section: bool,
    has_next_section: bool,
) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut start = 0;
    blocks.split(Block::is_page_break).flat_map(move |page| {
        let end = start + page.len();
        let leading = if start > 0 || has_previous_section {
            page.iter()
                .take_while(|block| blank_paragraph(block))
                .count()
        } else {
            0
        };
        let trailing = if end < blocks.len() || has_next_section {
            page[leading..]
                .iter()
                .rev()
                .take_while(|block| blank_paragraph(block))
                .count()
        } else {
            0
        };
        let padding = [start..start + leading, end - trailing..end];
        start = end + 1;
        padding.into_iter().filter(|range| !range.is_empty())
    })
}

fn page_divider(out: &mut String, html: bool) {
    if html {
        out.push_str("<hr class=\"page-break\">\n");
    } else {
        out.truncate(out.trim_end_matches([' ', '\t', '\r', '\n']).len());
        if !out.is_empty() {
            // A blank line separates a thematic break from a Setext heading.
            out.push_str("\n\n");
        }
        out.push_str("---\n\n");
    }
}

pub fn render(doc: &Document, format: &str, mode: &str, links: &Properties) -> Result<String> {
    crate::limits::check_request()?;
    if !matches!(mode, "semantic" | "layout" | "hybrid") {
        return Err("mode must be semantic, layout or hybrid".into());
    }
    if format == "csv" {
        return csv(doc);
    }
    if format == "json" {
        return Ok(serde_json::to_string_pretty(doc)? + "\n");
    }
    let html = format == "html";
    if !html && format != "md" {
        return Err("format must be md, html, json or csv".into());
    }
    let markdown_structure = !html && !matches!(doc.format.as_str(), "xls" | "xlsx" | "ods");
    let headings = if markdown_structure {
        structure::Headings::infer(
            doc.sections.iter().flat_map(|section| &section.blocks),
            structure::HeadingEvidence::Formatted,
        )
    } else {
        None
    };
    let embedded;
    let links = if html && links.is_empty() && !doc.assets.is_empty() {
        embedded = output::inline(doc, links)?;
        &embedded
    } else {
        links
    };
    let mut out = String::new();
    if html {
        out.push_str(concat!(
            "<!doctype html>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<style>\n",
            "article { box-sizing: border-box; max-width: 76rem; margin-inline: auto; padding: 1.25rem; font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; font-size: 16px; line-height: 1.5; overflow-wrap: break-word; print-color-adjust: exact; -webkit-print-color-adjust: exact; }\n",
            "article hr.page-break { border: 0; border-top: 1px dotted #777; margin-block: 1em; }\n",
            "article table { border-collapse: collapse; max-width: 100%; margin-block: 0.5em; font: inherit; }\n",
            "article td, article th { box-sizing: border-box; border: 1px solid #a0a0a0; padding: 0.3em 0.5em; vertical-align: top; white-space: pre-wrap; overflow-wrap: anywhere; }\n",
            "article .source-borders > tbody > tr > :is(td, th) { border: none; }\n",
            "article td > :first-child { margin-top: 0; }\n",
            "article td > :last-child { margin-bottom: 0; }\n",
            "article .hwp-table-scroll { overflow-x: auto; }\n",
            "article table.hwp-table { max-width: none; }\n",
            "article .hwp-table > tbody > tr > td { word-break: keep-all; overflow-wrap: break-word; }\n",
            "article .hwp-table > tbody > tr > td > p { margin-block: 0; }\n",
            "article img { max-width: 100%; }\n",
            "article .source-crop { position: relative; display: inline-block; max-width: 100%; overflow: hidden; vertical-align: middle; }\n",
            "article .source-crop > img { position: absolute; max-width: none; }\n",
            "article .source-page { display: block; max-width: 100%; height: auto; background: white; margin-block: 0 1em; break-inside: avoid; }\n",
            "article .source-line { font-family: Batang, 'Malgun Gothic', serif; line-height: 1; white-space: pre; }\n",
            "article .source-frame { box-sizing: border-box; display: flex; flex-direction: column; height: 100%; line-height: 1.13; }\n",
            "article .source-frame > :is(p, h2, table) { font: inherit; margin: 0; }\n",
            "article .source-frame > p > :is(img, .source-crop) { vertical-align: top; }\n</style>\n<article>\n",
        ));
    }
    let mut next_slide_number = 0;
    for (i, s) in doc.sections.iter().enumerate() {
        crate::limits::check_request()?;
        let hwp3 = doc.format == "hwp" && s.source == "document";
        let table_style = if !html {
            TableStyle::Markdown
        } else if doc.format == "hwp" && !hwp3 {
            TableStyle::Hwp5
        } else {
            TableStyle::Html
        };
        let mut blank_lines = 0;
        let mut previous_pdf_line: Option<(f32, f32)> = None;
        let slide_number = if s.kind == "slide" {
            next_slide_number += 1;
            Some(s.slide_number.unwrap_or(next_slide_number))
        } else {
            None
        };
        if i > 0 && !out.is_empty() {
            page_divider(&mut out, html);
        }
        // Warnings travel as a separate block, so a page that stopped early or
        // failed leaves a marker in the body: a reader of `--stdout` output alone
        // would otherwise not notice what is missing.
        if let Some(page) = s.properties.get("skipped").and(s.properties.get("page")) {
            writeln!(out, "<!-- page {page} skipped -->\n")?;
            continue;
        }
        if s.properties.contains_key("page_error")
            && let Some(page) = s.properties.get("page")
        {
            writeln!(out, "<!-- page {page} incomplete -->\n")?;
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
                let title = MarkdownEscaper::new(&s.title, MarkdownContext::Heading, &[])
                    .escape(0..s.title.len(), true);
                writeln!(out, "# {title}\n")?
            }
        }
        let slide = s.slide_layout.as_ref().filter(|_| html);
        if let Some(slide) = slide {
            pptx_slide(&mut out, slide, &s.blocks, links)?;
        }
        let mut placed_frames = slide
            .into_iter()
            .flat_map(|slide| &slide.frames)
            .map(|frame| &frame.blocks)
            .peekable();
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
        let mut list_indents = Vec::<usize>::new();
        let mut padding = (!html)
            .then(|| markdown_page_padding(&s.blocks, i > 0, i + 1 < doc.sections.len()))
            .into_iter()
            .flatten()
            .peekable();
        let mut pages = s
            .hwp3_layout
            .as_ref()
            .filter(|_| html)
            .into_iter()
            .flat_map(|layout| layout.pages.iter().map(|page| (layout.paper, page)))
            .peekable();
        let mut replaced = s
            .hwp3_layout
            .as_ref()
            .filter(|_| html)
            .into_iter()
            .flat_map(|layout| layout.pages.iter())
            .flat_map(|page| &page.replaced_blocks)
            .peekable();
        for (index, block) in s.blocks.iter().enumerate() {
            crate::limits::check_request()?;
            let page_break = block.is_page_break();
            if page_break {
                if let Some(old) = html_list.take() {
                    writeln!(out, "</{old}>")?;
                }
                blank_lines = 0;
                previous_pdf_line = None;
                in_list = false;
                list_indents.clear();
                page_divider(&mut out, html);
            }
            while pages
                .peek()
                .is_some_and(|(_, page)| page.first_block <= index)
            {
                let (paper, page) = pages.next().unwrap();
                hwp3_page(&mut out, paper, page, links)?;
            }
            while padding.peek().is_some_and(|range| range.end <= index) {
                padding.next();
            }
            if page_break || padding.peek().is_some_and(|range| range.contains(&index)) {
                continue;
            }
            while replaced.peek().is_some_and(|range| range.end <= index) {
                replaced.next();
            }
            while placed_frames.peek().is_some_and(|range| range.end <= index) {
                placed_frames.next();
            }
            if replaced.peek().is_some_and(|range| range.contains(&index))
                || placed_frames
                    .peek()
                    .is_some_and(|range| range.contains(&index))
            {
                continue;
            }
            if matches!(doc.format.as_str(), "pdf" | "docx" | "hwpx")
                && let Block::Paragraph {
                    runs, properties, ..
                } = block
                && !properties.contains_key("anchor")
                && runs.iter().all(|r| {
                    r.text.trim().is_empty()
                        && r.mathml.is_none()
                        && r.image.as_ref().is_none_or(|image| {
                            !links.contains_key(image)
                                && !links.contains_key(&output::inline_key(image))
                        })
                })
            {
                continue;
            }
            let source_blank = hwp3 && blank_paragraph(block);
            if source_blank && !html && mode != "layout" {
                blank_lines += 1;
                in_list = false;
                list_indents.clear();
                continue;
            }
            markdown_blank_lines(&mut out, std::mem::take(&mut blank_lines));
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
                    let attrs = if hwp3 {
                        " style=\"margin-block:0\""
                    } else {
                        ""
                    };
                    writeln!(out, "<{new}{attrs}>")?
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
                    let bullet = if markdown_structure && *heading == 0 && list.is_none() {
                        markdown_bullet(&s.blocks, index, items)
                    } else {
                        None
                    };
                    let mut stripped;
                    let items = if let Some(prefix) = bullet {
                        stripped = items.clone();
                        structure::remove_prefix(&mut stripped, prefix);
                        &stripped
                    } else {
                        items
                    };
                    let inferred_list = bullet.map(|_| List {
                        ordered: false,
                        depth: 0,
                    });
                    let heading = if *heading == 0 && list.is_none() && inferred_list.is_none() {
                        headings.as_ref().and_then(|h| h.level(items)).unwrap_or(0)
                    } else {
                        *heading
                    };
                    let list = if html || heading == 0 {
                        list.as_ref().or(inferred_list.as_ref())
                    } else {
                        None
                    };
                    let use_html = html
                        || (list.is_none()
                            && heading == 0
                            && (mode == "layout"
                                || (mode == "hybrid"
                                    && (properties.contains_key("dir")
                                        || properties.contains_key("writing-mode")))));
                    let context = if heading > 0 && list.is_none() {
                        MarkdownContext::Heading
                    } else {
                        MarkdownContext::Block
                    };
                    let val = if source_blank {
                        "<br>".into()
                    } else {
                        runs(items, (!use_html).then_some(context), links, html)
                    };
                    if !html && in_list && list.is_none() {
                        out.push('\n');
                    }
                    if use_html {
                        previous_pdf_line = None;
                        let elem = if heading > 0 {
                            format!("h{}", heading.clamp(1, 6))
                        } else {
                            "p".into()
                        };
                        let attrs = paragraph_css(
                            properties,
                            mode == "layout" || html,
                            (mode == "layout").then_some(left),
                        )?;
                        let dir = direction(properties);
                        if list.is_some() {
                            writeln!(out, "<li{dir} style=\"{attrs}\">{val}</li>")?;
                        } else {
                            writeln!(out, "<{elem}{dir} style=\"{attrs}\">{val}</{elem}>\n")?
                        }
                    } else if let Some(l) = list {
                        previous_pdf_line = None;
                        list_indents.truncate(l.depth.min(32));
                        let indent: usize = list_indents.iter().sum();
                        let marker = if l.ordered { "1. " } else { "- " };
                        list_indents.push(marker.len());
                        writeln!(
                            out,
                            "{}{marker}{}",
                            " ".repeat(indent),
                            val.replace('\n', &format!("\n{}", " ".repeat(indent + marker.len())))
                        )?
                    } else {
                        if heading > 0 {
                            write!(out, "{} ", "#".repeat(heading.min(6) as usize))?
                        }
                        if doc.format == "pdf" && heading == 0 {
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
                                writeln!(out, "{val}  ")?;
                            } else {
                                writeln!(out, "{val}\n")?;
                            }
                            previous_pdf_line = pdf_last_baseline(properties).map(|y| (y, size));
                        } else {
                            previous_pdf_line = None;
                            // HWP can use leading spaces for alignment. Four or more
                            // would turn the paragraph into a Markdown code block.
                            let val =
                                if doc.format == "hwp" && heading == 0 && val.starts_with("    ") {
                                    val.trim_start_matches(' ')
                                } else {
                                    &val
                                };
                            writeln!(out, "{val}\n")?
                        }
                    }
                    in_list = list.is_some();
                    if !in_list {
                        list_indents.clear();
                    }
                }
                Block::Table { rows } => {
                    previous_pdf_line = None;
                    in_list = false;
                    list_indents.clear();
                    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                    if width == 0 {
                        continue;
                    }
                    if !html && !out.is_empty() && !out.ends_with("\n\n") {
                        out.push('\n');
                    }
                    let needs_html = rows.iter().flatten().any(|cell| {
                        cell.colspan.is_some_and(|span| span != 1)
                            || cell.rowspan.is_some_and(|span| span != 1)
                            || cell.properties.contains_key("table_headerless")
                            || cell.blocks.iter().any(|b| matches!(b, Block::Table { .. }))
                    });
                    if html || needs_html || rows.len().saturating_mul(width) > CELL_LIMIT {
                        let scrollable = table_style == TableStyle::Hwp5;
                        if scrollable {
                            out.push_str("<div class=\"hwp-table-scroll\">\n");
                        }
                        html_table(&mut out, rows, links, table_style)?;
                        if scrollable {
                            out.push_str("\n</div>");
                        }
                        // A blank line ends the Markdown HTML block, so only
                        // add one after the outermost table.
                        out.push('\n');
                    } else {
                        for (n, row) in rows.iter().enumerate() {
                            out.push('|');
                            for j in 0..width {
                                out.push(' ');
                                if let Some(cell) = row.get(j) {
                                    markdown_cell(&mut out, cell, links);
                                }
                                out.push_str(" |");
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
            if out.len() > EXPANSION_LIMIT {
                return Err("rendered document exceeds output limit".into());
            }
        }
        if let Some(old) = html_list {
            writeln!(out, "</{old}>")?
        }
        markdown_blank_lines(&mut out, blank_lines);
        for (paper, page) in pages {
            hwp3_page(&mut out, paper, page, links)?;
        }
        if out.len() > EXPANSION_LIMIT {
            return Err("rendered document exceeds output limit".into());
        }
    }
    if html {
        out.push_str("</article>\n")
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{MarkdownContext, MarkdownEscaper, render};
    use crate::model::*;

    fn markdown(text: &str) -> String {
        MarkdownEscaper::new(text, MarkdownContext::Block, &[]).escape(0..text.len(), true)
    }

    #[test]
    fn markdown_keeps_literal_punctuation_readable() {
        for text in [
            "-10도",
            "+3%",
            "#태그",
            "--option",
            "_prefix",
            "report_2026_final.pdf, 이름_부분, _prefix",
            r"C:\Users\name\test_file.txt",
            "2 * 3 | 10 ~ 20 | ` 하나",
            "Lt.toe ulcer & rest pain",
            "R&D; AT&T, A && B",
        ] {
            assert_eq!(markdown(text), text);
        }
        assert_eq!(
            markdown("- 항목\n+ 항목\n# 제목\n---\n___\n~~~\n```"),
            "\\- 항목\n\\+ 항목\n\\# 제목\n\\---\n\\___\n\\~~~\n\\```"
        );
        assert_eq!(
            markdown("_강조_ *강조* ~~취소~~ `코드`"),
            "\\_강조_ \\*강조* \\~\\~취소~~ \\`코드`"
        );
        assert_eq!(markdown("A | B\n---|---"), "A | B\n---\\|---");
        assert_eq!(
            markdown("&copy; &#38; &#x26;"),
            "&amp;copy; &amp;#38; &amp;#x26;"
        );
    }

    #[test]
    fn markdown_uses_paragraph_and_table_context_for_runs() {
        let mut doc = Document::new("docx");
        let mut section = Section::new("section", "");
        section.title = "- 제목 #".into();
        let mut filename = Run::text("_");
        filename.style.font = Some("another font".into());
        let mut bold = Run::text("값 ");
        bold.style.bold = true;
        section.blocks.push(Block::flow(vec![
            bold,
            Run::text("- 항목 | 2 * 3 | report"),
            filename,
            Run::text("id\n"),
            Run::text("-"),
            Run::text(" 새 줄"),
        ]));
        let mut bold = Run::text("10");
        bold.style.bold = true;
        section.blocks.push(Block::flow(vec![Run::text("-"), bold]));
        let mut bold = Run::text("가운데");
        bold.style.bold = true;
        section.blocks.push(Block::flow(vec![
            Run::text("_앞 "),
            bold,
            Run::text(" 뒤_"),
        ]));
        section.blocks.push(Block::Table {
            rows: vec![vec![Cell::text("- 항목 | report_id")]],
        });
        doc.sections.push(section);

        assert_eq!(
            render(&doc, "md", "hybrid", &Properties::new()).unwrap(),
            concat!(
                "# - 제목 \\#\n\n**값** - 항목 | 2 * 3 | report_id  \n\\- 새 줄\n\n",
                "-**10**\n\n\\_앞 **가운데** 뒤_\n\n",
                "| - 항목 \\| report_id |\n| --- |\n\n",
            ),
        );
    }

    #[test]
    fn markdown_tables_keep_single_line_rows_and_preserve_nested_tables() {
        let mut doc = Document::new("epub");
        let mut section = Section::new("section", "");
        let mut link = Run::text("Link | label");
        link.link = Some("https://example.org/?q=a|b".into());
        section.blocks.push(Block::Table {
            rows: vec![
                vec![Cell::text("Header"), Cell::text("Notes")],
                vec![Cell {
                    runs: vec![link],
                    blocks: vec![Block::text("First\r\nSecond")],
                    ..Cell::default()
                }],
            ],
        });
        section.blocks.push(Block::Table {
            rows: vec![vec![Cell {
                blocks: vec![
                    Block::text("Outer"),
                    Block::Table {
                        rows: vec![vec![Cell::text("Inner")]],
                    },
                ],
                ..Cell::default()
            }]],
        });
        doc.sections.push(section);

        let md = render(&doc, "md", "hybrid", &Properties::new()).unwrap();
        assert!(
            md.starts_with(concat!(
                "| Header | Notes |\n| --- | --- |\n",
                "| [Link \\| label](<https://example.org/?q=a\\|b>)<br>First<br>Second |  |\n\n"
            )),
            "{md}"
        );
        assert_eq!(md.matches("<table>").count(), 2);
        assert!(md.contains("<p>Outer</p>\n<table>\n<tr><td>Inner</td></tr>"));
    }

    #[test]
    fn missing_pages_leave_a_marker_in_the_body() {
        let mut doc = Document::new("pdf");
        let mut failed = Section::new("page", "1 0 R").indexed(0);
        failed.properties.insert("page".into(), "1".into());
        failed.properties.insert("page_error".into(), "boom".into());
        failed.paragraph("Partial page");
        let mut skipped = Section::new("page", "").indexed(1);
        skipped.properties.insert("page".into(), "2".into());
        skipped
            .properties
            .insert("skipped".into(), "time_limit".into());
        doc.sections.push(failed);
        doc.sections.push(skipped);
        let out = render(&doc, "md", "hybrid", &Properties::new()).unwrap();
        assert!(out.contains("<!-- page 1 incomplete -->"), "{out}");
        assert!(out.contains("<!-- page 2 skipped -->"), "{out}");
        assert!(out.contains("Partial page"), "{out}");
        assert!(out.find("page 1 incomplete").unwrap() < out.find("Partial page").unwrap());
    }

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
        assert_eq!(markdown("<100점 만점 기준>"), "<100점 만점 기준>");
        assert_eq!(markdown("<1~3>과 <42>"), "<1~3>과 <42>");
        assert_eq!(markdown("<"), "<");
        assert_eq!(markdown(">"), ">");
        assert_eq!(markdown("고개 > 고개"), "고개 > 고개");
        assert_eq!(markdown("<A> 참조"), "<A> 참조");
    }

    #[test]
    fn markup_is_still_escaped() {
        assert_eq!(markdown("<div>"), "\\<div>");
        assert_eq!(markdown("<https://example.com>"), "\\<https://example.com>");
        assert_eq!(markdown("<123@example.com>"), "\\<123@example.com>");
    }

    #[test]
    fn csv_quotes_commas_and_marks_sheets() {
        let mut doc = Document::new("xlsx");
        let mut sales = Section::new("sheet", "xl/worksheets/sheet1.xml").indexed(0);
        sales.title = "매출".into();
        sales.blocks.push(Block::Table {
            rows: vec![
                vec![Cell::text("이름"), Cell::text("금액, 단위")],
                vec![Cell::text("A, B"), Cell::text("그는 \"좋다\"")],
                vec![Cell::text("줄\n바꿈")],
            ],
        });
        let mut summary = Section::new("sheet", "xl/worksheets/sheet2.xml").indexed(1);
        summary.title = "Summary".into();
        summary.blocks.push(Block::Table {
            rows: vec![vec![Cell::text("42")]],
        });
        doc.sections.push(sales);
        doc.sections.push(summary);
        let out = render(&doc, "csv", "hybrid", &Properties::new()).unwrap();
        assert_eq!(
            out,
            "# Sheet: 매출\n이름,\"금액, 단위\"\n\"A, B\",\"그는 \"\"좋다\"\"\"\n\"줄\n바꿈\",\n\n# Sheet: Summary\n42\n"
        );
        // A single selected sheet stays plain CSV without sheet markers.
        doc.sections.remove(0);
        assert_eq!(
            render(&doc, "csv", "hybrid", &Properties::new()).unwrap(),
            "42\n"
        );
        assert!(render(&Document::new("docx"), "csv", "hybrid", &Properties::new()).is_err());
    }
}
