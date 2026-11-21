//! Rebuilds LaTeX from positioned math glyphs on a PDF page.
//!
//! Hangul word processors draw an equation with ordinary glyphs of a private-use
//! math font: a stretched minus glyph is a fraction bar or a radical overline and
//! raised or lowered glyphs are scripts. Only geometry survives a PDF export, so
//! the structures below are recovered from positions and glyph widths. Every
//! routine is conservative: when a construct is not recognised the plain glyph
//! text is emitted instead of inventing structure.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Script {
    Sup,
    Sub,
}

#[derive(Clone, Debug)]
pub(super) struct Segment {
    pub text: String,
    pub script: Option<Script>,
    /// The text already holds LaTeX and must not be mapped again.
    pub latex: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Glyph {
    pub x: f32,
    pub end: f32,
    pub y: f32,
    pub size: f32,
    pub segments: Vec<Segment>,
}

impl Glyph {
    #[cfg(test)]
    pub(super) fn new(x: f32, end: f32, y: f32, size: f32, text: &str) -> Self {
        Self {
            x,
            end,
            y,
            size,
            segments: vec![Segment {
                text: text.into(),
                script: None,
                latex: false,
            }],
        }
    }

    fn literal(x: f32, end: f32, y: f32, size: f32, latex: String) -> Self {
        Self {
            x,
            end,
            y,
            size,
            segments: vec![Segment {
                text: latex,
                script: None,
                latex: true,
            }],
        }
    }

    pub(super) fn width(&self) -> f32 {
        (self.end - self.x).max(0.05)
    }

    fn text(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }

