use super::relationships;
use crate::{flow::Flow, formatting, model::*, package::*};
use std::collections::HashMap;

pub(super) type Palette = HashMap<String, String>;
type Matrix = [f64; 6];
pub(super) const IDENTITY: Matrix = [1., 0., 0., 1., 0., 0.];

pub(super) fn palette(pkg: &mut Package, path: Option<&String>) -> Result<Palette> {
    let Some(path) = path else {
        return Ok(Palette::new());
    };
    let source = pkg.text(path)?;
    let document = xml(&source)?;
    let mut palette = Palette::new();
    if let Some(scheme) = desc(document.root(), "clrScheme").next() {
        for color in scheme.children().filter(|n| n.is_element()) {
            if let Some(value) = color.first_element_child().and_then(|n| {
                attr(n, "lastClr")
                    .or_else(|| attr(n, "val"))
                    .and_then(formatting::color)
            }) {
                palette.insert(tag(color).into(), value);
            }
        }
    }
    Ok(palette)
}

fn color(node: N, palette: &Palette) -> Option<String> {
    let value = node.first_element_child()?;
    match tag(value) {
        "srgbClr" => formatting::color(attr(value, "val")?),
        "schemeClr" => {
            let key = match attr(value, "val")? {
                "bg1" => "lt1",
                "bg2" => "lt2",
                "tx1" => "dk1",
                "tx2" => "dk2",
                key => key,
            };
            palette.get(key).cloned()
        }
        _ => None,
    }
}

pub(super) fn background(root: N, palette: &Palette) -> Option<String> {
    let bg = child(child(root, "cSld")?, "bg")?;
    child(bg, "bgPr")
        .and_then(|p| child(p, "solidFill"))
        .or_else(|| child(bg, "bgRef"))
        .and_then(|p| color(p, palette))
}

#[derive(Clone)]
struct Placement {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    angle: f64,
    flip_x: bool,
    flip_y: bool,
}

impl Placement {
    fn read(n: N) -> Option<Self> {
        let (x, y) = pair(child(n, "off")?, "x", "y")?;
        let (w, h) = pair(child(n, "ext")?, "cx", "cy")?;
        if w < 0. || h < 0. {
            return None;
        }
        Some(Self {
            x,
            y,
            w,
            h,
            angle: formatting::units(attr(n, "rot"), 60000.)
                .filter(|value| value.is_finite())
                .unwrap_or_default()
                .to_radians(),
            flip_x: matches!(attr(n, "flipH"), Some("1" | "true")),
            flip_y: matches!(attr(n, "flipV"), Some("1" | "true")),
        })
    }

    fn matrix(&self, orient: bool) -> Matrix {
        if !orient {
            return [1., 0., 0., 1., self.x, self.y];
        }
        let (sin, cos) = self.angle.sin_cos();
        let sx = if self.flip_x { -1. } else { 1. };
        let sy = if self.flip_y { -1. } else { 1. };
        let (a, b, c, d) = (cos * sx, sin * sx, -sin * sy, cos * sy);
        let (cx, cy) = (self.w / 2., self.h / 2.);
        [
            a,
            b,
            c,
            d,
            self.x + cx - a * cx - c * cy,
            self.y + cy - b * cx - d * cy,
        ]
    }
}

fn pair(n: N, a: &str, b: &str) -> Option<(f64, f64)> {
    let read = |key| formatting::units(attr(n, key), 9525.).filter(|value| value.abs() <= 20000.);
    Some((read(a)?, read(b)?))
}

fn compose(left: Matrix, right: Matrix) -> Matrix {
    let [a, b, c, d, e, f] = left;
    let [g, h, i, j, k, l] = right;
    [
        a * g + c * h,
        b * g + d * h,
        a * i + c * j,
        b * i + d * j,
        a * k + c * l + e,
        b * k + d * l + f,
    ]
}

fn transform(n: N) -> Option<N> {
    child(n, "spPr")
        .or_else(|| child(n, "grpSpPr"))
        .and_then(|p| child(p, "xfrm"))
        .or_else(|| child(n, "xfrm"))
}

