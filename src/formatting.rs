use crate::{model::Properties, package::*};

pub const SIDES: [&str; 4] = ["left", "right", "top", "bottom"];
pub const BORDERS: [&str; 4] = ["border-left", "border-right", "border-top", "border-bottom"];

pub fn color(value: &str) -> Option<String> {
    if value.eq_ignore_ascii_case("white") {
        return Some("#ffffff".into());
    }
    let hex = value.strip_prefix('#').unwrap_or(value);
    (matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("#{hex}"))
}

pub fn length_in_points(value: &str) -> Option<f64> {
    let value = value.trim();
    if value == "0" {
        return Some(0.);
    }
    for (unit, scale) in [
        ("pt", 1.),
        ("px", 0.75),
        ("pc", 12.),
        ("mm", 72. / 25.4),
        ("cm", 72. / 2.54),
        ("in", 72.),
    ] {
        if let Some(number) = value.strip_suffix(unit) {
            return Some(number.trim().parse::<f64>().ok()? * scale).filter(|v| v.is_finite());
        }
    }
    None
}

pub fn length(value: &str, negative: bool) -> Option<String> {
    if value == "0" {
        return Some("0".into());
    }
    for unit in ["pt", "px", "mm", "cm", "in", "em", "%"] {
        if let Some(n) = value.strip_suffix(unit)
            && let Ok(n) = n.trim().parse::<f64>()
            && n.abs() <= 10000.
            && (negative || n >= 0.)
        {
            return Some(format!("{n}{unit}"));
        }
    }
    None
}

pub fn points(props: &mut Properties, key: &str, value: Option<f64>) {
    if let Some(n) = value.filter(|n| n.abs() <= 10000.) {
        props.insert(key.into(), format!("{n}pt"));
    }
}

pub fn units(value: Option<&str>, divisor: f64) -> Option<f64> {
    Some(value?.parse::<f64>().ok()? / divisor)
}

pub fn alignment(value: &str) -> Option<&'static str> {
    match value {
        "left" | "LEFT" | "start" | "l" => Some("left"),
        "right" | "RIGHT" | "end" | "r" => Some("right"),
        "center" | "CENTER" | "ctr" | "c" => Some("center"),
        "justify" | "JUSTIFY" | "both" | "distribute" | "DISTRIBUTE" | "just" => Some("justify"),
        _ => None,
    }
}

pub fn vertical(value: &str) -> Option<&'static str> {
    match value {
        "top" | "TOP" | "t" => Some("top"),
        "middle" | "center" | "CENTER" | "ctr" => Some("middle"),
        "bottom" | "BOTTOM" | "b" => Some("bottom"),
        _ => None,
    }
}

pub fn border(style: &str, width: &str, color: Option<&str>) -> String {
    if style == "none" {
        return "none".into();
    }
    format!("{width} {style} {}", color.unwrap_or("#000000"))
}

fn css_border(value: &str) -> Option<String> {
    let mut width = None;
    let mut shade = None;
    let mut style = None;
    for part in value.split_whitespace() {
        match part {
            "none" | "hidden" | "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge"
            | "inset" | "outset"
                if style.is_none() =>
            {
                style = Some(part)
            }
            "thin" | "medium" | "thick" if width.is_none() => width = Some(part.into()),
            "currentColor" | "transparent" if shade.is_none() => shade = Some(part.into()),
            _ if shade.is_none() && part.starts_with('#') && color(part).is_some() => {
                shade = color(part)
            }
            _ if width.is_none() => width = Some(length(part, false)?),
            _ => return None,
        }
    }
    Some(border(
        style?,
        width.as_deref().unwrap_or("medium"),
        shade.as_deref(),
    ))
}