    /// A stretched dash glyph: a fraction bar or a radical overline.
    fn is_rule(&self) -> bool {
        if self.segments.iter().any(|s| s.latex || s.script.is_some()) {
            return false;
        }
        let mut count = 0;
        let mut rule = true;
        for c in self.text().chars().filter(|c| !c.is_whitespace()) {
            count += 1;
            rule &= matches!(c, '−' | '-' | '─' | '—' | '–' | '‒' | '_');
        }
        // HWP draws a short fraction bar as a stretched dash barely wider than
        // the glyph, so the width test must stay close to the font size.
        rule && (1..=12).contains(&count) && self.width() > self.size * 0.85
    }
}

pub(super) fn to_latex(glyphs: &[Glyph]) -> Option<String> {
    if glyphs.is_empty() {
        return None;
    }
    let body = tidy(&assemble(glyphs, 0));
    // Unbalanced braces mean the geometry did not describe a formula, so the
    // caller keeps the plain glyph text instead.
    if body.is_empty() || !balanced(&body) {
        return None;
    }
    Some(body)
}

fn balanced(latex: &str) -> bool {
    let mut depth = 0i32;
    for c in latex.chars() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn assemble(glyphs: &[Glyph], depth: usize) -> String {
    if glyphs.is_empty() || depth > 6 {
        return glyphs.iter().map(segments_latex).collect();
    }
    let items = fold_radicals(glyphs.to_vec(), depth);
    let items = fold_fractions(items, depth);
    linear(&items, depth)
}

// ---------------------------------------------------------------- radicals

fn fold_radicals(mut items: Vec<Glyph>, depth: usize) -> Vec<Glyph> {
    let mut i = 0;
    while i < items.len() {
        if !items[i].text().contains('√') {
            i += 1;
            continue;
        }
        let sign = items[i].clone();
        // Split the sign glyph into the text before the radical, an optional
        // index carried as a superscript and the radical body.
        let mut prefix = String::new();
        let mut index = String::new();
        let mut done = false;
        for s in &sign.segments {
            if done {
                continue;
            }
            if let Some(at) = s.text.find('√') {
                prefix.push_str(&s.text[..at]);
                done = true;
            } else if s.script == Some(Script::Sup) {
                index.push_str(s.text.trim());
            } else if s.script.is_none() {
                prefix.push_str(&s.text);
            }
        }
        // The stretched drawing glyph can leave dashes around the radical sign.
        let prefix = prefix
            .trim_start_matches(['-', '−', '─', ' '])
            .to_string();
        let overline = items.iter().enumerate().position(|(j, g)| {
            j != i
                && g.is_rule()
                && g.x >= sign.end - sign.size * 0.5
                && g.x <= sign.end + sign.size * 2.
                && g.y > sign.y + sign.size * 0.15
                && g.y < sign.y + sign.size * 1.6
        });
        let (start, end) = match overline.map(|j| items[j].clone()) {
            Some(bar) => (bar.x, bar.end),
            None => (sign.end - sign.size * 0.2, sign.end + sign.size * 2.4),
        };
        let mut radicand = Vec::new();
        let mut consumed = Vec::new();
        for (j, g) in items.iter().enumerate() {
            if j == i {
                continue;
            }
            if Some(j) == overline {
                consumed.push(j);
                continue;
            }
            let overlap = (g.end.min(end) - g.x.max(start)).max(0.);
            let centre = (g.x + g.end) / 2.;
            let near = centre >= start - sign.size * 0.5 && centre <= end + sign.size * 1.5;
            if (overlap >= g.width() * 0.45 || near) && (g.y - sign.y).abs() <= sign.size * 3.
            {
                radicand.push(g.clone());
                consumed.push(j);
            }
        }
        let body = assemble(&radicand, depth + 1);
        let index = if index.is_empty() {
            String::new()
        } else {
            format!("[{index}]")
        };
        let latex = format!("{prefix}\\sqrt{index}{{{body}}}");
        let mut shift = 0;
        for &j in &consumed {
            if j < i {
                shift += 1;
            }
        }
        for j in consumed.into_iter().rev() {
            items.remove(j);
        }
        let at = i - shift;
        let end = end.max(sign.end);
        items[at] = Glyph::literal(sign.x, end, sign.y, sign.size, latex);
        i = at + 1;
    }
    items
}

// --------------------------------------------------------------- fractions

fn fold_fractions(mut items: Vec<Glyph>, depth: usize) -> Vec<Glyph> {
    loop {
        let mut best: Option<(usize, Vec<usize>, Vec<usize>, f32)> = None;
        for (i, bar) in items.iter().enumerate() {
            if !bar.is_rule() {
                continue;
            }
            let mut above = Vec::new();
            let mut below = Vec::new();
            for (j, g) in items.iter().enumerate() {
                if j == i || g.is_rule() {
                    continue;
                }
                let overlap = (g.end.min(bar.end) - g.x.max(bar.x)).max(0.);
                if overlap < g.width().min(bar.width()) * 0.45 {
                    continue;
                }
                if g.y > bar.y + bar.size * 0.2 {
                    above.push(j);
                } else if g.y < bar.y - bar.size * 0.2 {
                    below.push(j);
                }
            }
            if above.is_empty() || below.is_empty() {
                continue;
            }
            let score = (above.len() + below.len()) as f32;
            if best
                .as_ref()
                .is_none_or(|(_, a, b, s)| score > *s || (score == *s && a.len() + b.len() == 0))
            {
                best = Some((i, above, below, score));
            }
        }
        let Some((i, above, below, _)) = best else {
            return items;
        };
        let num = assemble(
            &above.iter().map(|&j| items[j].clone()).collect::<Vec<_>>(),
            depth + 1,
        );
        let den = assemble(
            &below.iter().map(|&j| items[j].clone()).collect::<Vec<_>>(),
            depth + 1,
        );
        let bar = items[i].clone();
        let (num, sign) = match num.trim_start().strip_prefix('-') {
            Some(rest) => (rest.trim_start().to_string(), "-".to_string()),
            None => (num, String::new()),
        };
        let latex = format!("{sign}\\frac{{{num}}}{{{den}}}");
        let mut end = bar.end;
        for &j in above.iter().chain(below.iter()) {
            end = end.max(items[j].end);
        }
        let mut drop: Vec<usize> = above.into_iter().chain(below).collect();
        drop.push(i);
        drop.sort_unstable();
        for j in drop.into_iter().rev() {
            items.remove(j);
        }
        items.push(Glyph::literal(bar.x, end, bar.y, bar.size, latex));
    }
}

// ------------------------------------------------------------------ linear

fn dist(g: &Glyph, x: f32) -> f32 {
    if x < g.x {
        g.x - x
    } else if x > g.end {
        x - g.end
    } else {
        0.
    }
}

fn linear(items: &[Glyph], depth: usize) -> String {
    if items.is_empty() {
        return String::new();
    }
    let Some(main) = items
        .iter()
        .max_by(|a, b| a.width().total_cmp(&b.width()).then_with(|| a.size.total_cmp(&b.size)))
    else {
        return String::new();
    };
    let main_y = main.y;
    let main_size = main.size;
    let mut row: Vec<Glyph> = Vec::new();
    let mut above: Vec<Glyph> = Vec::new();
    let mut below: Vec<Glyph> = Vec::new();
    for g in items {
        let delta = g.y - main_y;
        // A smaller glyph only shares the row when it sits close to the main
        // baseline; otherwise it is a raised or lowered script.
        let tol = (g.size.min(main_size) * 0.45).max(0.8);
        if delta.abs() <= tol {
            row.push(g.clone());
        } else if delta > 0. {
            above.push(g.clone());
        } else {
            below.push(g.clone());
        }
    }
    row.sort_by(|a, b| a.x.total_cmp(&b.x));
    above.sort_by(|a, b| a.x.total_cmp(&b.x));
    below.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut sup: Vec<Vec<usize>> = vec![Vec::new(); row.len()];
    let mut sub: Vec<Vec<usize>> = vec![Vec::new(); row.len()];
    for (k, g) in above.iter().enumerate() {
        let centre = (g.x + g.end) / 2.;
        let nearest = row
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| dist(a, centre).total_cmp(&dist(b, centre)));
        if let Some((i, base)) = nearest
            && dist(base, centre) <= base.size * 1.5
        {
            sup[i].push(k);
        }
    }
    for (k, g) in below.iter().enumerate() {
        let centre = (g.x + g.end) / 2.;
        let nearest = row
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| dist(a, centre).total_cmp(&dist(b, centre)));
        if let Some((i, base)) = nearest
            && dist(base, centre) <= base.size * 1.5
        {
            sub[i].push(k);
        }
    }
    let mut out = String::new();
    for (i, g) in row.iter().enumerate() {
        let mut piece = g.clone();
        if let Some(&k) = sup[i].first() {
            let at = above[k].x;
            if at > g.x + g.width() * 0.35 && at < g.end {
                let (left, right) = split_at(g, at);
                out.push_str(&segments_latex(&left));
                piece = right;
            }
        }
        out.push_str(&segments_latex(&piece));
        if !sup[i].is_empty() {
            let group: Vec<Glyph> = sup[i].iter().map(|&k| above[k].clone()).collect();
            let body = assemble(&group, depth + 1);
            let body = body.trim();
            if !body.is_empty() {
                out.push_str(&format!("^{{{body}}}"));
            }
        }
        if !sub[i].is_empty() {
            let group: Vec<Glyph> = sub[i].iter().map(|&k| below[k].clone()).collect();
            let body = assemble(&group, depth + 1);
            let body = body.trim();
            if !body.is_empty() {
                out.push_str(&format!("_{{{body}}}"));
            }
        }
        out.push(' ');
    }
    out
}

