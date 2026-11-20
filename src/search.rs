use crate::model::*;
use serde::Serialize;
use std::borrow::Cow;

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;
const CONTEXT_CHARS: usize = 60;

pub struct Query {
    needle: String,
    case_sensitive: bool,
    limit: usize,
}

impl Query {
    pub fn new(query: &str, case_sensitive: bool, limit: usize) -> Result<Self> {
        if query.trim().is_empty() || query.chars().take(1025).count() > 1024 {
            return Err(
                "search query must contain 1 to 1024 characters and cannot be blank".into(),
            );
        }
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(format!("search limit must be between 1 and {MAX_LIMIT}").into());
        }
        Ok(Self {
            needle: if case_sensitive {
                query.into()
            } else {
                lowercase(query)
            },
            case_sensitive,
            limit,
        })
    }

    pub fn find<'a>(&self, doc: &'a Document) -> SearchResult<'a> {
        let mut matches = Vec::new();
        let mut path = Vec::new();
        for (i, section) in doc.sections.iter().enumerate() {
            let mut traversal = Search {
                query: self,
                section,
                index: section.index.unwrap_or(i + 1),
                matches: &mut matches,
            };
            if traversal.blocks(&section.blocks, &mut path, None) {
                return SearchResult {
                    format: &doc.format,
                    warnings: &doc.warnings,
                    matches,
                    truncated: true,
                };
            }
        }
        SearchResult {
            format: &doc.format,
            warnings: &doc.warnings,
            matches,
            truncated: false,
        }
    }
}

#[derive(Serialize)]
pub struct SearchResult<'a> {
    format: &'a str,
    warnings: &'a [String],
    matches: Vec<Match<'a>>,
    truncated: bool,
}

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
enum Step {
    Block { block: usize },
    Cell { row: usize, column: usize },
}

#[derive(Serialize)]
struct Match<'a> {
    section_index: usize,
    section_kind: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    section_title: &'a str,
    source: &'a str,
    path: Vec<Step>,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    x_pt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y_pt: Option<f64>,
    start_char: usize,
    end_char: usize,
    before: String,
    text: String,
    after: String,
}

struct Search<'a, 'b> {
    query: &'b Query,
    section: &'a Section,
    index: usize,
    matches: &'b mut Vec<Match<'a>>,
}

impl<'a> Search<'a, '_> {
    fn blocks(
        &mut self,
        blocks: &'a [Block],
        path: &mut Vec<Step>,
        address: Option<&'a str>,
    ) -> bool {
        for (i, block) in blocks.iter().enumerate() {
            path.push(Step::Block { block: i + 1 });
            match block {
                Block::Paragraph {
                    runs, properties, ..
                } => {
                    if self.runs(runs, properties, path, address) {
                        return true;
                    }
                }
                Block::Table { rows } => {
                    for (r, row) in rows.iter().enumerate() {
                        for (c, cell) in row.iter().enumerate() {
                            path.push(Step::Cell {
                                row: r + 1,
                                column: c + 1,
                            });
                            let address = cell
                                .properties
                                .get("address")
                                .map(String::as_str)
                                .or(address);
                            if self.runs(&cell.runs, &cell.properties, path, address)
                                || self.blocks(&cell.blocks, path, address)
                            {
                                return true;
                            }
                            path.pop();
                        }
                    }
                }
            }
            path.pop();
        }
        false
    }

    fn runs(
        &mut self,
        runs: &[Run],
        props: &'a Properties,
        path: &[Step],
        address: Option<&'a str>,
    ) -> bool {
        let text: Cow<'_, str> = match runs {
            [] => return false,
            [run] => Cow::Borrowed(&run.text),
            _ => Cow::Owned(runs.iter().map(|run| run.text.as_str()).collect()),
        };
        let haystack = if self.query.case_sensitive {
            Cow::Borrowed(text.as_ref())
        } else if text.is_ascii() {
            if text.bytes().any(|b| b.is_ascii_uppercase()) {
                Cow::Owned(text.to_ascii_lowercase())
            } else {
                Cow::Borrowed(text.as_ref())
            }
        } else {
            Cow::Owned(lowercase(&text))
        };
        let mut chars = text.chars();
        let mut offsets = Offsets::default();
        let coord = |key| {
            props
                .get(key)
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite())
        };
        for (start, matched) in haystack.match_indices(&self.query.needle) {
            let end = start + matched.len();
            let (start_byte, start_char) =
                offsets.advance(start, &mut chars, self.query.case_sensitive);
            let start_boundary = offsets.folded == start;
            let (end_byte, end_char) = offsets.advance(end, &mut chars, self.query.case_sensitive);
            if !start_boundary || offsets.folded != end {
                continue;
            }
            if self.matches.len() == self.query.limit {
                return true;
            }
            let before = &text[..start_byte];
            let after = &text[end_byte..];
            let left = before
                .char_indices()
                .rev()
                .nth(CONTEXT_CHARS - 1)
                .map_or(0, |(i, _)| i);
            let right = after
                .char_indices()
                .nth(CONTEXT_CHARS)
                .map_or(after.len(), |(i, _)| i);
            self.matches.push(Match {
                section_index: self.index,
                section_kind: &self.section.kind,
                section_title: &self.section.title,
                source: props.get("source").unwrap_or(&self.section.source),
                path: path.to_vec(),
                address,
                x_pt: coord("x_pt"),
                y_pt: coord("y_pt"),
                start_char,
                end_char,
                before: before[left..].into(),
                text: text[start_byte..end_byte].into(),
                after: after[..right].into(),
            });
        }
        false
    }
}

fn lowercase(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

#[derive(Default)]
struct Offsets {
    original: usize,
    chars: usize,
    folded: usize,
}

impl Offsets {
    fn advance(
        &mut self,
        target: usize,
        chars: &mut std::str::Chars<'_>,
        exact: bool,
    ) -> (usize, usize) {
        while self.folded < target {
            let Some(c) = chars.next() else { break };
            self.original += c.len_utf8();
            self.chars += 1;
            self.folded += if exact || c.is_ascii() {
                c.len_utf8()
            } else {
                c.to_lowercase().map(char::len_utf8).sum()
            };
        }
        (self.original, self.chars)
    }
}