pub fn box_property(props: &mut Properties, key: &str, value: &str) {
    let validated = match key {
        "border" | "border-left" | "border-right" | "border-top" | "border-bottom" => {
            css_border(value)
        }
        "background-color" => {
            color(value).or_else(|| (value == "transparent").then(|| value.into()))
        }
        "width" | "height" | "padding-left" | "padding-right" | "padding-top"
        | "padding-bottom" => length(value, false),
        "text-align" => alignment(value).map(str::to_owned),
        "vertical-align" => vertical(value).map(str::to_owned),
        "white-space" if matches!(value, "normal" | "nowrap" | "pre" | "pre-wrap") => {
            Some(value.into())
        }
        _ => None,
    };
    if let Some(v) = validated {
        if key == "border" {
            props.extend(BORDERS.map(|side| (side.into(), v.clone())));
        } else {
            props.insert(key.into(), v);
        }
    }
}

pub fn hwpx_border_fill(node: N) -> Properties {
    let mut props = Properties::new();
    for (side, key) in ["leftBorder", "rightBorder", "topBorder", "bottomBorder"]
        .into_iter()
        .zip(BORDERS)
    {
        let Some(edge) = child(node, side) else {
            continue;
        };
        let style = match attr(edge, "type").unwrap_or("") {
            "NONE" => "none",
            "DOT" | "CIRCLE" => "dotted",
            "DASH" | "LONG_DASH" | "DASH_DOT" | "DASH_DOT_DOT" | "WAVE" => "dashed",
            "DOUBLE_SLIM" | "SLIM_THICK" | "THICK_SLIM" | "SLIM_THICK_SLIM" | "DOUBLE_WAVE" => {
                "double"
            }
            "" => continue,
            _ => "solid",
        };
        let width = attr(edge, "width").and_then(|v| length(v, false));
        let shade = attr(edge, "color").and_then(color);
        props.insert(
            key.into(),
            border(
                style,
                width.as_deref().unwrap_or("0.12mm"),
                shade.as_deref(),
            ),
        );
    }
    if let Some(shade) = child(node, "fillBrush")
        .and_then(|brush| child(brush, "winBrush"))
        .and_then(|brush| attr(brush, "faceColor"))
        .and_then(color)
    {
        props.insert("background-color".into(), shade);
    }
    props
}

pub fn word_borders(node: N) -> Properties {
    let mut props = Properties::new();
    for edge in node.children().filter(|n| n.is_element()) {
        let key = match tag(edge) {
            "left" | "start" => "border-left",
            "right" | "end" => "border-right",
            "top" => "border-top",
            "bottom" => "border-bottom",
            "insideH" => "insideH",
            "insideV" => "insideV",
            _ => continue,
        };
        let style = match attr(edge, "val") {
            Some("none" | "nil") => "none",
            Some("dotted") => "dotted",
            Some("dashSmallGap" | "dashed" | "dotDash" | "dotDotDash" | "wave") => "dashed",
            Some("double" | "triple" | "doubleWave") => "double",
            Some(v) if v.contains("ThinThick") || v.contains("ThickThin") => "double",
            Some(_) => "solid",
            None => continue,
        };
        let size = units(attr(edge, "sz"), 8.)
            .filter(|n| (0.0..=96.).contains(n))
            .unwrap_or(0.5);
        let shade = attr(edge, "color").and_then(color);
        props.insert(
            key.into(),
            border(style, &format!("{size}pt"), shade.as_deref()),
        );
    }
    props
}