fn split_at(g: &Glyph, x: f32) -> (Glyph, Glyph) {
    let mut left = Glyph {
        x: g.x,
        end: x,
        y: g.y,
        size: g.size,
        segments: Vec::new(),
    };
    let mut right = Glyph {
        x,
        end: g.end,
        y: g.y,
        size: g.size,
        segments: Vec::new(),
    };
    let mut cursor = g.x;
    for s in &g.segments {
        let mut front = String::new();
        let mut back = String::new();
        for c in s.text.chars() {
            let width = char_width(c, g.size);
            if cursor + width * 0.5 <= x {
                front.push(c);
            } else {
                back.push(c);
            }
            cursor += width;
        }
        if !front.is_empty() {
            left.segments.push(Segment {
                text: front,
                script: s.script,
                latex: s.latex,
            });
        }
        if !back.is_empty() {
            right.segments.push(Segment {
                text: back,
                script: s.script,
                latex: s.latex,
            });
        }
    }
    (left, right)
}

fn char_width(c: char, size: f32) -> f32 {
    let factor = match c {
        ' ' | '\t' => 0.25,
        'i' | 'l' | 'I' | 'j' | '!' | '.' | ',' | ':' | ';' | '\'' => 0.26,
        'm' | 'w' | 'M' | 'W' => 0.82,
        c if c.is_ascii_digit() || c.is_ascii_uppercase() => 0.6,
        c if c.is_alphabetic() && !c.is_ascii() => 1.,
        _ => 0.5,
    };
    factor * size
}

