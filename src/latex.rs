//! Presentation MathML (and the OMML subset normalized by `math.rs`) to LaTeX.
//!
//! The converter is intentionally lossy for unsupported constructs: unknown
//! elements are unwrapped and their children are kept, so no information
//! disappears from the output.
// The module is registered in `lib.rs`; callers are added separately.
#![allow(dead_code)]
use roxmltree::{Document, Node};

/// Convert presentation MathML to LaTeX.
///
/// The input is normally the normalized MathML built by `math::formula`, but
/// plain OMML and hand-written MathML are accepted too. `None` is returned when
/// the markup cannot be parsed or converts to an empty string.
pub fn from_mathml(xml: &str) -> Option<String> {
    let latex = match Document::parse(xml) {
        Ok(doc) => conv(doc.root_element()),
        Err(_) => {
            // Normalized formulas may consist of sibling roots without a parent.
            let wrapped = format!("<math>{xml}</math>");
            let doc = Document::parse(&wrapped).ok()?;
            conv(doc.root_element())
        }
    };
    let latex = latex.trim();
    (!latex.is_empty()).then(|| latex.to_string())
}

fn conv(n: Node) -> String {
    match n.tag_name().name() {
        "annotation" | "annotation-xml" | "script" | "style" => String::new(),
        "semantics" => n
            .children()
            .find(Node::is_element)
            .map(conv)
            .unwrap_or_default(),
        "mi" => ident(n),
        "mn" => escape_text(text(n).trim()),
        "mo" => operator(&text(n)),
        "mtext" => format!("\\text{{{}}}", escape_text(text(n).trim())),
        "mspace" => "\\,".into(),
        "mfrac" => fraction(n),
        "msqrt" => format!("\\sqrt{{{}}}", seq(n).trim()),
        "mroot" => root(n),
        "msup" => script(n, true),
        "msub" => script(n, false),
        "msubsup" => subsup(n),
        "munder" => under(n),
        "mover" => over(n),
        "munderover" => under_over(n),
        "mfenced" => fenced(n),
        "mtable" => table(n),
        "menclose" => enclosed(n),
        // mrow, math, mstyle, mpadded, mphantom and unknown elements keep content.
        _ => seq(n),
    }
}

/// Convert every child and join the pieces with a single space.
fn seq(n: Node) -> String {
    let mut parts = Vec::new();
    for c in n.children() {
        let s = if c.is_text() {
            escape_text(c.text().unwrap_or(""))
        } else if c.is_element() {
            conv(c)
        } else {
            continue;
        };
        let s = s.trim();
        if !s.is_empty() {
            parts.push(s.to_string())
        }
    }
    parts.join(" ")
}

/// Convert one argument element (child of a structured element).
fn arg(n: Node) -> String {
    conv(n).trim().to_string()
}

fn text(n: Node) -> String {
    let mut out = String::new();
    for t in n.descendants().filter(Node::is_text) {
        out.push_str(t.text().unwrap_or(""))
    }
    out
}

fn raw_text(n: Option<Node>) -> String {
    n.map(|n| text(n).trim().to_string()).unwrap_or_default()
}

fn attr(n: Node, name: &str) -> Option<String> {
    n.attributes()
        .find(|a| a.name() == name)
        .map(|a| a.value().to_string())
}

fn ident(n: Node) -> String {
    let s = text(n);
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    if let Some(cmd) = symbol(s) {
        return cmd.into();
    }
    if s.chars().count() == 1 {
        escape_text(s)
    } else {
        format!("\\mathrm{{{}}}", escape_text(s))
    }
}

fn operator(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    match symbol(s) {
        Some(cmd) => cmd.into(),
        None => escape_text(s),
    }
}

fn fraction(n: Node) -> String {
    let mut it = n.children().filter(Node::is_element);
    let num = it.next().map(arg).unwrap_or_default();
    let den = it.next().map(arg).unwrap_or_default();
    format!("\\frac{{{}}}{{{}}}", num.trim(), den.trim())
}

fn root(n: Node) -> String {
    let mut it = n.children().filter(Node::is_element);
    let base = it.next().map(arg).unwrap_or_default();
    let deg = it.next().map(arg).unwrap_or_default();
    let (base, deg) = (base.trim(), deg.trim());
    if deg.is_empty() {
        format!("\\sqrt{{{base}}}")
    } else {
        format!("\\sqrt[{deg}]{{{base}}}")
    }
}