fn placeholder(n: N<'_>) -> Option<(&str, &str)> {
    let ph = desc(n, "ph").next()?;
    let kind = match attr(ph, "type") {
        Some("ctrTitle") => "title",
        Some(kind) => kind,
        None => "obj",
    };
    Some((kind, attr(ph, "idx").unwrap_or("0")))
}

#[derive(Default)]
pub(super) struct Template {
    placeholders: Vec<(String, String, Placement, SlideText)>,
    pub background: Option<String>,
}

impl Template {
    fn lookup(
        &self,
        (kind, index): (&str, &str),
    ) -> Option<&(String, String, Placement, SlideText)> {
        self.placeholders
            .iter()
            .rev()
            .find(|(k, i, _, _)| k == kind && i == index)
            .or_else(|| {
                self.placeholders
                    .iter()
                    .rev()
                    .find(|(k, i, _, _)| i == index && k != "title" && kind != "title")
            })
            .or_else(|| {
                self.placeholders
                    .iter()
                    .rev()
                    .find(|(k, _, _, _)| k == kind)
            })
    }

    pub(super) fn read(pkg: &mut Package, path: &str, palette: &Palette) -> Result<Self> {
        let (_, types) = relationships(pkg, path)?;
        let mut template = Self::default();
        for source in types
            .get("slideMaster")
            .map(String::as_str)
            .into_iter()
            .chain([path])
        {
            let source = pkg.text(source)?;
            let tree = xml(&source)?;
            let root = tree.root_element();
            template.background = background(root, palette).or(template.background);
            for n in child(root, "cSld")
                .and_then(|n| child(n, "spTree"))
                .into_iter()
                .flat_map(|n| n.children())
            {
                if let Some(key) = placeholder(n) {
                    let inherited = template.lookup(key);
                    if let Some(p) = transform(n)
                        .and_then(Placement::read)
                        .or_else(|| inherited.map(|(_, _, p, _)| p.clone()))
                    {
                        let text = text_style(n, inherited.map(|(_, _, _, t)| t), false);
                        template
                            .placeholders
                            .push((key.0.into(), key.1.into(), p, text));
                    }
                }
            }
        }
        Ok(template)
    }
}

fn text_style(n: N, inherited: Option<&SlideText>, unpadded: bool) -> SlideText {
    let title = placeholder(n).is_some_and(|(kind, _)| kind == "title");
    let mut style = inherited.cloned().unwrap_or(SlideText {
        size: if title { 36. } else { 24. },
        padding: [9.6, 4.8, 9.6, 4.8],
        vertical: "start",
    });
    if unpadded {
        style.padding = [0.; 4];
        return style;
    }
    if let Some(body) = child(n, "txBody") {
        if let Some(size) = child(body, "lstStyle")
            .and_then(|s| child(s, "lvl1pPr"))
            .and_then(|p| child(p, "defRPr"))
            .and_then(|p| formatting::units(attr(p, "sz"), 75.))
            .filter(|value| (1. ..=1000.).contains(value))
        {
            style.size = size;
        }
        if let Some(p) = child(body, "bodyPr") {
            for (i, key) in ["lIns", "tIns", "rIns", "bIns"].into_iter().enumerate() {
                if let Some(value) = formatting::units(attr(p, key), 9525.)
                    .filter(|value| (0. ..=10000.).contains(value))
                {
                    style.padding[i] = value;
                }
            }
            style.vertical = match attr(p, "anchor") {
                Some("ctr") => "center",
                Some("b") => "end",
                Some(_) => "start",
                None => style.vertical,
            };
        }
    }
    style
}

