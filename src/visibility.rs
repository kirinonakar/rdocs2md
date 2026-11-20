use crate::{formatting::length_in_points, model::*};

pub const TINY_POINTS: f64 = 3.;

pub fn annotate(doc: &mut Document) {
    let mut detected = [false; 4];
    let mut hidden_sheets = false;
    let mut hidden_columns = false;
    let mut hidden_rows = false;
    for section in &mut doc.sections {
        if section.kind == "sheet" {
            hidden_sheets |= section
                .properties
                .get("hidden")
                .is_some_and(|v| v == "true");
            hidden_columns |= section.properties.contains_key("hidden_columns");
            hidden_rows |= section.properties.contains_key("hidden_rows");
        }
        annotate_blocks(&mut section.blocks, doc.format != "pdf", &mut detected);
    }
    for (found, warning) in detected.into_iter().zip([
        "White text was detected and may blend into the background.",
        "Text smaller than 3 pt was detected.",
        "Text outside the page or slide bounds was detected.",
        "Text marked hidden or invisible was detected.",
    ]) {
        if found {
            doc.warn(warning);
        }
    }
    for (found, warning) in [
        (hidden_sheets, "Hidden sheets were detected."),
        (hidden_columns, "Hidden spreadsheet columns were detected."),
        (hidden_rows, "Hidden spreadsheet rows were detected."),
    ] {
        if found {
            doc.warn(warning);
        }
    }
}

fn annotate_blocks(blocks: &mut [Block], check_size: bool, detected: &mut [bool; 4]) {
    for block in blocks {
        match block {
            Block::Paragraph { runs, .. } => annotate_runs(runs, check_size, detected),
            Block::Table { rows } => {
                for cell in rows.iter_mut().flatten() {
                    annotate_runs(&mut cell.runs, check_size, detected);
                    annotate_blocks(&mut cell.blocks, check_size, detected);
                }
            }
        }
    }
}

fn annotate_runs(runs: &mut [Run], check_size: bool, detected: &mut [bool; 4]) {
    for run in runs.iter_mut().filter(|r| !r.text.trim().is_empty()) {
        let style = &run.style;
        for (found, flag) in [
            (
                style.color.as_deref().is_some_and(|v| {
                    v.eq_ignore_ascii_case("#fff") || v.eq_ignore_ascii_case("#ffffff")
                }),
                VisibilityFlag::WhiteText,
            ),
            (
                check_size
                    && style
                        .size
                        .as_deref()
                        .and_then(length_in_points)
                        .is_some_and(|v| (0. ..TINY_POINTS).contains(&v)),
                VisibilityFlag::TinyText,
            ),
            (style.hidden == Some(true), VisibilityFlag::HiddenText),
        ] {
            if found && !run.visibility_flags.contains(&flag) {
                run.visibility_flags.push(flag);
            }
        }
        for (found, flag) in detected.iter_mut().zip(VisibilityFlag::ALL) {
            *found |= run.visibility_flags.contains(&flag);
        }
    }
}

pub fn flag_text(blocks: &mut [Block], flag: VisibilityFlag) {
    for block in blocks {
        match block {
            Block::Paragraph { runs, .. } => flag_runs(runs, flag),
            Block::Table { rows } => {
                for cell in rows.iter_mut().flatten() {
                    flag_runs(&mut cell.runs, flag);
                    flag_text(&mut cell.blocks, flag);
                }
            }
        }
    }
}

fn flag_runs(runs: &mut [Run], flag: VisibilityFlag) {
    for run in runs.iter_mut().filter(|r| !r.text.trim().is_empty()) {
        run.visibility_flags.push(flag);
    }
}