fn script(n: Node, sup: bool) -> String {
    let mut it = n.children().filter(Node::is_element);
    let base = it.next().map(arg).unwrap_or_default();
    let arg = it.next().map(arg).unwrap_or_default();
    let base = atom(&base);
    let arg = arg.trim();
    if arg.is_empty() {
        return base;
    }
    if sup {
        format!("{base}^{{{arg}}}")
    } else {
        format!("{base}_{{{arg}}}")
    }
}

fn subsup(n: Node) -> String {
    let mut it = n.children().filter(Node::is_element);
    let base = it.next().map(arg).unwrap_or_default();
    let sub = it.next().map(arg).unwrap_or_default();
    let sup = it.next().map(arg).unwrap_or_default();
    let mut out = atom(&base);
    for (marker, arg) in [('_', &sub), ('^', &sup)] {
        let arg = arg.trim();
        if !arg.is_empty() {
            out.push_str(&format!("{marker}{{{arg}}}"))
        }
    }
    out
}

fn under(n: Node) -> String {
    let mut it = n.children().filter(Node::is_element);
    let base_node = it.next();
    let sub = it.next().map(arg).unwrap_or_default();
    let base = base_node.map(arg).unwrap_or_default();
    if let Some(op) = big(&raw_text(base_node)).or_else(|| big(base.trim())) {
        return with_scripts(op, Some(&sub), None);
    }
    format!("\\underset{{{}}}{{{}}}", sub.trim(), base.trim())
}

fn over(n: Node) -> String {
    let mut it = n.children().filter(Node::is_element);
    let base_node = it.next();
    let sup_node = it.next();
    let base = base_node.map(arg).unwrap_or_default();
    let sup = sup_node.map(arg).unwrap_or_default();
    if let Some(op) = big(&raw_text(base_node)).or_else(|| big(base.trim())) {
        return with_scripts(op, None, Some(&sup));
    }
    if let Some(cmd) = accent(&raw_text(sup_node)) {
        return format!("{cmd}{{{}}}", base.trim());
    }
    format!("\\overset{{{}}}{{{}}}", sup.trim(), base.trim())
}

fn under_over(n: Node) -> String {
    let mut it = n.children().filter(Node::is_element);
    let base_node = it.next();
    let sub = it.next().map(arg).unwrap_or_default();
    let sup = it.next().map(arg).unwrap_or_default();
    let base = base_node.map(arg).unwrap_or_default();
    if let Some(op) = big(&raw_text(base_node)).or_else(|| big(base.trim())) {
        return with_scripts(op, Some(&sub), Some(&sup));
    }
    format!(
        "\\overset{{{}}}{{\\underset{{{}}}{{{}}}}}",
        sup.trim(),
        sub.trim(),
        base.trim()
    )
}

fn with_scripts(op: &str, sub: Option<&str>, sup: Option<&str>) -> String {
    let mut out = op.to_string();
    if let Some(s) = sub.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str(&format!("_{{{s}}}"))
    }
    if let Some(s) = sup.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str(&format!("^{{{s}}}"))
    }
    out
}

fn fenced(n: Node) -> String {
    let open = attr(n, "open").unwrap_or_else(|| "(".into());
    let close = attr(n, "close").unwrap_or_else(|| ")".into());
    let seps: Vec<char> = attr(n, "separators")
        .unwrap_or_else(|| ",".into())
        .chars()
        .collect();
    let mut parts = Vec::new();
    for c in n.children().filter(Node::is_element) {
        let s = conv(c);
        let s = s.trim();
        if !s.is_empty() {
            parts.push(s.to_string())
        }
    }
    let mut body = String::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            let sep = seps.get(i - 1).or(seps.last()).copied().unwrap_or(',');
            body.push_str(&delim(&sep.to_string()));
            body.push(' ');
        }
        body.push_str(part)
    }
    let (l, r) = (delim(&open), delim(&close));
    if body.is_empty() {
        format!("\\left{l}\\right{r}")
    } else {
        format!("\\left{l} {body} \\right{r}")
    }
}

fn delim(s: &str) -> String {
    match s {
        "(" => "(".to_string(),
        ")" => ")".to_string(),
        "[" => "[".to_string(),
        "]" => "]".to_string(),
        "{" => "\\{".to_string(),
        "}" => "\\}".to_string(),
        "|" => "|".to_string(),
        "‖" | "∥" => "\\|".to_string(),
        "<" | "⟨" => "\\langle".to_string(),
        ">" | "⟩" => "\\rangle".to_string(),
        "." | "" => ".".to_string(),
        _ => escape_text(s),
    }
}