pub fn word_cell(props: &mut Properties, node: N) {
    if let Some(edges) = child(node, "tcBorders") {
        props.extend(
            word_borders(edges)
                .into_iter()
                .filter(|(key, _)| BORDERS.contains(&key.as_str())),
        );
    }
    if let Some(shade) = child(node, "shd").and_then(|shade| attr(shade, "fill")) {
        let shade = if shade == "auto" {
            "transparent"
        } else {
            shade
        };
        box_property(props, "background-color", shade);
    }
    if let Some(width) = child(node, "tcW").and_then(|w| {
        let value = attr(w, "w")?.parse::<u32>().ok()?;
        match attr(w, "type") {
            Some("dxa") => Some(format!("{}pt", value as f64 / 20.)),
            Some("pct") => Some(format!("{}%", value as f64 / 50.)),
            _ => None,
        }
    }) {
        box_property(props, "width", &width);
    }
    if let Some(v) = child(node, "vAlign").and_then(|n| attr(n, "val")) {
        box_property(props, "vertical-align", v);
    }
    if let Some(margins) = child(node, "tcMar").or_else(|| child(node, "tblCellMar")) {
        for (side, key) in SIDES.into_iter().zip([
            "padding-left",
            "padding-right",
            "padding-top",
            "padding-bottom",
        ]) {
            let edge = child(margins, side).or_else(|| match side {
                "left" => child(margins, "start"),
                "right" => child(margins, "end"),
                _ => None,
            });
            points(
                props,
                key,
                units(edge.and_then(|m| attr(m, "w")), 20.).filter(|n| *n >= 0.),
            );
        }
    }
}

pub fn word_paragraph(props: &mut Properties, node: N) {
    if let Some(v) = child(node, "jc").and_then(|n| attr(n, "val")) {
        props.insert("align".into(), v.into());
    }
    if let Some(indent) = child(node, "ind") {
        for (key, first, fallback) in [
            ("margin-left", "start", "left"),
            ("margin-right", "end", "right"),
        ] {
            points(
                props,
                key,
                units(attr(indent, first).or_else(|| attr(indent, fallback)), 20.),
            );
        }
        let first =
            units(attr(indent, "hanging"), -20.).or_else(|| units(attr(indent, "firstLine"), 20.));
        points(props, "text-indent", first);
    }
    if let Some(spacing) = child(node, "spacing") {
        points(props, "margin-top", units(attr(spacing, "before"), 20.));
        points(props, "margin-bottom", units(attr(spacing, "after"), 20.));
        let rule = attr(spacing, "lineRule").unwrap_or("auto");
        if rule == "exact" {
            points(props, "line-height", units(attr(spacing, "line"), 20.));
        } else if rule == "auto"
            && let Some(n) =
                units(attr(spacing, "line"), 2.4).filter(|n| (1.0..=1000.0).contains(n))
        {
            props.insert("line-height".into(), format!("{n}%"));
        }
    }
}

pub fn drawing_cell(props: &mut Properties, node: N) {
    let rgb = |parent| {
        child(parent, "solidFill")
            .and_then(|fill| child(fill, "srgbClr"))
            .and_then(|rgb| attr(rgb, "val"))
            .and_then(color)
    };
    for (name, key) in ["lnL", "lnR", "lnT", "lnB"].into_iter().zip(BORDERS) {
        let Some(edge) = child(node, name) else {
            continue;
        };
        let width = units(attr(edge, "w"), 12700.)
            .filter(|n| (0.0..=1000.0).contains(n))
            .unwrap_or(0.75);
        let style = if child(edge, "noFill").is_some() {
            "none"
        } else if matches!(
            attr(edge, "cmpd"),
            Some("dbl" | "thickThin" | "thinThick" | "tri")
        ) {
            "double"
        } else {
            match child(edge, "prstDash").and_then(|d| attr(d, "val")) {
                Some("dot" | "sysDot") => "dotted",
                Some(v) if v != "solid" => "dashed",
                _ => "solid",
            }
        };
        props.insert(
            key.into(),
            border(style, &format!("{width}pt"), rgb(edge).as_deref()),
        );
    }
    if let Some(shade) = rgb(node) {
        props.insert("background-color".into(), shade);
    }
    for (key, source) in [
        ("padding-left", "marL"),
        ("padding-right", "marR"),
        ("padding-top", "marT"),
        ("padding-bottom", "marB"),
    ] {
        points(
            props,
            key,
            units(attr(node, source), 12700.).filter(|n| *n >= 0.),
        );
    }
    if let Some(anchor) = attr(node, "anchor") {
        box_property(props, "vertical-align", anchor);
    }
}
