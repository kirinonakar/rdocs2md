use crate::model::*;
use encoding_rs::{Encoding, WINDOWS_1252};
use std::collections::HashMap;
#[derive(Clone, Copy, PartialEq)]
enum Dest {
    Body,
    Skip,
    Font,
    Color,
    Picture,
    Instruction,
}
#[derive(Clone)]
struct State {
    style: Style,
    dest: Dest,
    uc: usize,
    cp: u16,
    font: i32,
    link: Option<String>,
    align: Option<String>,
    indent: i32,
    depth: usize,
    list: bool,
    star: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            style: Style::default(),
            dest: Dest::Body,
            uc: 1,
            cp: 1252,
            font: 0,
            link: None,
            align: None,
            indent: 0,
            depth: 0,
            list: false,
            star: false,
        }
    }
}
fn encoding(cp: u16) -> &'static Encoding {
    match cp {
        932 => encoding_rs::SHIFT_JIS,
        936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        65001 => encoding_rs::UTF_8,
        n => Encoding::for_label(format!("windows-{n}").as_bytes()).unwrap_or(WINDOWS_1252),
    }
}
struct Parser {
    doc: Document,
    section: Section,
    state: State,
    stack: Vec<State>,
    buf: Vec<u8>,
    runs: Vec<Run>,
    rows: Vec<Vec<Cell>>,
    row: Vec<Cell>,
    font_cp: HashMap<i32, u16>,
    fonts: HashMap<i32, String>,
    font_name: Vec<u8>,
    colors: Vec<Option<String>>,
    rgb: Option<[u8; 3]>,
    fallback: usize,
    high: Option<u16>,
    picture: Vec<u8>,
    nibble: Option<u8>,
    images: bool,
    inst: String,
}
impl Parser {
    fn emit(&mut self, t: &str) {
        if self.state.dest == Dest::Instruction {
            if self.inst.len() < 65536 {
                self.inst.push_str(t)
            }
        } else if self.state.dest == Dest::Body {
            push_run(
                &mut self.runs,
                Run {
                    text: t.into(),
                    style: self.state.style.clone(),
                    link: self.state.link.clone(),
                    ..Run::default()
                },
            )
        }
    }
    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let buf = std::mem::take(&mut self.buf);
            let cp = self
                .font_cp
                .get(&self.state.font)
                .copied()
                .unwrap_or(self.state.cp);
            self.emit(&encoding(cp).decode_without_bom_handling(&buf).0)
        }
    }
    fn literal(&mut self, b: u8) -> Result<()> {
        if self.fallback > 0 {
            self.fallback -= 1;
            return Ok(());
        }
        match self.state.dest {
            Dest::Body | Dest::Instruction => self.buf.push(b),
            Dest::Font if b == b';' => {
                let name = encoding(self.state.cp)
                    .decode_without_bom_handling(&self.font_name)
                    .0
                    .trim()
                    .to_string();
                self.font_name.clear();
                self.fonts.insert(self.state.font, name);
            }
            Dest::Font if self.font_name.len() < 512 => self.font_name.push(b),
            Dest::Color if b == b';' => {
                if self.colors.len() >= 65536 {
                    return Err("RTF color table too large".into());
                }
                self.colors.push(
                    self.rgb
                        .take()
                        .map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}")),
                );
            }
            Dest::Picture if self.images && !b.is_ascii_whitespace() => {
                let hex = (b as char).to_digit(16).ok_or("invalid RTF picture hex")? as u8;
                if let Some(n) = self.nibble.take() {
                    if self.picture.len() >= PART_LIMIT {
                        return Err("RTF image too large".into());
                    }
                    self.picture.push(n * 16 + hex)
                } else {
                    self.nibble = Some(hex)
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn paragraph(&mut self) {
        self.flush();
        if !self.runs.is_empty() {
            let mut properties = Properties::new();
            if self.state.indent != 0 {
                properties.insert("left_indent_twips".into(), self.state.indent.to_string());
            }
            if let Some(a) = &self.state.align {
                properties.insert("align".into(), a.clone());
            }
            self.section.blocks.push(Block::Paragraph {
                runs: std::mem::take(&mut self.runs),
                heading: 0,
                list: self.state.list.then_some(List {
                    ordered: false,
                    depth: self.state.depth,
                }),
                properties,
            });
        }
    }
    fn table(&mut self) {
        if !self.rows.is_empty() {
            self.section.blocks.push(Block::Table {
                rows: std::mem::take(&mut self.rows),
            })
        }
    }
    fn close(&mut self) -> Result<()> {
        self.flush();
        if self.stack.len() == 1 {
            self.table();
            self.paragraph()
        }
        let prev = self.stack.pop().ok_or("unbalanced RTF closing brace")?;
        if self.state.dest == Dest::Picture && prev.dest != Dest::Picture {
            if self.nibble.is_some() {
                return Err("incomplete RTF picture".into());
            }
            let key = format!("rtf/picture-{}", self.doc.assets.len() + 1);
            if self.images && image_ext(&self.picture).is_some() {
                self.doc
                    .assets
                    .insert(key.clone(), std::mem::take(&mut self.picture));
                push_run(
                    &mut self.runs,
                    Run {
                        image: Some(key),
                        ..Run::default()
                    },
                )
            }
        }
        let link = if self.state.dest == Dest::Instruction && prev.dest != Dest::Instruction {
            let cmd = self.inst.trim();
            let (name, arg) = cmd.split_once(char::is_whitespace).unwrap_or(("", ""));
            (name.eq_ignore_ascii_case("HYPERLINK"))
                .then(|| arg.trim().trim_matches('"').to_owned())
        } else {
            None
        };
        self.state = prev;
        if let Some(l) = link {
            self.state.link = Some(l)
        }
        self.fallback = 0;
        Ok(())
    }
    fn word(&mut self, k: &str, arg: Option<i32>) -> Result<()> {
        self.flush();
        let n = arg.unwrap_or(1);
        if self.state.dest == Dest::Skip {
            return Ok(());
        }
        if self.state.star && !matches!(k, "fldinst" | "shppict") {
            self.state.dest = Dest::Skip;
        }
        self.state.star = false;
        match k {
            "fonttbl" => self.state.dest = Dest::Font,
            "colortbl" => self.state.dest = Dest::Color,
            "red" | "green" | "blue" if self.state.dest == Dest::Color => {
                self.rgb.get_or_insert([0; 3])[match k {
                    "red" => 0,
                    "green" => 1,
                    _ => 2,
                }] = n.clamp(0, 255) as u8
            }
            "stylesheet" | "info" | "filetbl" | "object" | "datastore" | "xmlnstbl"
            | "listtable" | "listoverridetable" | "nonshppict" | "themedata" | "generator"
            | "pntext" | "listtext" => self.state.dest = Dest::Skip,
            "fldinst" => {
                self.inst.clear();
                self.state.dest = Dest::Instruction
            }
            "pict" => {
                self.state.dest = Dest::Picture;
                self.picture.clear();
                self.nibble = None;
            }
            "f" => {
                self.state.font = n;
                if self.state.dest == Dest::Body {
                    self.state.style.font = self.fonts.get(&n).cloned()
                }
            }
            "fcharset" if self.state.dest == Dest::Font => {
                let cp = match n {
                    128 => 932,
                    129 => 949,
                    134 => 936,
                    136 => 950,
                    204 => 1251,
                    238 => 1250,
                    161 => 1253,
                    162 => 1254,
                    177 => 1255,
                    178 => 1256,
                    186 => 1257,
                    163 => 1258,
                    _ => self.state.cp,
                };
                self.font_cp.insert(self.state.font, cp);
            }
            "ansicpg" => self.state.cp = n.try_into()?,
            "uc" => {
                if !(0..=32).contains(&n) {
                    return Err("invalid RTF Unicode fallback length".into());
                }
                self.state.uc = n as usize
            }
            "u" => {
                let unit = n as u16;
                if (0xd800..=0xdbff).contains(&unit) {
                    self.high = Some(unit)
                } else {
                    let v = if let Some(h) = self.high.take() {
                        String::from_utf16_lossy(&[h, unit])
                    } else {
                        String::from_utf16_lossy(&[unit])
                    };
                    self.emit(&v)
                }
                self.fallback = self.state.uc
            }
            _ if self.state.dest != Dest::Body => {}
            "b" => self.state.style.bold = n != 0,
            "i" => self.state.style.italic = n != 0,
            "ul" => self.state.style.underline = n != 0,
            "ulnone" => self.state.style.underline = false,
            "strike" => self.state.style.strike = n != 0,
            "super" | "sub" => {
                self.state.style.script = Some(if k == "super" { "sup" } else { "sub" }.into())
            }
            "nosupersub" => self.state.style.script = None,
            "fs" => self.state.style.size = Some(format!("{}pt", n as f64 / 2.0)),
            "plain" => self.state.style = Style::default(),
            "ls" | "pn" => self.state.list = true,
            "ilvl" => self.state.depth = n.clamp(0, 32) as usize,
            "cf" => {
                self.state.style.color = usize::try_from(n)
                    .ok()
                    .and_then(|i| self.colors.get(i))
                    .cloned()
                    .flatten()
            }
            "li" => self.state.indent = n,
            "pard" => {
                self.state.indent = 0;
                self.state.align = None;
                self.state.list = false;
                self.state.depth = 0
            }
            "ql" | "qr" | "qc" | "qj" => {
                self.state.align = Some(
                    match k {
                        "ql" => "left",
                        "qr" => "right",
                        "qc" => "center",
                        _ => "justify",
                    }
                    .into(),
                )
            }
            "par" => {
                self.table();
                self.paragraph()
            }
            "page" | "sect" => {
                self.table();
                self.paragraph();
                let next = Section::new("page", format!("page {}", self.doc.sections.len() + 2));
                self.doc
                    .sections
                    .push(std::mem::replace(&mut self.section, next));
            }
            "trowd" => self.paragraph(),
            "cell" => self.row.push(Cell {
                runs: std::mem::take(&mut self.runs),
                ..Cell::default()
            }),
            "row" => {
                if !self.row.is_empty() {
                    self.rows.push(std::mem::take(&mut self.row))
                }
            }
            "line" => self.emit("\n"),
            "tab" => self.emit("\t"),
            "emdash" => self.emit("—"),
            "endash" => self.emit("–"),
            "bullet" => self.emit("•"),
            "lquote" => self.emit("‘"),
            "rquote" => self.emit("’"),
            "ldblquote" => self.emit("“"),
            "rdblquote" => self.emit("”"),
            _ => {}
        }
        Ok(())
    }
}
pub fn parse(data: &[u8], images: bool) -> Result<Document> {
    let mut p = Parser {
        doc: Document::new("rtf"),
        section: Section::new("page", "page 1"),
        state: State::default(),
        stack: vec![],
        buf: vec![],
        runs: vec![],
        rows: vec![],
        row: vec![],
        font_cp: HashMap::new(),
        fonts: HashMap::new(),
        font_name: vec![],
        colors: vec![],
        rgb: None,
        fallback: 0,
        high: None,
        picture: vec![],
        nibble: None,
        images,
        inst: String::new(),
    };
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            b'{' => {
                p.flush();
                if p.stack.len() >= 128 {
                    return Err("RTF group depth exceeds 128".into());
                }
                p.stack.push(p.state.clone());
                p.fallback = 0
            }
            b'}' => {
                p.close()?;
                if p.stack.is_empty() {
                    if data[i..]
                        .iter()
                        .any(|b| !b.is_ascii_whitespace() && *b != 0)
                    {
                        return Err("trailing data after RTF root".into());
                    }
                    break;
                }
            }
            b'\n' | b'\r' => {}
            b'\\' => {
                let ch = *data.get(i).ok_or("truncated RTF escape")?;
                i += 1;
                if ch.is_ascii_alphabetic() {
                    let start = i - 1;
                    while data.get(i).is_some_and(u8::is_ascii_alphabetic) {
                        i += 1
                    }
                    let k = std::str::from_utf8(&data[start..i])?;
                    let num = i;
                    if data.get(i) == Some(&b'-') {
                        i += 1
                    }
                    while data.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1
                    }
                    let arg = if i > num {
                        Some(std::str::from_utf8(&data[num..i])?.parse()?)
                    } else {
                        None
                    };
                    if data.get(i) == Some(&b' ') {
                        i += 1
                    }
                    if k == "bin" {
                        p.flush();
                        let size = usize::try_from(arg.ok_or("RTF bin needs length")?)?;
                        if size > PART_LIMIT || size > data.len() - i {
                            return Err("invalid RTF binary length".into());
                        }
                        if p.images && p.state.dest == Dest::Picture {
                            if p.picture.len() + size > PART_LIMIT {
                                return Err("RTF picture too large".into());
                            }
                            p.picture.extend_from_slice(&data[i..i + size])
                        }
                        i += size
                    } else {
                        p.word(k, arg)?
                    }
                } else {
                    match ch {
                        b'\'' => {
                            let digits = data.get(i..i + 2).ok_or("truncated RTF hex escape")?;
                            i += 2;
                            p.literal(u8::from_str_radix(std::str::from_utf8(digits)?, 16)?)?
                        }
                        b'*' => p.state.star = true,
                        b'{' | b'}' | b'\\' => p.literal(ch)?,
                        b'~' | b'_' | b'-' => {
                            if p.fallback > 0 {
                                p.fallback -= 1
                            } else {
                                p.flush();
                                p.emit(match ch {
                                    b'~' => "\u{a0}",
                                    b'_' => "‑",
                                    _ => "\u{ad}",
                                })
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => p.literal(b)?,
        }
    }
    if !p.stack.is_empty() {
        return Err("unclosed RTF group".into());
    }
    p.table();
    p.paragraph();
    p.doc.sections.push(p.section);
    Ok(p.doc)
}