fn table(n: Node) -> String {
    let mut body = String::new();
    let mut rows = 0usize;
    for r in n.descendants().filter(|c| {
        c.is_element() && matches!(c.tag_name().name(), "mtr" | "mlabeledtr")
    }) {
        let mut cells = Vec::new();
        for c in r.children().filter(|c| {
            c.is_element() && matches!(c.tag_name().name(), "mtd" | "mlabeledtd")
        }) {
            cells.push(conv(c).trim().to_string())
        }
        if rows > 0 {
            body.push_str(" \\\\ ")
        }
        body.push_str(&cells.join(" & "));
        rows += 1;
    }
    if rows == 0 {
        return String::new();
    }
    format!("\\begin{{aligned}} {body} \\end{{aligned}}")
}

fn enclosed(n: Node) -> String {
    let body = seq(n);
    let body = body.trim();
    let notation = attr(n, "notation").unwrap_or_default();
    if notation.split_whitespace().any(|v| v == "radical") {
        format!("\\sqrt{{{body}}}")
    } else if notation
        .split_whitespace()
        .any(|v| matches!(v, "box" | "roundedbox" | "circle"))
    {
        format!("\\boxed{{{body}}}")
    } else {
        body.to_string()
    }
}

/// Braces around a script base unless it is already one atomic token.
fn atom(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || is_atom(s) {
        s.to_string()
    } else {
        format!("{{{s}}}")
    }
}

fn is_atom(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('\\') else {
        return s.chars().count() == 1;
    };
    let Some(first) = rest.chars().next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        // Escaped symbol such as `\{`.
        return rest.chars().count() == 1;
    }
    let name = rest.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    let mut depth = 0usize;
    for c in rest[name..].chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            '}' => return false,
            _ if depth == 0 => return false,
            _ => {}
        }
    }
    depth == 0
}

/// Big operators whose limits are typeset as `_{...}^{...}`.
fn big(s: &str) -> Option<&'static str> {
    Some(match s {
        "\\sum" | "∑" => "\\sum",
        "\\prod" | "∏" => "\\prod",
        "\\int" | "∫" => "\\int",
        "\\iint" | "∬" => "\\iint",
        "\\oint" | "∮" => "\\oint",
        "\\bigcup" | "⋃" => "\\bigcup",
        "\\bigcap" | "⋂" => "\\bigcap",
        "\\bigvee" | "⋁" => "\\bigvee",
        "\\bigwedge" | "⋀" => "\\bigwedge",
        "\\lim" | "lim" => "\\lim",
        "\\max" | "max" => "\\max",
        "\\min" | "min" => "\\min",
        "\\sup" | "sup" => "\\sup",
        "\\inf" | "inf" => "\\inf",
        _ => return None,
    })
}

/// Accents written as a mover script.
fn accent(s: &str) -> Option<&'static str> {
    Some(match s {
        "^" | "ˆ" => "\\hat",
        "¯" | "‾" | "―" | "—" => "\\bar",
        "˜" | "~" => "\\tilde",
        "→" | "⃗" => "\\vec",
        "˙" => "\\dot",
        "¨" => "\\ddot",
        _ => return None,
    })
}

