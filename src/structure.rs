use crate::{formatting, model::*};
use std::collections::BTreeMap;

pub(crate) fn is_bullet(c: char) -> bool {
    matches!(
        c,
        '•' | '◦'
            | '‣'
            | '⁃'
            | '∙'
            | '·'
            | '▪'
            | '▫'
            | '▸'
            | '□'
            | '■'
            | '○'
            | '●'
            | '◇'
            | '◆'
            | '△'
            | '▲'
            | '▽'
            | '▼'
            | '◁'
            | '◀'
            | '▷'
            | '▶'
    )
}

/// Byte length of the marker and its surrounding indentation, across run boundaries.
pub(crate) fn bullet_prefix(runs: &[Run]) -> Option<(char, usize)> {
    let mut chars = runs
        .iter()
        .take_while(|run| run.image.is_none() && run.latex.is_none() && run.mathml.is_none())
        .flat_map(|run| run.text.chars())
        .peekable();
    let space = |c| matches!(c, ' ' | '\t' | '\u{00a0}' | '\u{3000}');
    let mut len = 0;
    while let Some(c) = chars.next_if(|&c| space(c)) {
        len += c.len_utf8();
    }
    let marker = chars.next()?;
    if !is_bullet(marker) && !matches!(marker, '-' | '+' | '*') {
        return None;
    }
    len += marker.len_utf8();
    let end = len;
    while let Some(c) = chars.next_if(|&c| space(c)) {
        len += c.len_utf8();
    }
    if len == end && !matches!(marker, '•' | '◦' | '‣' | '⁃' | '▪' | '▫') {
        return None;
    }
    chars.any(|c| !c.is_whitespace()).then_some((marker, len))
}

pub(crate) fn remove_prefix(runs: &mut Vec<Run>, mut len: usize) {
    let first = runs
        .iter()
        .position(|run| {
            if len < run.text.len() {
                return true;
            }
            len -= run.text.len();
            false
        })
        .unwrap_or(runs.len());
    runs.drain(..first);
    if let Some(run) = runs.first_mut() {
        run.text.drain(..len);
    }
}

#[derive(Clone, Copy)]
pub(crate) enum HeadingEvidence {
    Formatted,
    Hwp3,
}

pub(crate) struct Headings {
    body: u16,
    sizes: Vec<u16>,
    evidence: HeadingEvidence,
}

impl Headings {
    pub(crate) fn infer<'a>(
        blocks: impl Iterator<Item = &'a Block> + Clone,
        evidence: HeadingEvidence,
    ) -> Option<Self> {
        let mut body_sizes = BTreeMap::<_, usize>::new();
        for block in blocks.clone() {
            let Block::Paragraph {
                heading,
                list,
                runs,
                ..
            } = block
            else {
                continue;
            };
            // The source hierarchy takes precedence when it is available.
            if *heading != 0 {
                return None;
            }
            if list.is_some() {
                continue;
            }
            for run in runs
                .iter()
                .filter(|run| !run.style.bold && run.image.is_none())
            {
                if let Some(size) = run_size(run) {
                    *body_sizes.entry(size).or_default() += run.text.trim().chars().count();
                }
            }
        }
        let (body, _) = body_sizes
            .into_iter()
            .filter(|&(_, weight)| weight > 0)
            .max_by_key(|&(size, weight)| (weight, std::cmp::Reverse(size)))?;
        let mut headings = Self {
            body,
            sizes: Vec::new(),
            evidence,
        };
        headings.sizes = blocks
            .filter_map(|block| match block {
                Block::Paragraph {
                    list: None, runs, ..
                } => headings.candidate(runs),
                _ => None,
            })
            .collect();
        headings.sizes.sort_unstable_by(|a, b| b.cmp(a));
        headings.sizes.dedup();
        (!headings.sizes.is_empty()).then_some(headings)
    }

    fn candidate(&self, runs: &[Run]) -> Option<u16> {
        let mut text = runs.iter().filter(|run| !run.text.trim().is_empty());
        let first = text.next()?;
        let size = run_size(first)?;
        if size < self.body
            || runs
                .iter()
                .any(|run| run.image.is_some() || run.latex.is_some() || run.mathml.is_some())
        {
            return None;
        }
        let mut bold = first.style.bold;
        for run in text {
            if run_size(run) != Some(size) {
                return None;
            }
            bold &= run.style.bold;
        }
        let threshold = match self.evidence {
            HeadingEvidence::Hwp3 if bold => 100,
            HeadingEvidence::Hwp3 => return None,
            HeadingEvidence::Formatted if bold => 108,
            HeadingEvidence::Formatted => 125,
        };
        if u32::from(size) * 100 < u32::from(self.body) * threshold {
            return None;
        }
        let chars = runs.iter().flat_map(|run| run.text.chars());
        if chars.clone().take(81).count() > 80
            || chars.clone().any(|c| c == '\n')
            || !chars.clone().any(char::is_alphabetic)
            || bullet_prefix(runs).is_some()
        {
            return None;
        }
        let last = chars.rev().find(|c| !c.is_whitespace())?;
        (!matches!(last, '.' | '?' | '!' | '。' | '？' | '！')).then_some(size)
    }

    pub(crate) fn level(&self, runs: &[Run]) -> Option<u8> {
        let size = self.candidate(runs)?;
        let position = self.sizes.binary_search_by(|other| size.cmp(other)).ok()?;
        Some((position + 1).min(6) as u8)
    }
}

fn run_size(run: &Run) -> Option<u16> {
    let points = formatting::length_in_points(run.style.size.as_deref()?)?;
    (points > 0. && points < 1000.).then_some((points * 2.).round() as u16)
}
