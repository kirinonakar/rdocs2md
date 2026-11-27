use super::{u16le, u32le, word_fib_table};
use crate::model::List;
use std::collections::{HashMap, HashSet};

const LEVELS: usize = 9;

#[derive(Clone, Copy, Default)]
pub(super) struct WordListRef {
    pub(super) id: Option<u16>,
    pub(super) depth: Option<usize>,
}

impl WordListRef {
    pub(super) fn apply(&mut self, code: u16, operand: &[u8]) {
        match code {
            0x260a => self.depth = operand.first().copied().map(usize::from),
            0x460b => {
                self.id = u16le(operand, 0).ok().map(|id| match id {
                    1..=0x07fe => id,
                    0xf802..=0xffff => 0u16.wrapping_sub(id),
                    _ => 0,
                });
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct Level {
    format: u8,
    start: i32,
    restart_before: usize,
}

type ListLevels = [Option<Level>; LEVELS];

struct ListOverride {
    identity: u32,
    levels: ListLevels,
    start: [Option<i32>; LEVELS],
}

pub(super) struct WordLists {
    lists: Vec<ListOverride>,
    next: HashMap<u32, [Option<i32>; LEVELS]>,
    seen_starts: HashSet<(usize, usize)>,
}

pub(super) struct WordListItem {
    pub(super) list: List,
    pub(super) identity: u32,
    pub(super) number: Option<i32>,
    pub(super) html_type: Option<&'static str>,
}

impl WordLists {
    pub(super) fn new(data: &[u8], table: &[u8], base: usize, pairs: usize) -> Self {
        let lists = if pairs > 74 {
            read_lists(data, table, base).unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            lists,
            next: HashMap::new(),
            seen_starts: HashSet::new(),
        }
    }

    pub(super) fn next_item(&mut self, reference: WordListRef) -> Option<WordListItem> {
        let id = usize::from(reference.id?.checked_sub(1)?);
        let depth = reference.depth.unwrap_or(0);
        let list = self.lists.get(id)?;
        let level = (*list.levels.get(depth)?)?;
        let numbers = self.next.entry(list.identity).or_default();
        for (next, other) in numbers.iter_mut().zip(list.levels).skip(depth + 1) {
            if other.is_some_and(|other| depth < other.restart_before) {
                *next = None;
            }
        }
        if level.format == 0xff {
            return None;
        }
        let ordered = level.format != 0x17;
        let number = ordered.then(|| {
            let restart = list.start[depth].filter(|_| self.seen_starts.insert((id, depth)));
            let value = restart.or(numbers[depth]).unwrap_or(level.start);
            numbers[depth] = Some(value.saturating_add(1));
            value
        });
        let html_type = match level.format {
            1 => Some("I"),
            2 => Some("i"),
            3 => Some("A"),
            4 => Some("a"),
            _ => None,
        };
        Some(WordListItem {
            list: List { ordered, depth },
            identity: list.identity,
            number,
            html_type,
        })
    }
}

fn read_lists(data: &[u8], table: &[u8], base: usize) -> Option<Vec<ListOverride>> {
    let headers = word_fib_table(data, table, base, 73)?;
    let count = usize::from(u16le(headers, 0).ok()?);
    let headers = headers.get(2..2 + count * 28)?;
    // lcbPlfLst covers the list headers. The levels start immediately after them.
    let start = u32le(data, base + 73 * 8).ok()? as usize;
    let mut levels = table.get(start.checked_add(2 + headers.len())?..)?;
    let mut definitions = HashMap::new();
    for header in headers.chunks_exact(28) {
        let identity = u32le(header, 0).ok()?;
        let count = if header[26] & 1 != 0 { 1 } else { LEVELS };
        let mut definition = [None; LEVELS];
        for (depth, level) in definition[..count].iter_mut().enumerate() {
            *level = Some(read_level(&mut levels, depth)?);
        }
        definitions.insert(identity, definition);
    }

    let overrides = word_fib_table(data, table, base, 74)?;
    let count = u32le(overrides, 0).ok()? as usize;
    let header_size = count.checked_mul(16)?;
    let (headers, mut records) = overrides[4..].split_at_checked(header_size)?;
    let mut lists = Vec::new();
    for header in headers.chunks_exact(16) {
        let identity = u32le(header, 0).ok()?;
        let mut levels = definitions.get(&identity).copied().unwrap_or_default();
        let mut start = [None; LEVELS];
        records = records.get(4..)?; // Character position of the first use.
        for _ in 0..header[12] {
            let (record, rest) = records.split_at_checked(8)?;
            records = rest;
            let flags = record[4];
            let depth = usize::from(flags & 0x0f);
            let override_level = if flags & 0x20 != 0 {
                Some(read_level(&mut records, depth)?)
            } else {
                None
            };
            if depth < LEVELS {
                if let Some(level) = override_level {
                    levels[depth] = Some(level);
                }
                if flags & 0x10 != 0 {
                    start[depth] = override_level
                        .map(|l| l.start)
                        .or_else(|| read_start(record));
                }
            }
        }
        lists.push(ListOverride {
            identity,
            levels,
            start,
        });
    }
    Some(lists)
}

fn read_start(bytes: &[u8]) -> Option<i32> {
    Some(i32::from_le_bytes(bytes.get(..4)?.try_into().ok()?))
}

fn read_level(bytes: &mut &[u8], depth: usize) -> Option<Level> {
    let (header, properties) = bytes.split_at_checked(28)?;
    let label = properties.get(usize::from(header[24]) + usize::from(header[25])..)?;
    let chars = usize::from(u16le(label, 0).ok()?);
    *bytes = label.get(2 + chars * 2..)?;
    Some(Level {
        format: header[4],
        start: read_start(header)?,
        restart_before: if header[5] & 8 != 0 {
            usize::from(header[26])
        } else {
            depth
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Document, Properties, Run, Section};
    use crate::render::render;

    #[test]
    fn word_lists_preserve_levels_continuation_and_start_overrides() {
        let mut fib = vec![0; 75 * 8];
        let mut table = vec![2, 0]; // Two LSTF headers followed by their LVLs.
        for (id, simple) in [(42u32, 0), (43, 1)] {
            let mut header = [0; 28];
            header[..4].copy_from_slice(&id.to_le_bytes());
            header[26] = simple;
            table.extend(header);
        }
        fib[73 * 8 + 4..74 * 8].copy_from_slice(&(table.len() as u32).to_le_bytes());
        for format in [0, 0x17, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x17] {
            let mut level = [0; 30];
            level[0] = 1;
            level[4] = format;
            table.extend(level);
        }
        let lfo = table.len();
        table.extend(3u32.to_le_bytes());
        for (id, count) in [(42u32, 0), (43, 0), (42, 1)] {
            let mut header = [0; 16];
            header[..4].copy_from_slice(&id.to_le_bytes());
            header[12] = count;
            table.extend(header);
        }
        table.extend([0xff; 12]); // One CP for each override.
        table.extend([6, 0, 0, 0, 0x10, 0, 0, 0]); // Restart level zero at six.
        fib[74 * 8..74 * 8 + 4].copy_from_slice(&(lfo as u32).to_le_bytes());
        fib[74 * 8 + 4..75 * 8].copy_from_slice(&((table.len() - lfo) as u32).to_le_bytes());

        let mut section = Section::default();
        let mut lists = WordLists::new(&fib, &table, 0, 75);
        for (id, depth, text) in [
            (1u16, 0, "Parent"),
            (1, 1, "Child"),
            (2, 0, "Other"),
            (1, 0, "Second"),
            (3, 0, "Restart"),
            (3, 0, "Next"),
        ] {
            let mut reference = WordListRef::default();
            reference.apply(0x460b, &id.to_le_bytes());
            reference.apply(0x260a, &[depth]);
            super::super::word_paragraph(
                &mut section,
                vec![Run::text(text)],
                None,
                lists.next_item(reference),
            );
        }
        let mut doc = Document::new("doc");
        doc.sections.push(section);
        let md = render(&doc, "md", "semantic", &Properties::new()).unwrap();
        assert_eq!(
            md,
            "1. Parent\n   - Child\n\n- Other\n\n2. Second\n6) Restart\n7) Next\n"
        );
        let html = render(&doc, "html", "semantic", &Properties::new()).unwrap();
        assert!(html.contains("Parent<ul><li"));
        assert!(html.contains("Child</li></ul></li></ol>"));
        assert!(html.contains("<ol start=\"2\">"));
        assert!(html.contains(" value=\"6\">Restart</li>"));
    }
}