fn symbol(s: &str) -> Option<&'static str> {
    Some(match s {
        "×" => "\\times",
        "·" | "⋅" => "\\cdot",
        "÷" => "\\div",
        "±" => "\\pm",
        "∓" => "\\mp",
        "≤" | "⩽" => "\\leq",
        "≥" | "⩾" => "\\geq",
        "≠" => "\\neq",
        "≈" => "\\approx",
        "≡" => "\\equiv",
        "∼" | "~" => "\\sim",
        "≪" => "\\ll",
        "≫" => "\\gg",
        "∝" => "\\propto",
        "∞" => "\\infty",
        "∑" => "\\sum",
        "∏" => "\\prod",
        "∫" => "\\int",
        "∬" => "\\iint",
        "∭" => "\\iiint",
        "∮" => "\\oint",
        "⋃" => "\\bigcup",
        "⋂" => "\\bigcap",
        "⋁" => "\\bigvee",
        "⋀" => "\\bigwedge",
        "√" => "\\sqrt",
        "∂" => "\\partial",
        "∇" => "\\nabla",
        "∈" => "\\in",
        "∉" => "\\notin",
        "∋" => "\\ni",
        "⊂" => "\\subset",
        "⊆" => "\\subseteq",
        "⊃" => "\\supset",
        "⊇" => "\\supseteq",
        "∪" => "\\cup",
        "∩" => "\\cap",
        "∅" => "\\emptyset",
        "∀" => "\\forall",
        "∃" => "\\exists",
        "¬" => "\\neg",
        "∧" => "\\wedge",
        "∨" => "\\vee",
        "⊕" => "\\oplus",
        "⊗" => "\\otimes",
        "→" => "\\to",
        "⇒" => "\\Rightarrow",
        "⇔" => "\\Leftrightarrow",
        "↔" => "\\leftrightarrow",
        "←" => "\\leftarrow",
        "⇐" => "\\Leftarrow",
        "↦" => "\\mapsto",
        "∴" => "\\therefore",
        "∵" => "\\because",
        "∠" => "\\angle",
        "⊥" => "\\perp",
        "∥" | "‖" => "\\parallel",
        "∘" => "\\circ",
        "°" => "^{\\circ}",
        "°C" => "^{\\circ}\\mathrm{C}",
        "ℓ" => "\\ell",
        "ℏ" => "\\hbar",
        "′" => "'",
        "″" => "''",
        "−" | "–" => "-",
        "…" => "\\ldots",
        "⋯" => "\\cdots",
        "∗" => "\\ast",
        "∣" => "\\mid",
        "ℝ" => "\\mathbb{R}",
        "ℤ" => "\\mathbb{Z}",
        "ℕ" => "\\mathbb{N}",
        "ℚ" => "\\mathbb{Q}",
        "ℂ" => "\\mathbb{C}",
        "α" => "\\alpha",
        "β" => "\\beta",
        "γ" => "\\gamma",
        "δ" => "\\delta",
        "ε" => "\\varepsilon",
        "ϵ" => "\\epsilon",
        "ζ" => "\\zeta",
        "η" => "\\eta",
        "θ" => "\\theta",
        "ϑ" => "\\vartheta",
        "ι" => "\\iota",
        "κ" => "\\kappa",
        "λ" => "\\lambda",
        "μ" => "\\mu",
        "ν" => "\\nu",
        "ξ" => "\\xi",
        "ο" => "o",
        "π" => "\\pi",
        "ϖ" => "\\varpi",
        "ρ" => "\\rho",
        "ϱ" => "\\varrho",
        "σ" => "\\sigma",
        "ς" => "\\varsigma",
        "τ" => "\\tau",
        "υ" => "\\upsilon",
        "φ" => "\\varphi",
        "ϕ" => "\\phi",
        "χ" => "\\chi",
        "ψ" => "\\psi",
        "ω" => "\\omega",
        "Γ" => "\\Gamma",
        "Δ" => "\\Delta",
        "Θ" => "\\Theta",
        "Λ" => "\\Lambda",
        "Ξ" => "\\Xi",
        "Π" => "\\Pi",
        "Σ" => "\\Sigma",
        "Υ" => "\\Upsilon",
        "Φ" => "\\Phi",
        "Ψ" => "\\Psi",
        "Ω" => "\\Omega",
        "Α" => "A",
        "Β" => "B",
        "Ε" => "E",
        "Ζ" => "Z",
        "Η" => "H",
        "Ι" => "I",
        "Κ" => "K",
        "Μ" => "M",
        "Ν" => "N",
        "Ο" => "O",
        "Ρ" => "P",
        "Τ" => "T",
        "Χ" => "X",
        "lim" => "\\lim",
        "max" => "\\max",
        "min" => "\\min",
        "sup" => "\\sup",
        "inf" => "\\inf",
        _ => return None,
    })
}

