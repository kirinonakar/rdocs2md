use crate::{model::Run, package::*, render::escape};

/// Only presentation MathML elements are serialized. Embedded HTML, links,
/// event attributes and alternate executable content never enter the output.
fn presentation(n: N, out: &mut String) {
    if n.is_text() {
        out.push_str(&escape(n.text().unwrap_or("")));
        return;
    }
    let name = tag(n);
    if matches!(name, "annotation-xml" | "annotation" | "script" | "style") {
        return;
    }
    if name == "semantics" {
        if let Some(c) = n.first_element_child() {
            presentation(c, out)
        }
        return;
    }
    let elem = match name {
        "math" | "mi" | "mn" | "mo" | "mtext" | "mfrac" | "msqrt" | "mroot" | "msup" | "msub"
        | "msubsup" | "mrow" | "mtable" | "mtr" | "mtd" | "mover" | "munder" | "munderover" => name,
        "oMath" => "math",
        "f" => "mfrac",
        "sSup" => "msup",
        "sSub" => "msub",
        "sSubSup" => "msubsup",
        "t" => "mtext",
        "rad" if child(n, "deg").is_some_and(|d| !all_text(d).is_empty()) => "mroot",
        "rad" => "msqrt",
        "num" | "den" | "e" | "deg" | "sup" | "sub" => "mrow",
        _ => "",
    };
    if !elem.is_empty() {
        out.push('<');
        out.push_str(elem);
        out.push('>')
    }
    if name == "rad" {
        if let Some(base) = child(n, "e") {
            presentation(base, out)
        }
        if elem == "mroot"
            && let Some(deg) = child(n, "deg")
        {
            presentation(deg, out)
        }
    } else {
        for c in n.children() {
            presentation(c, out)
        }
    }
    if !elem.is_empty() {
        out.push_str("</");
        out.push_str(elem);
        out.push('>')
    }
}
pub fn formula(n: N) -> Run {
    let mut math = String::new();
    presentation(n, &mut math);
    Run {
        mathml: Some(math),
        text: all_text(n),
        ..Run::default()
    }
}