fn segments_latex(g: &Glyph) -> String {
    let mut out = String::new();
    for s in &g.segments {
        let body = if s.latex {
            s.text.clone()
        } else {
            symbols(s.text.trim())
        };
        match s.script {
            Some(Script::Sup) => out.push_str(&format!("^{{{body}}}")),
            Some(Script::Sub) => out.push_str(&format!("_{{{body}}}")),
            None => out.push_str(&body),
        }
    }
    out
}

// ----------------------------------------------------------------- symbols

fn symbols(text: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_ascii_alphabetic() {
            word.push(c);
            continue;
        }
        flush_word(&mut word, &mut out);
        match char_latex(c) {
            Some(command) => out.push_str(command),
            None => out.push_str(&escape_char(c)),
        }
    }
    flush_word(&mut word, &mut out);
    out
}

fn flush_word(word: &mut String, out: &mut String) {
    if word.is_empty() {
        return;
    }
    let name = std::mem::take(word);
    let command = match name.as_str() {
        "lim" => Some("\\lim "),
        "sin" => Some("\\sin "),
        "cos" => Some("\\cos "),
        "tan" => Some("\\tan "),
        "log" => Some("\\log "),
        "ln" => Some("\\ln "),
        "max" => Some("\\max "),
        "min" => Some("\\min "),
        "sup" => Some("\\sup "),
        "inf" => Some("\\inf "),
        "det" => Some("\\det "),
        "gcd" => Some("\\gcd "),
        "exp" => Some("\\exp "),
        "mod" => Some("\\bmod "),
        _ => None,
    };
    match command {
        Some(command) => out.push_str(command),
        None => out.push_str(&name),
    }
}

fn escape_char(c: char) -> String {
    match c {
        '{' | '}' | '%' | '#' | '&' | '$' => format!("\\{c}"),
        '\\' => "\\backslash ".into(),
        '~' => "\\sim ".into(),
        '\u{a0}' => " ".into(),
        c => c.to_string(),
    }
}

fn char_latex(c: char) -> Option<&'static str> {
    Some(match c {
        '×' => "\\times ",
        '⋅' | '·' => "\\cdot ",
        '÷' => "\\div ",
        '±' => "\\pm ",
        '∓' => "\\mp ",
        '≤' | '≦' => "\\le ",
        '≥' | '≧' => "\\ge ",
        '≠' => "\\ne ",
        '≈' => "\\approx ",
        '≡' => "\\equiv ",
        '∼' => "\\sim ",
        '≪' => "\\ll ",
        '≫' => "\\gg ",
        '∞' => "\\infty ",
        '∑' => "\\sum ",
        '∏' => "\\prod ",
        '∫' => "\\int ",
        '∬' => "\\iint ",
        '∮' => "\\oint ",
        '∂' => "\\partial ",
        '∇' => "\\nabla ",
        '∈' => "\\in ",
        '∉' => "\\notin ",
        '⊂' => "\\subset ",
        '⊆' => "\\subseteq ",
        '∪' => "\\cup ",
        '∩' => "\\cap ",
        '∅' => "\\emptyset ",
        '→' => "\\to ",
        '←' => "\\leftarrow ",
        '↔' => "\\leftrightarrow ",
        '⇒' => "\\Rightarrow ",
        '⇔' => "\\Leftrightarrow ",
        '∴' => "\\therefore ",
        '∵' => "\\because ",
        '∠' => "\\angle ",
        '⊥' => "\\perp ",
        '∥' => "\\parallel ",
        '°' => "^{\\circ}",
        '′' => "'",
        '″' => "''",
        '−' | '–' => "-",
        '─' | '—' | '‒' => "",
        '…' => "\\ldots ",
        '⋯' => "\\cdots ",
        '∗' => "\\ast ",
        '∣' => "\\mid ",
        'α' => "\\alpha ",
        'β' => "\\beta ",
        'γ' => "\\gamma ",
        'δ' => "\\delta ",
        'ε' => "\\varepsilon ",
        'ζ' => "\\zeta ",
        'η' => "\\eta ",
        'θ' => "\\theta ",
        'ι' => "\\iota ",
        'κ' => "\\kappa ",
        'λ' => "\\lambda ",
        'μ' => "\\mu ",
        'ν' => "\\nu ",
        'ξ' => "\\xi ",
        'π' => "\\pi ",
        'ρ' => "\\rho ",
        'σ' => "\\sigma ",
        'τ' => "\\tau ",
        'υ' => "\\upsilon ",
        'φ' => "\\varphi ",
        'χ' => "\\chi ",
        'ψ' => "\\psi ",
        'ω' => "\\omega ",
        'Γ' => "\\Gamma ",
        'Δ' => "\\Delta ",
        'Θ' => "\\Theta ",
        'Λ' => "\\Lambda ",
        'Ξ' => "\\Xi ",
        'Π' => "\\Pi ",
        'Σ' => "\\Sigma ",
        'Φ' => "\\Phi ",
        'Ψ' => "\\Psi ",
        'Ω' => "\\Omega ",
        _ => return None,
    })
}

fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(x: f32, end: f32, y: f32, size: f32, text: &str) -> Glyph {
        Glyph::new(x, end, y, size, text)
    }

    fn squash(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn fraction_under_a_limit() {
        let glyphs = vec![
            glyph(285.3, 380.0, 557.51, 10.98, "f(1+h)− f(1)"),
            glyph(263.8, 290.0, 549.54, 13.2, "lim"),
            glyph(283.7, 355.7, 546.84, 10.98, "────"),
            glyph(316.7, 322.0, 543.0, 10.98, "h"),
            glyph(264.4, 285.0, 540.9, 7.5, "h→0"),
        ];
        let latex = to_latex(&glyphs).unwrap();
        assert_eq!(squash(&latex), "\\lim_{h\\to0}\\frac{f(1+h)-f(1)}{h}");
    }

    #[test]
    fn radical_index_and_fraction_exponent() {
        let mut sign = Glyph::new(87.9, 117.0, 896.2, 13.0, "1.");
        sign.segments.push(Segment {
            text: "3".into(),
            script: Some(Script::Sup),
            latex: false,
        });
        sign.segments.push(Segment {
            text: "√".into(),
            script: None,
            latex: false,
        });
        let glyphs = vec![
            glyph(157.8, 160.0, 909.96, 7.5, "1"),
            glyph(156.7, 167.0, 902.82, 7.5, "───"),
            glyph(157.8, 160.0, 900.18, 7.5, "3"),
            glyph(117.0, 160.0, 898.92, 10.98, "────"),
            sign,
            glyph(117.18, 160.0, 895.32, 10.98, "32 × 4"),
            glyph(148.92, 152.0, 905.46, 7.5, "−"),
        ];
        let latex = to_latex(&glyphs).unwrap();
        assert_eq!(
            squash(&latex),
            "1.\\sqrt[3]{32\\times4^{-\\frac{1}{3}}}"
        );
    }

    #[test]
    fn simple_superscript_attaches_to_the_base() {
        let glyphs = vec![
            glyph(10.0, 16.0, 100.0, 12.0, "x"),
            glyph(16.4, 20.0, 104.0, 8.0, "2"),
        ];
        assert_eq!(squash(&to_latex(&glyphs).unwrap()), "x^{2}");
    }

    #[test]
    fn subscript_attaches_to_the_base() {
        let glyphs = vec![
            glyph(10.0, 16.0, 100.0, 12.0, "a"),
            glyph(16.4, 20.0, 96.0, 8.0, "n"),
        ];
        assert_eq!(squash(&to_latex(&glyphs).unwrap()), "a_{n}");
    }

    #[test]
    fn plain_glyphs_stay_text() {
        let glyphs = vec![glyph(10.0, 20.0, 100.0, 11.0, "3x+1")];
        assert_eq!(squash(&to_latex(&glyphs).unwrap()), "3x+1");
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(to_latex(&[]).is_none());
    }
}