/// Escape the characters that are special in LaTeX text.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\backslash{}"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '#' | '$' | '%' | '&' | '_' => {
                out.push('\\');
                out.push(c)
            }
            '^' => out.push_str("\\textasciicircum{}"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::from_mathml;

    fn conv(xml: &str) -> String {
        from_mathml(xml).expect("converted LaTeX")
    }

    #[test]
    fn fractions() {
        assert_eq!(
            conv("<math><mfrac><mi>a</mi><mi>b</mi></mfrac></math>"),
            "\\frac{a}{b}"
        );
        assert_eq!(
            conv("<math><mfrac><mrow><mi>a</mi><mo>+</mo><mi>b</mi></mrow><mn>2</mn></mfrac></math>"),
            "\\frac{a + b}{2}"
        );
    }

    #[test]
    fn roots() {
        assert_eq!(conv("<math><msqrt><mi>x</mi></msqrt></math>"), "\\sqrt{x}");
        assert_eq!(
            conv("<math><mroot><mi>x</mi><mn>3</mn></mroot></math>"),
            "\\sqrt[3]{x}"
        );
    }

    #[test]
    fn scripts() {
        assert_eq!(
            conv("<math><msup><mi>x</mi><mn>2</mn></msup></math>"),
            "x^{2}"
        );
        assert_eq!(
            conv("<math><msub><mi>x</mi><mn>1</mn></msub></math>"),
            "x_{1}"
        );
        assert_eq!(
            conv("<math><msubsup><mi>x</mi><mi>i</mi><mn>2</mn></msubsup></math>"),
            "x_{i}^{2}"
        );
        assert_eq!(
            conv("<math><msup><mrow><mi>a</mi><mi>b</mi></mrow><mn>2</mn></msup></math>"),
            "{a b}^{2}"
        );
    }

    #[test]
    fn sum_limits() {
        assert_eq!(
            conv("<math><munderover><mo>∑</mo><mrow><mi>k</mi><mo>=</mo><mn>1</mn></mrow><mi>n</mi></munderover></math>"),
            "\\sum_{k = 1}^{n}"
        );
    }

    #[test]
    fn integral_limits() {
        assert_eq!(
            conv("<math><msubsup><mo>∫</mo><mn>0</mn><mn>1</mn></msubsup></math>"),
            "\\int_{0}^{1}"
        );
        assert_eq!(
            conv("<math><munder><mo>∫</mo><mi>S</mi></munder></math>"),
            "\\int_{S}"
        );
    }

    #[test]
    fn greek() {
        assert_eq!(conv("<math><mi>α</mi></math>"), "\\alpha");
        assert_eq!(
            conv("<math><msub><mi>θ</mi><mi>i</mi></msub></math>"),
            "\\theta_{i}"
        );
    }

    #[test]
    fn relations() {
        assert_eq!(
            conv("<math><mi>x</mi><mo>≤</mo><mn>1</mn></math>"),
            "x \\leq 1"
        );
        assert_eq!(
            conv("<math><mn>2</mn><mo>×</mo><mn>3</mn></math>"),
            "2 \\times 3"
        );
        assert_eq!(conv("<math><mo>°</mo></math>"), "^{\\circ}");
    }

    #[test]
    fn text_and_identifiers() {
        assert_eq!(conv("<math><mtext>if</mtext></math>"), "\\text{if}");
        assert_eq!(conv("<math><mi>sin</mi></math>"), "\\mathrm{sin}");
        assert_eq!(conv("<math><mi>x</mi></math>"), "x");
        assert_eq!(conv("<math><mspace/></math>"), "\\,");
    }

    #[test]
    fn fenced() {
        assert_eq!(
            conv("<math><mfenced open=\"[\" close=\"]\"><mi>x</mi><mo>+</mo><mn>1</mn></mfenced></math>"),
            "\\left[ x, +, 1 \\right]"
        );
        assert_eq!(
            conv("<math><mfenced><mi>a</mi><mi>b</mi></mfenced></math>"),
            "\\left( a, b \\right)"
        );
    }

    #[test]
    fn tables() {
        assert_eq!(
            conv("<math><mtable><mtr><mtd><mi>a</mi></mtd><mtd><mi>b</mi></mtd></mtr><mtr><mtd><mi>c</mi></mtd><mtd><mi>d</mi></mtd></mtr></mtable></math>"),
            "\\begin{aligned} a & b \\\\ c & d \\end{aligned}"
        );
    }

    #[test]
    fn accents_and_unknown_elements() {
        assert_eq!(
            conv("<math><mover><mi>x</mi><mo>¯</mo></mover></math>"),
            "\\bar{x}"
        );
        assert_eq!(conv("<math><foo><mi>y</mi></foo></math>"), "y");
        assert_eq!(
            conv("<math><semantics><mi>z</mi><annotation>x</annotation></semantics></math>"),
            "z"
        );
    }

    #[test]
    fn invalid_and_empty() {
        assert_eq!(from_mathml(""), None);
        assert_eq!(from_mathml("<math></math>"), None);
        assert_eq!(from_mathml("<mrow>"), None);
    }
}