fn shape(n: N, palette: &Palette) -> Option<SlideShape> {
    let props = child(n, "spPr")?;
    let kind = child(props, "prstGeom").and_then(|g| attr(g, "prst"))?;
    if !matches!(
        kind,
        "rect"
            | "roundRect"
            | "ellipse"
            | "line"
            | "triangle"
            | "rtTriangle"
            | "diamond"
            | "chevron"
    ) {
        return None;
    }
    let line = child(props, "ln");
    let style = child(n, "style");
    let fill = if child(props, "noFill").is_none() && kind != "line" {
        child(props, "solidFill")
            .or_else(|| {
                style
                    .and_then(|p| child(p, "fillRef"))
                    .filter(|r| attr(*r, "idx") != Some("0"))
            })
            .and_then(|n| color(n, palette))
    } else {
        None
    };
    let stroke = if !line.is_some_and(|p| child(p, "noFill").is_some()) {
        line.and_then(|p| child(p, "solidFill"))
            .or_else(|| {
                style
                    .and_then(|p| child(p, "lnRef"))
                    .filter(|r| attr(*r, "idx") != Some("0"))
            })
            .and_then(|n| color(n, palette))
            .or_else(|| (kind == "line").then(|| "#000000".into()))
            .map(|color| {
                (
                    color,
                    line.and_then(|p| formatting::units(attr(p, "w"), 9525.))
                        .filter(|v| (0. ..=1000.).contains(v))
                        .unwrap_or(1.),
                )
            })
    } else {
        None
    };
    (fill.is_some() || stroke.is_some()).then(|| SlideShape {
        kind: kind.into(),
        fill,
        stroke,
    })
}

pub(super) struct SlideReader<'a, 'b> {
    pub flow: &'a Flow<'b>,
    pub template: &'a Template,
    pub palette: &'a Palette,
}

impl SlideReader<'_, '_> {
    pub(super) fn collect(
        &self,
        n: N,
        parent: Matrix,
        blocks: &mut Vec<Block>,
        frames: &mut Vec<SlideFrame>,
    ) -> Result<()> {
        let xfrm = transform(n);
        let placement = xfrm.and_then(Placement::read);
        if tag(n) == "grpSp"
            && let Some(p) = &placement
            && let Some(xfrm) = xfrm
            && let Some((x, y)) = child(xfrm, "chOff").and_then(|n| pair(n, "x", "y"))
            && let Some((w, h)) = child(xfrm, "chExt").and_then(|n| pair(n, "cx", "cy"))
            && w > 0.
            && h > 0.
        {
            let (sx, sy) = (p.w / w, p.h / h);
            let matrix = compose(
                compose(parent, p.matrix(true)),
                [sx, 0., 0., sy, -x * sx, -y * sy],
            );
            if matrix.iter().all(|v| v.abs() <= 1e9) {
                for n in n.children().filter(|n| n.is_element()) {
                    self.collect(n, matrix, blocks, frames)?;
                }
                return Ok(());
            }
        }
        let start = blocks.len();
        self.flow.blocks(n, blocks)?;
        if !matches!(tag(n), "sp" | "pic" | "graphicFrame" | "cxnSp") {
            return Ok(());
        }
        let template = placeholder(n).and_then(|key| self.template.lookup(key));
        if let Some(p) = placement.or_else(|| template.map(|(_, _, p, _)| p.clone())) {
            let has_content = blocks[start..].iter().any(|b| match b {
                Block::Paragraph { runs, .. } => runs
                    .iter()
                    .any(|r| r.image.is_some() || r.mathml.is_some() || !r.text.trim().is_empty()),
                Block::Table { rows } => !rows.is_empty(),
            });
            let decoration = shape(n, self.palette);
            let place_content = has_content && p.w > 0. && p.h > 0.;
            if place_content || decoration.is_some() {
                frames.push(SlideFrame {
                    transform: compose(parent, p.matrix(tag(n) != "pic")),
                    size: [p.w, p.h],
                    blocks: start..if place_content { blocks.len() } else { start },
                    shape: decoration,
                    text: text_style(
                        n,
                        template.map(|(_, _, _, t)| t),
                        matches!(tag(n), "pic" | "graphicFrame"),
                    ),
                });
            }
        }
        Ok(())
    }
}
