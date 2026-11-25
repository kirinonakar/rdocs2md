use rdocs2md::{convert, convert_range, convert_sheets, model::*, render::render, search::Query};
use serde_json::{json, to_value};
use std::io::{Cursor, Write};
fn package(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, content) in entries {
        z.start_file(*path, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(content.as_bytes()).unwrap()
    }
    z.finish().unwrap().into_inner()
}
fn md(d: &Document) -> String {
    render(d, "md", "hybrid", &Properties::new()).unwrap()
}
#[test]
fn rtf_groups_codepages_unicode_fields_and_table() {
    let d=convert(br#"{\rtf1\ansi\ansicpg949 \'c7\'d1 {\b bold} \u-10179?\u-8704?{\*\ignored private}{\field{\*\fldinst HYPERLINK "https://example.org"}{\fldrslt site}}\par\trowd A\cell B\cell\row\page end}"#,false).unwrap();
    let s = md(&d);
    for v in [
        "한",
        "**bold**",
        "😀",
        "[site](<https://example.org>)",
        "| A | B |",
        "end",
    ] {
        assert!(s.contains(v), "{v} in {s}")
    }
    assert!(!s.contains("private"));
    assert_eq!(d.sections.len(), 2);
    let html = render(&d, "html", "hybrid", &Properties::new()).unwrap();
    assert!(html.contains("border: 1px solid") && html.contains("<table>"));
}
#[test]
fn ods_repeats_formulas_spans_and_modes() {
    let content = r#"<document><body><spreadsheet><table name="예시"><table-row><table-cell number-columns-spanned="2"><p>자료</p></table-cell><covered-table-cell/></table-row><table-row number-rows-repeated="2"><table-cell number-columns-repeated="2" formula="of:=1+1" value="2"><p>2</p></table-cell></table-row></table></spreadsheet></body></document>"#;
    let d = convert(
        &package(&[
            ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
            ("content.xml", content),
        ]),
        false,
    )
    .unwrap();
    let Block::Table { rows } = &d.sections[0].blocks[0] else {
        panic!()
    };
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].len(), 2);
    assert_eq!(rows[2][0].properties["formula"], "of:=1+1");
    for mode in ["layout", "semantic", "hybrid"] {
        assert!(
            render(&d, "md", mode, &Properties::new())
                .unwrap()
                .contains("colspan=\"2\"")
        )
    }
}
#[test]
fn epub_spine_direction_css_and_safe_markup() {
    let doc = package(&[
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            "<container><rootfile full-path='OPS/book.opf'/></container>",
        ),
        (
            "OPS/book.opf",
            "<package><manifest><item id='a' href='a.xhtml' media-type='application/xhtml+xml'/><item id='b' href='b.xhtml' media-type='application/xhtml+xml'/></manifest><spine><itemref idref='b'/><itemref idref='a'/></spine></package>",
        ),
        ("OPS/a.xhtml", "<html><body><h1>A</h1></body></html>"),
        (
            "OPS/b.xhtml",
            "<!DOCTYPE html><html><head><style>.v{writing-mode:vertical-rl;font-weight:bold}</style></head><body><p class='v' dir='rtl'>한글 <ruby>字<rt>じ</rt></ruby> &lt;hello&gt;</p><script>hidden</script></body></html>",
        ),
    ]);
    let mut d = convert(&doc, false).unwrap();
    assert_eq!(d.sections[0].source, "OPS/b.xhtml");
    let s = md(&d);
    for v in ["vertical-rl", "rtl", "<rt>", "&lt;hello&gt;", "# A"] {
        assert!(s.contains(v), "{s}")
    }
    assert!(!s.contains("hidden"));
    d.select("2").unwrap();
    assert_eq!(md(&d).trim(), "# A");
    assert!(d.select("0").is_err())
}
#[test]
fn rejects_invalid_structures_and_excessive_expansion() {
    for b in [
        b"not a document".as_slice(),
        br"{\rtf1 truncated",
        br"{\rtf1\bin999 x}",
    ] {
        assert!(convert(b, false).is_err())
    }
    let r = format!("{{\\rtf1 {}x{}", "{".repeat(129), "}".repeat(130));
    assert!(convert(r.as_bytes(), false).is_err());
    for bad in [
        "<r><body><spreadsheet><table><table-row number-rows-repeated='900000'><table-cell/></table-row></table></spreadsheet></body></r>",
        "<!DOCTYPE r [<!ENTITY x 'x'>]><r>&x;</r>",
    ] {
        assert!(
            convert(
                &package(&[
                    ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
                    ("content.xml", bad)
                ]),
                false
            )
            .is_err()
        )
    }
}
#[test]
fn pdf_columns_coordinates_and_page_range() {
    use lopdf::{Document as Pdf, Object, Stream, dictionary};
    let mut p = Pdf::with_version("1.4");
    let tree = p.new_object_id();
    let f = p.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let resource = p.add_object(dictionary! {"Font"=>dictionary!{"F"=>f}});
    let mut pages = Vec::new();
    for _ in 0..2 {
        let mut content = String::from(
            "BT /F 18 Tf 1 0 0 1 40 750 Tm (A full-width heading across the three text columns) Tj ET\n",
        );
        for (x, offset, lines) in [
            (
                40,
                0,
                [
                    "This is the first para-",
                    "graph in the left column.",
                    "Another left paragraph.",
                ],
            ),
            (
                220,
                2,
                [
                    "The middle column starts",
                    "separately from the left.",
                    "Another middle paragraph.",
                ],
            ),
            (
                400,
                1,
                [
                    "The right column begins",
                    "under the same heading.",
                    "Another right paragraph.",
                ],
            ),
        ] {
            for (i, line) in lines.iter().enumerate() {
                let y = 700 - offset - (i as i32 * 14) - i.saturating_sub(1) as i32 * 12;
                content += &format!("BT /F 12 Tf 1 0 0 1 {x} {y} Tm ({line}) Tj ET\n");
            }
        }
        let stream = p.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let id=p.add_object(dictionary!{"Type"=>"Page","Parent"=>tree,"Contents"=>stream,"Resources"=>resource,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()]});
        pages.push(Object::Reference(id));
    }
    p.objects.insert(
        tree,
        dictionary! {"Type"=>"Pages","Kids"=>pages,"Count"=>2}.into(),
    );
    let catalog = p.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree});
    p.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    p.save_to(&mut bytes).unwrap();
    let mut d = convert(&bytes, false).unwrap();
    d.select("2").unwrap();
    let s = md(&d);
    assert!(
        s.contains("This is the first paragraph in the left column.\n\nAnother left paragraph.")
    );
    assert!(s.contains("The middle column starts separately from the left."));
    assert!(s.contains("The right column begins under the same heading."));
    assert!(s.find("Another left paragraph.").unwrap() < s.find("The middle column").unwrap());
    assert!(s.find("Another middle paragraph.").unwrap() < s.find("The right column").unwrap());
    assert!(s.find("A full-width heading").unwrap() < s.find("This is the first").unwrap());
    assert!(serde_json::to_string(&d).unwrap().contains("x_pt"));
}

fn positioned_pdf(content: String) -> Vec<u8> {
    pdf_with_pages(&[&content])
}

fn pdf_with_pages(contents: &[&str]) -> Vec<u8> {
    use lopdf::{Document as Pdf, Stream, dictionary};

    let mut pdf = Pdf::with_version("1.4");
    let tree = pdf.new_object_id();
    let font =
        pdf.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let resources = dictionary! {"Font"=>dictionary! {"F"=>font}};
    let pages: Vec<_> = contents
        .iter()
        .map(|content| {
            let stream = pdf.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
            pdf.add_object(dictionary! {
                "Type"=>"Page","Parent"=>tree,"Contents"=>stream,"Resources"=>resources.clone()
            })
            .into()
        })
        .collect();
    pdf.objects.insert(
        tree,
        dictionary! {"Type"=>"Pages","Kids"=>pages,"Count"=>contents.len() as i64,
        "MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]}
        .into(),
    );
    let catalog = pdf.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree});
    pdf.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn pdf_decrypts_permission_only_encryption_and_rejects_user_passwords() {
    use lopdf::{Document as Pdf, EncryptionState, EncryptionVersion, Object, Permissions};

    fn encrypted(owner: &str, user: &str) -> Vec<u8> {
        let mut pdf = Pdf::load_mem(&pdf_with_pages(&[
            "BT /F 12 Tf 1 0 0 1 30 180 Tm (Permission only text) Tj ET",
        ]))
        .unwrap();
        // lopdf derives the file key from the trailer's ID pair.
        pdf.trailer
            .set("ID", vec![Object::string_literal("0123456789abcdef"); 2]);
        let state = EncryptionState::try_from(EncryptionVersion::V2 {
            document: &pdf,
            owner_password: owner,
            user_password: user,
            key_length: 128,
            permissions: Permissions::empty(),
        })
        .unwrap();
        pdf.encrypt(&state).unwrap();
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        bytes
    }

    let open = encrypted("owner-secret", "");
    let rendered = md(&convert(&open, false).unwrap());
    assert!(rendered.contains("Permission only text"), "{rendered}");

    let locked = encrypted("owner-secret", "user-secret");
    assert!(convert(&locked, false).is_err());
}

#[test]
fn pdf_range_skips_bad_page_and_marks_visibility() {
    let bytes = pdf_with_pages(&[
        "1 0 cm",
        "BT /F 10 Tf 1 0 0 1 30 180 Tm (Normal) Tj ET\n\
         q 1 g BT /F 2 Tf 1 0 0 1 30 130 Tm (White tiny) Tj ET Q\n\
         BT /F 10 Tf 3 Tr 1 0 0 1 30 80 Tm (Invisible) Tj ET\n\
         BT /F 10 Tf 0 Tr 1 0 0 1 400 40 Tm (Outside) Tj ET",
        "q BI /W 1 /H 1 /BPC 8 /CS /G ID X EI Q",
    ]);
    // A malformed page is reported instead of discarding the other pages.
    let full = convert(&bytes, false).unwrap();
    assert_eq!(full.sections.len(), 3);
    assert!(full.sections[0].properties.contains_key("page_error"));
    assert!(
        full.warnings
            .iter()
            .any(|w| w.contains("could not be read completely"))
    );
    let d = convert_range(&bytes, false, Some("3,2,2")).unwrap();
    assert_eq!(d.sections.len(), 2);
    assert_eq!(
        d.sections[1].properties["text_extraction"],
        "ocr_needed_for_image_text"
    );
    assert!(
        d.warnings
            .iter()
            .any(|w| w.contains("Text visible in the images requires OCR"))
    );
    let runs = d.sections[0]
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph { runs, .. } => Some(runs.as_slice()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    for (text, flags) in [
        ("Normal", &[][..]),
        (
            "White tiny",
            &[VisibilityFlag::WhiteText, VisibilityFlag::TinyText],
        ),
        ("Invisible", &[VisibilityFlag::HiddenText]),
        ("Outside", &[VisibilityFlag::OffPage]),
    ] {
        assert_eq!(
            runs.iter()
                .find(|r| r.text == text)
                .unwrap()
                .visibility_flags,
            flags,
            "{text}"
        );
    }
    let result = to_value(Query::new("outside", false, 1).unwrap().find(&d)).unwrap();
    assert_eq!(result["matches"][0]["section_index"], 2);
    assert_eq!(result["matches"][0]["x_pt"], 400.);
    assert!(convert_range(&bytes, false, Some("4")).is_err());
}

#[test]
fn ods_visibility_and_range_skip_expansion_in_other_sheet() {
    let bytes = package(&[
        ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
        (
            "content.xml",
            "<r><automatic-styles><style name='hidden'><table-properties display='false'/></style></automatic-styles><body><spreadsheet>\
            <table><table-row number-rows-repeated='900000'><table-cell/></table-row></table>\
            <table name='Hidden' style-name='hidden'><table-column/><table-column-group display='false'><table-column number-columns-repeated='2'/></table-column-group>\
            <table-row visibility='filter'><table-cell number-columns-repeated='3'><p>Token</p></table-cell></table-row></table>\
            </spreadsheet></body></r>",
        ),
    ]);
    assert!(convert(&bytes, false).is_err());
    let d = convert_range(&bytes, false, Some("2")).unwrap();
    let s = &d.sections[0];
    assert_eq!(s.properties["hidden"], "true");
    assert_eq!(s.properties["hidden_columns"], "2-3");
    assert_eq!(s.properties["hidden_rows"], "1");
    let Block::Table { rows } = &s.blocks[0] else {
        panic!()
    };
    assert!(!rows[0][0].properties.contains_key("hidden_column"));
    assert_eq!(rows[0][1].properties["hidden_column"], "true");
    assert_eq!(rows[0][2].properties["hidden_row"], "true");
    assert_eq!(d.warnings.len(), 3);
    let result = to_value(Query::new("TOKEN", false, 4).unwrap().find(&d)).unwrap();
    assert_eq!(result["matches"].as_array().unwrap().len(), 3);
    assert_eq!(
        result["matches"][2]["path"],
        json!([{"block":1},{"row":1,"column":3}])
    );
}

#[test]
fn rtf_range_skips_later_pages_and_keeps_inherited_state() {
    let bytes = br"{\rtf1\fs4\v before\page middle\page broken {";
    assert!(convert(bytes, false).is_err());
    let d = convert_range(bytes, false, Some("2")).unwrap();
    assert_eq!(md(&d).trim(), "middle");
    let ir = to_value(d).unwrap();
    assert_eq!(
        ir["sections"][0]["blocks"][0]["runs"][0]["visibility_flags"],
        json!(["tiny_text", "hidden_text"])
    );
}

#[test]
fn a_panicking_document_does_not_stop_subsequent_work() {
    let first = rdocs2md::recover_document::<()>(|| panic!("simulated parser panic"));
    assert!(first.unwrap_err().to_string().contains("panicked"));
    let next = rdocs2md::recover_document(|| convert(br"{\rtf1 next}", false)).unwrap();
    assert_eq!(md(&next).trim(), "next");
}

fn cell_text(row: &[Cell]) -> Vec<String> {
    row.iter()
        .map(|cell| cell.runs.iter().map(|run| run.text.as_str()).collect())
        .collect()
}

#[test]
fn pdf_reflows_loose_leading_and_keeps_paragraph_and_reference_boundaries() {
    let content = "BT /F 9 Tf\n\
        1 0 0 1 49 200 Tm (The document includes page-naviga) Tj\n\
        /Span << /MCID 1 >> BDC 1 0 0 1 189 200 Tm (-) Tj EMC\n\
        1 0 0 1 40 185 Tm (tion and other reading controls for one-) Tj\n\
        1 0 0 1 40 170 Tm (step access. This line is filled to the margin.) Tj\n\
        1 0 0 1 49 155 Tm (Another paragraph starts here and then) Tj\n\
        1 0 0 1 40 140 Tm (continues on the next line.) Tj\n\
        1 0 0 1 40 104 Tm (1. A reference with a long title that wraps.) Tj\n\
        1 0 0 1 52 89 Tm (Publisher and year.) Tj\n\
        1 0 0 1 40 74 Tm (2. The next reference.) Tj ET";
    let rendered = md(&convert(&positioned_pdf(content.into()), false).unwrap());
    assert_eq!(
        rendered.trim(),
        "The document includes page-navigation and other reading controls for one-step access. This line is filled to the margin.\n\
         \nAnother paragraph starts here and then continues on the next line.\n\
         \n1. A reference with a long title that wraps. Publisher and year.\n\
         \n2. The next reference."
    );
}

#[test]
fn pdf_segmented_rules_keep_centered_headers_and_wrapped_cells() {
    let content = "1 w 40 175 m 90 175 l 90 175 m 255 175 l S\n\
        0.5 w 40 160 m 90 160 l 90 160 m 255 160 l S\n\
        40 92 m 90 92 l 90 92 m 255 92 l S\n\
        BT /F 8 Tf\n\
        1 0 0 1 40 189 Tm (Table caption) Tj\n\
        1 0 0 1 53 164 Tm (Group) Tj 1 0 0 1 150 164 Tm (Description) Tj\n\
        1 0 0 1 60 148 Tm (A1) Tj 1 0 0 1 90 148 Tm (A description that has a) Tj\n\
        1 0 0 1 96 135 Tm (second line.) Tj 1 0 0 1 263 135 Tm (Side note) Tj\n\
        1 0 0 1 60 121 Tm (A2) Tj 1 0 0 1 90 121 Tm (Another description with) Tj\n\
        1 0 0 1 96 108 Tm (its own continuation.) Tj\n\
        1 0 0 1 40 81 Tm (Table note) Tj ET";
    let doc = convert(&positioned_pdf(content.into()), false).unwrap();
    let tables: Vec<_> = doc.sections[0]
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Table { rows } => {
                Some(rows.iter().map(|row| cell_text(row)).collect::<Vec<_>>())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        tables,
        [vec![
            vec!["Group", "Description"],
            vec!["A1", "A description that has a second line."],
            vec!["A2", "Another description with its own continuation."],
        ]]
    );
    let rendered = md(&doc);
    assert!(rendered.contains("Table caption\n\n| Group | Description |\n| --- | --- |"));
    assert!(rendered.contains("Side note") && rendered.contains("Table note"));
}

#[test]
fn pdf_spaced_glyphs_and_ruled_headings_form_a_table() {
    let mut content = String::from(
        "84 167 m 180 167 l 196 167 m 292 167 l S\n\
         84 156 m 124 156 l 140 156 m 180 156 l 196 156 m 236 156 l 252 156 m 292 156 l S\n\
         BT /F 8 Tf\n\
         1 0 0 1 40 185 Tm (Results) Tj\n\
         1 0 0 1 112 170 Tm (Reader A) Tj 1 0 0 1 224 170 Tm (Reader B) Tj\n\
         1 0 0 1 88 159 Tm (First) Tj 1 0 0 1 144 159 Tm (Second) Tj\n\
         1 0 0 1 200 159 Tm (First) Tj 1 0 0 1 256 159 Tm (Second) Tj\n",
    );
    for (x, label) in [40, 84, 112, 140, 168, 196, 224, 252, 280]
        .into_iter()
        .zip(["No.", "TP", "FP", "TP", "FP", "TP", "FP", "TP", "FP"])
    {
        content.push_str(&format!("1 0 0 1 {x} 148 Tm ({label}) Tj\n"));
    }
    content.push_str(
        "24 Tc\n\
         1 0 0 1 40 134 Tm [(9) -2000 (01234567)] TJ\n\
         1 0 0 1 36 123 Tm [(1) 3000 (0) -2000 (76543210)] TJ\n\
         0 Tc 1 0 0 1 40 108 Tm (Table note) Tj ET\n",
    );
    let doc = convert(&positioned_pdf(content), false).unwrap();
    let [
        Block::Paragraph { .. },
        Block::Table { rows },
        Block::Paragraph { .. },
    ] = doc.sections[0].blocks.as_slice()
    else {
        panic!("expected caption, table and note: {}", md(&doc));
    };
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][1].colspan, Some(4));
    assert_eq!(rows[0][2].colspan, Some(4));
    assert!(rows[1][1..].iter().all(|cell| cell.colspan == Some(2)));
    assert!(rows[2..].iter().all(|row| row.len() == 9));
    assert_eq!(cell_text(&rows[0]), ["", "Reader A", "Reader B"]);
    assert_eq!(
        cell_text(&rows[3]),
        ["9", "0", "1", "2", "3", "4", "5", "6", "7"]
    );
    assert_eq!(
        cell_text(&rows[4]),
        ["10", "7", "6", "5", "4", "3", "2", "1", "0"]
    );
}

#[test]
fn pdf_ruled_table_keeps_section_rows_and_wrapped_headings_beside_prose() {
    let content = "0.75 w 1 0 0 RG [3 2] 0 d 40 150 m 222 150 l S 40 80 m 222 80 l S\n\
        BT /F 8 Tf\n\
        1 0 0 1 94 167 Tm (First) Tj 1 0 0 1 166 167 Tm (Third) Tj\n\
        1 0 0 1 250 163 Tm (Adjacent prose) Tj\n\
        1 0 0 1 40 156 Tm (Measure) Tj 1 0 0 1 94 156 Tm (score) Tj\n\
        1 0 0 1 145 156 Tm (B) Tj 1 0 0 1 166 156 Tm (score) Tj\n\
        1 0 0 1 250 151 Tm (next line) Tj\n\
        1 0 0 1 42 140 Tm (First group) Tj\n\
        1 0 0 1 45 129 Tm (Mean) Tj 1 0 0 1 94 129 Tm (0.51) Tj\n\
        1 0 0 1 145 129 Tm [(0.53) -821 (0.87)] TJ\n\
        1 0 0 1 42 107 Tm (Second group) Tj\n\
        1 0 0 1 45 96 Tm (Mean) Tj 1 0 0 1 94 96 Tm (0.61) Tj\n\
        1 0 0 1 145 96 Tm [(0.68) -821 (0.93)] TJ\n\
        /F 7 Tf 1 0 0 1 40 70 Tm (Table note) Tj ET";
    let doc = convert(&positioned_pdf(content.into()), false).unwrap();
    let table = doc.sections[0].blocks.iter().find_map(|block| match block {
        Block::Table { rows } => Some(rows),
        _ => None,
    });
    let rows = table.expect("expected one ruled table");
    assert_eq!(rows.len(), 5);
    assert_eq!(
        cell_text(&rows[0]),
        ["Measure", "First score", "B", "Third score"]
    );
    assert_eq!(cell_text(&rows[1]), ["First group"]);
    assert_eq!(rows[1][0].colspan, Some(4));
    assert_eq!(cell_text(&rows[2]), ["Mean", "0.51", "0.53", "0.87"]);
    assert_eq!(cell_text(&rows[3]), ["Second group"]);
    assert_eq!(rows[3][0].colspan, Some(4));
    assert_eq!(cell_text(&rows[4]), ["Mean", "0.61", "0.68", "0.93"]);
    let rendered = md(&doc);
    assert_eq!(rendered.matches("<table>").count(), 1);
    assert!(rendered.contains("Adjacent prose") && rendered.contains("</table>\n\nTable note"));
    let html = render(&doc, "html", "hybrid", &Properties::new()).unwrap();
    assert_eq!(
        html.matches("border-bottom:0.75pt dashed #FF0000;").count(),
        8
    );
}

#[test]
fn hwp_tables_keep_merged_cells_and_nested_answers() {
    fn record(out: &mut Vec<u8>, tag: u32, level: u32, data: &[u8]) {
        let header = (data.len() as u32) << 20 | level << 10 | tag;
        out.extend_from_slice(&header.to_le_bytes());
        out.extend_from_slice(data);
    }
    fn table(out: &mut Vec<u8>, level: u32, rows: u16, columns: u16) {
        record(out, 71, level, b" lbt");
        let mut data = [0; 8];
        data[4..6].copy_from_slice(&rows.to_le_bytes());
        data[6..8].copy_from_slice(&columns.to_le_bytes());
        record(out, 77, level + 1, &data);
    }
    fn cell(out: &mut Vec<u8>, level: u32, address: [u16; 4]) {
        let mut data = vec![1, 0, 0, 0, 0, 0, 5, 0];
        data.extend(address.into_iter().flat_map(u16::to_le_bytes));
        data.resize(47, 0);
        data[16..20].copy_from_slice(&6000_u32.to_le_bytes());
        data[32..34].copy_from_slice(&1_u16.to_le_bytes());
        record(out, 72, level, &data);
    }
    fn paragraph(out: &mut Vec<u8>, level: u32, text: &str) {
        record(out, 66, level, &[0; 10]);
        let styled = text == "After";
        let text: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        record(out, 67, level + 1, &text);
        if styled {
            let shapes: Vec<_> = [0_u32, 0, 2, 1]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            record(out, 68, level + 1, &shapes);
        }
    }
    let mut raw = Vec::new();
    paragraph(&mut raw, 0, "Before");
    table(&mut raw, 1, 2, 3);
    cell(&mut raw, 2, [0, 0, 1, 2]);
    paragraph(&mut raw, 2, "1");
    cell(&mut raw, 2, [1, 0, 2, 1]);
    paragraph(&mut raw, 2, "Question and answer");
    cell(&mut raw, 2, [1, 1, 1, 1]);
    paragraph(&mut raw, 2, "Question");
    cell(&mut raw, 2, [2, 1, 1, 1]);
    paragraph(&mut raw, 2, "Answer");
    table(&mut raw, 3, 1, 2);
    cell(&mut raw, 4, [0, 0, 1, 1]);
    paragraph(&mut raw, 4, "Nested answer");
    cell(&mut raw, 4, [1, 0, 1, 1]);
    let mut shape = [0; 50];
    shape[..8].copy_from_slice(b"cip$cip$");
    shape[28..32].copy_from_slice(&950_u32.to_le_bytes());
    shape[32..36].copy_from_slice(&1000_u32.to_le_bytes());
    shape[40..42].copy_from_slice(&270_u16.to_le_bytes());
    record(&mut raw, 76, 6, &shape);
    let mut picture = [0; 73];
    picture[71..73].copy_from_slice(&1_u16.to_le_bytes());
    record(&mut raw, 85, 7, &picture);
    paragraph(&mut raw, 2, "Answer continued");
    paragraph(&mut raw, 0, "After");
    let mut file = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    let mut header = [0; 40];
    header[..17].copy_from_slice(b"HWP Document File");
    header[35] = 5;
    file.create_stream("/FileHeader")
        .unwrap()
        .write_all(&header)
        .unwrap();
    file.create_storage("/BodyText").unwrap();
    file.create_stream("/BodyText/Section0")
        .unwrap()
        .write_all(&raw)
        .unwrap();
    let mut bin_data = vec![0x11, 0, 7, 0, 3, 0];
    bin_data.extend("bmp".encode_utf16().flat_map(u16::to_le_bytes));
    let mut info = Vec::new();
    record(&mut info, 18, 0, &bin_data);
    let mut borders = [0; 48];
    borders[2..8].copy_from_slice(&[2, 6, 0xAB, 0x12, 0x34, 0]);
    borders[32] = 1;
    borders[36..39].copy_from_slice(&[0xF2, 0xE5, 0xC4]);
    record(&mut info, 20, 0, &borders);
    let mut characters = [0; 56];
    characters[42..46].copy_from_slice(&1200_u32.to_le_bytes());
    characters[52..55].copy_from_slice(&[0x11, 0x22, 0x33]);
    record(&mut info, 21, 0, &characters);
    characters[42..46].copy_from_slice(&900_u32.to_le_bytes());
    record(&mut info, 21, 0, &characters);
    record(&mut info, 25, 0, &[12]);
    file.create_stream("/DocInfo")
        .unwrap()
        .write_all(&info)
        .unwrap();
    let mut bitmap = [0; 58];
    bitmap[..2].copy_from_slice(b"BM");
    bitmap[2] = 58;
    bitmap[10] = 54;
    bitmap[14] = 40;
    bitmap[18] = 1;
    bitmap[22] = 1;
    bitmap[26] = 1;
    bitmap[28] = 24;
    let mut packed = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    packed.write_all(&bitmap).unwrap();
    file.create_storage("/BinData").unwrap();
    file.create_stream("/BinData/BIN0007.bmp")
        .unwrap()
        .write_all(&packed.finish().unwrap())
        .unwrap();
    let file = file.into_inner().into_inner();
    let doc = convert(&file, false).unwrap();
    let text = md(&doc);
    assert!(text.contains("<td rowspan=\"2\">1</td><td colspan=\"2\">Question and answer</td>"));
    assert!(text.contains("<td>Question</td><td><p>Answer</p>\n<table>"));
    assert!(text.contains("<td>Nested answer</td>"));
    assert!(text.contains("</table>\n<p>Answer continued</p>\n</td>"));
    assert_eq!(text.matches("\n\n").count(), 3);
    assert!(text.starts_with("Before\n\n<table>") && text.ends_with("After\n\n"));
    assert!(doc.assets.is_empty());

    let doc = convert(&file, true).unwrap();
    assert_eq!(doc.assets["hwp/1"], bitmap);
    let links = Properties::from([("hwp/1".into(), "image.bmp".into())]);
    let result = render(&doc, "md", "hybrid", &links).unwrap();
    assert!(result.contains("<td><img alt=\"\" src=\"image.bmp\" style=\"width:9.5pt;height:10pt;transform:rotate(270deg)\"></td>"));
    let html = render(&doc, "html", "hybrid", &links).unwrap();
    for expected in [
        "border-left:0.4mm dashed #AB1234;",
        "border-right:none;",
        "background-color:#F2E5C4;",
        "width:60pt;",
        "rowspan=\"2\"",
        "<p style=\"text-align:center;\">Answer continued</p>",
        "font-size:12pt;color:#112233;\">Af</span><span style=\"font-size:9pt;color:#112233;\">ter</span>",
    ] {
        assert!(html.contains(expected), "missing {expected} in {html}");
    }
}

#[test]
fn html_hwpx_keeps_referenced_cell_border_fill_and_alignment() {
    let input = package(&[
        ("Contents/content.hpf", "<package/>"),
        (
            "Contents/header.xml",
            r##"<head><borderFill id="7"><leftBorder type="SOLID" width="0.4 mm" color="#A43221"/><rightBorder type="NONE"/><topBorder type="DOT" width="0.2mm"/><fillBrush><winBrush faceColor="#F1E2D3"/></fillBrush></borderFill><paraPr id="3"><align horizontal="CENTER"/></paraPr></head>"##,
        ),
        (
            "Contents/section0.xml",
            r#"<sec><tbl><tr><tc borderFillIDRef="7"><subList vertAlign="CENTER"><p paraPrIDRef="3"><run><t>원본 표</t></run></p></subList><cellSpan colSpan="2"/><cellSz width="9000" height="2400"/><cellMargin left="400"/></tc></tr></tbl></sec>"#,
        ),
    ]);
    let doc = convert(&input, false).unwrap();
    for mode in ["semantic", "layout", "hybrid"] {
        let html = render(&doc, "html", mode, &Properties::new()).unwrap();
        for expected in [
            "colspan=\"2\"",
            "border-left:0.4mm solid #A43221;",
            "border-right:none;",
            "border-top:0.2mm dotted #000000;",
            "background-color:#F1E2D3;",
            "height:24pt;",
            "width:90pt;",
            "padding-left:4pt;",
            "text-align:center;",
            "vertical-align:middle;",
        ] {
            assert!(html.contains(expected), "missing {expected} in {html}");
        }
    }
    assert!(md(&doc).contains("<td colspan=\"2\">원본 표</td>"));
}

#[test]
fn html_hwp3_restores_page_anchored_vector_pictures_and_label_positions() {
    fn short(buffer: &mut [u8], offset: usize, value: u16) {
        buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn drawing(kind: u16, order: i32) -> Vec<u8> {
        let mut payload = Vec::new();
        for value in [24i32, order, 1, -10, -10, 800, 600, 88] {
            payload.extend(value.to_le_bytes());
        }
        let mut shape = [0u8; 88];
        short(&mut shape, 0, kind);
        for (offset, value) in [(12, 780i32), (16, 580), (60, 20), (64, 0x00_00_ff)] {
            shape[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        payload.extend(shape);
        if kind == 1 {
            payload.extend([4, 0, 0, 0, 0, 0, 0, 0]);
        } else {
            payload.extend([0; 4]);
        }
        payload.extend([0; 4]);
        payload
    }

    let mut input = b"HWP Document File V3.00 \x1a\x01\x02\x03\x04\x05".to_vec();
    input.resize(30 + 128 + 1008 + 16, 0);
    short(&mut input, 30 + 6, 21000);
    short(&mut input, 30 + 8, 15000);
    short(&mut input, 30 + 14, 2000);
    let mut paragraph = [0u8; 43 + 14];
    paragraph[0] = 1;
    short(&mut paragraph, 1, 18);
    short(&mut paragraph, 3, 1);
    short(&mut paragraph, 12, 250);
    short(&mut paragraph, 43 + 6, 400);
    input.extend(paragraph);
    for (kind, order, x) in [(1, 4, 900), (3, 2, 800)] {
        input.extend([11, 0, 0, 0, 0, 0, 11, 0]);
        let payload = drawing(kind, order);
        let mut picture = [0u8; 348];
        picture[..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        picture[8] = 3;
        picture[74] = 3;
        short(&mut picture, 62, x);
        short(&mut picture, 64, 1000);
        input.extend(picture);
        input.extend(payload);
        input.extend([0; 43]);
    }
    for unit in [9, 450, 0, 9]
        .into_iter()
        .chain("Label".encode_utf16())
        .chain([13])
    {
        input.extend(unit.to_le_bytes());
    }
    input.extend([0; 43]);

    let doc = convert(&input, true).unwrap();
    let first = std::str::from_utf8(&doc.assets["hwp3/drawing/1"]).unwrap();
    let second = std::str::from_utf8(&doc.assets["hwp3/drawing/2"]).unwrap();
    assert!(
        first.contains("<line")
            && second.contains("<ellipse")
            && second.contains("fill=\"#ff0000\"")
    );
    let html = render(&doc, "html", "hybrid", &Properties::new()).unwrap();
    assert!(html.contains("viewBox=\"0 0 15000 21000\""));
    assert_eq!(html.matches("href=\"data:image/svg+xml;base64,").count(), 2);
    assert!(html.find("<image x=\"2790\"").unwrap() < html.find("<image x=\"2890\"").unwrap());
    assert!(html.contains("width:450px\"></span>Label"));
    assert_eq!(html.matches("Label").count(), 1);
    assert!(!html.contains("[그림]"));
    assert!(
        serde_json::to_string(&convert(&input, false).unwrap())
            .unwrap()
            .contains("[그림][그림]")
    );
}

#[test]
fn html_docx_table_widths_follow_the_source_grid_across_rowspans() {
    let doc = convert(&package(&[
        ("[Content_Types].xml", "<Types><Override PartName='/word/document.xml' ContentType='application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'/></Types>"),
        ("_rels/.rels", "<Relationships><Relationship Id='r' Type='rel/officeDocument' Target='word/document.xml'/></Relationships>"),
        ("word/document.xml", "<document><body><tbl><tblGrid><gridCol w='900'/><gridCol w='2700'/></tblGrid><tr><trPr><trHeight val='520'/></trPr><tc><tcPr><vMerge val='restart'/></tcPr><p><r><t>A</t></r></p></tc><tc><p><r><t>B</t></r></p></tc></tr><tr><tc><tcPr><vMerge/></tcPr><p/></tc><tc><p><r><t>C</t></r></p></tc></tr></tbl></body></document>"),
    ]), false).unwrap();
    let html = render(&doc, "html", "hybrid", &Properties::new()).unwrap();
    for value in [
        "width:180.0000pt;table-layout:fixed",
        "width:25.00000%",
        "width:75.00000%",
        "rowspan=\"2\"",
        "height:26pt;",
    ] {
        assert!(html.contains(value), "{value} in {html}");
    }
}

#[test]
fn html_pptx_keeps_layer_order_theme_fill_and_group_positions() {
    let input = package(&[
        (
            "[Content_Types].xml",
            "<Types><Override PartName='/ppt/presentation.xml' ContentType='application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml'/></Types>",
        ),
        (
            "_rels/.rels",
            "<Relationships><Relationship Id='r' Type='rel/officeDocument' Target='ppt/presentation.xml'/></Relationships>",
        ),
        (
            "ppt/presentation.xml",
            "<presentation xmlns:r='urn:relationships'><sldIdLst><sldId r:id='A'/></sldIdLst><sldSz cx='9525000' cy='4762500'/></presentation>",
        ),
        (
            "ppt/_rels/presentation.xml.rels",
            "<Relationships><Relationship Id='A' Type='rel/slide' Target='slides/slide1.xml'/><Relationship Id='B' Type='rel/theme' Target='theme/theme1.xml'/></Relationships>",
        ),
        (
            "ppt/slides/slide1.xml",
            "<sld><cSld><spTree><grpSp><grpSpPr><xfrm><off x='952500' y='476250'/><ext cx='1905000' cy='1905000'/><chOff x='0' y='0'/><chExt cx='952500' cy='952500'/></xfrm></grpSpPr><sp><spPr><xfrm><off x='95250' y='95250'/><ext cx='476250' cy='476250'/></xfrm><prstGeom prst='ellipse'/><solidFill><schemeClr val='accent1'/></solidFill></spPr></sp><sp><spPr><xfrm><off x='190500' y='190500'/><ext cx='666750' cy='476250'/></xfrm></spPr><txBody><p><r><t>Front label</t></r></p></txBody></sp></grpSp></spTree></cSld></sld>",
        ),
        (
            "ppt/theme/theme1.xml",
            "<theme><themeElements><clrScheme><accent1><srgbClr val='224466'/></accent1></clrScheme></themeElements></theme>",
        ),
    ]);
    let doc = convert(&input, true).unwrap();
    let html = render(&doc, "html", "hybrid", &Properties::new()).unwrap();
    assert!(html.contains("viewBox=\"0 0 1000 500\""));
    assert!(html.contains("matrix(2.000000 0.000000 0.000000 2.000000 120.000000 70.000000)"));
    assert!(html.find("fill=\"#224466\"").unwrap() < html.find("Front label").unwrap());
    assert_eq!(html.matches("Front label").count(), 1);
    assert!(md(&doc).contains("Front label"));
}

#[test]
fn hwpx_bold_labels_keep_literal_angle_brackets_in_markdown() {
    let doc = convert(
        &package(&[
            ("Contents/content.hpf", "<package/>"),
            (
                "Contents/header.xml",
                r#"<head><charPr id="1"><bold/></charPr></head>"#,
            ),
            (
                "Contents/section0.xml",
                r#"<sec><p><run charPrIDRef="1"><t>&lt;공통&gt;</t></run></p><p><run><t>1 &lt; 2 &gt; 0</t></run></p><p><run><t>&lt;script&gt;</t></run></p></sec>"#,
            ),
        ]),
        false,
    )
    .unwrap();
    assert_eq!(md(&doc), "**<공통>**\n\n1 < 2 > 0\n\n\\<script>\n\n");
    assert!(
        render(&doc, "html", "hybrid", &Properties::new())
            .unwrap()
            .contains("<strong>&lt;공통&gt;</strong>")
    );
}

#[test]
fn hwpx_keeps_real_underlines_and_ignores_blank_paragraphs() {
    let doc = convert(&package(&[
        ("Contents/content.hpf", "<package/>"),
        ("Contents/header.xml", r#"<head><charPr id="1"><underline type="NONE" shape="SOLID"/></charPr><charPr id="2"><underline type="BOTTOM" shape="SOLID"/></charPr></head>"#),
        ("Contents/section0.xml", r#"<sec><p><run charPrIDRef="1"><t>Plain</t></run></p><p><run><t> </t></run></p><p><run charPrIDRef="2"><t>Underlined</t></run></p></sec>"#),
    ]), false).unwrap();
    assert_eq!(md(&doc).trim(), "Plain\n\n<u>Underlined</u>");
}

#[test]
fn docx_empty_spacer_tables_and_paragraphs_are_omitted() {
    let doc = convert(&package(&[
        ("[Content_Types].xml", r#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#),
        ("_rels/.rels", r#"<Relationships><Relationship Id="a" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
        ("word/document.xml", r#"<document><body><p><r><t>Before</t></r></p><tbl><tr><tc><p/></tc></tr></tbl><p><r><t>&#160;</t></r></p><tbl><tr><tc><p><r><t>Real table</t></r></p></tc></tr></tbl><p><r><t>After</t></r></p></body></document>"#),
    ]), false).unwrap();
    assert_eq!(
        md(&doc).trim(),
        "Before\n\n| Real table |\n| --- |\n\nAfter"
    );
}
#[test]
#[ignore = "run tests/fetch-samples.ps1 once and then cargo test -- --ignored"]
fn real_documents_all_fourteen() {
    let cases = [
        ("FormattingShowcase.hwpx", "형식 테스트 문서"),
        ("LibreOfficeCalc_ods_1.3.ods", "example"),
        ("LibreOfficeImpress_odp_1.3.odp", "example Impress file"),
        ("testEPUB.epub", "Chapter 2"),
        ("testEXCEL.xls", "Squares"),
        ("testEXCEL.xlsx", "Squares"),
        ("testHWP_5.0.hwp", "테스트"),
        ("testOpenOffice2.odt", "NeoOffice"),
        ("testPDF.pdf", "Apache Tika"),
        ("testPPT.ppt", "Powerpoint"),
        ("testPPT.pptx", "Attachment Test"),
        ("testRTF.rtf", "indexation"),
        ("testWORD.doc", "Main Heading"),
        ("testWORD.docx", "Main Heading"),
    ];
    for (name, needle) in cases {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/corpus")
            .join(name);
        let d = rdocs2md::load(&path, true).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(d.format, path.extension().unwrap().to_str().unwrap());
        for mode in ["semantic", "hybrid", "layout"] {
            let s = render(&d, "md", mode, &Properties::new()).unwrap();
            assert!(s.contains(needle), "{name}/{mode}: missing {needle}");
        }
    }
}

#[test]
fn docx_nested_table_vertical_merge_notes_and_header() {
    let data = package(&[
        (
            "[Content_Types].xml",
            "<Types><Override PartName='/word/document.xml' ContentType='application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'/></Types>",
        ),
        (
            "_rels/.rels",
            "<Relationships><Relationship Id='r1' Target='word/document.xml' Type='rel/officeDocument'/></Relationships>",
        ),
        (
            "word/_rels/document.xml.rels",
            "<Relationships><Relationship Id='H' Target='header1.xml' Type='rel/header'/><Relationship Id='S' Target='styles.xml' Type='rel/styles'/></Relationships>",
        ),
        (
            "word/styles.xml",
            "<styles><style styleId='Base'><tblPr><tblBorders><insideV val='double' sz='24' color='FF0011'/><insideH val='single' sz='4'/></tblBorders></tblPr></style><style styleId='Grid'><basedOn val='Base'/></style></styles>",
        ),
        ("word/header1.xml", "<hdr><p><r><t>머리글</t></r></p></hdr>"),
        (
            "word/document.xml",
            "<document><body><tbl><tblPr><tblStyle val='Grid'/><tblBorders><top val='dashed' sz='12' color='112233'/><bottom val='single' sz='8'/></tblBorders></tblPr><tr><tc><tcPr><vMerge val='restart'/><shd fill='FFEECC'/><tcW w='1200' type='dxa'/></tcPr><p><pPr><jc val='center'/></pPr><r><t>위</t></r></p></tc><tc><tbl><tr><tc><p><r><t>안쪽</t></r></p></tc></tr></tbl></tc></tr><tr><tc><tcPr><vMerge/><tcBorders><bottom val='nil'/></tcBorders></tcPr></tc><tc><p><r><t>아래</t></r></p></tc></tr></tbl><sectPr><headerReference id='H'/></sectPr></body></document>",
        ),
    ]);
    let d = convert(&data, false).unwrap();
    let m = md(&d);
    assert_eq!(m.matches("<table>").count(), 2);
    assert!(m.contains("rowspan=\"2\""));
    assert!(m.find("머리글").unwrap() < m.find("안쪽").unwrap());
    let html = render(&d, "html", "hybrid", &Properties::new()).unwrap();
    assert_eq!(html.matches("class=\"source-borders\"").count(), 1);
    assert!(html.contains("rowspan=\"2\" style=\"background-color:#FFEECC;border-bottom:none;border-right:3pt double #FF0011;border-top:1.5pt dashed #112233;text-align:center;width:60pt;\""));
}
#[test]
fn odf_math_preserves_fraction_without_active_markup() {
    let d=convert(&package(&[("mimetype","application/vnd.oasis.opendocument.text"),
        ("content.xml","<doc><body><text><p><object href='./Object 1'/></p></text></body></doc>"),
        ("Object 1/content.xml","<math><semantics><mfrac><mi>a</mi><mi>b</mi></mfrac><annotation-xml><script>ignored</script></annotation-xml></semantics></math>")]),false).unwrap();
    let s = md(&d);
    // Formulas are rendered as LaTeX, never as active MathML markup.
    assert!(s.contains("\\frac{a}{b}"), "{s}");
    assert!(s.contains('$'));
    assert!(!s.contains("<mfrac>"));
    assert!(!s.contains("script"));
}
#[test]
fn rejects_unsafe_package_references() {
    let doc = package(&[
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            "<container><rootfile full-path='book.opf'/></container>",
        ),
        (
            "book.opf",
            "<package><manifest><item id='a' href='../outside.xhtml' media-type='application/xhtml+xml'/></manifest><spine><itemref idref='a'/></spine></package>",
        ),
    ]);
    assert!(convert(&doc, true).is_err());
}

#[test]
fn odt_explicit_page_break_preserves_exact_sides() {
    let data = package(&[
        ("mimetype", "application/vnd.oasis.opendocument.text"),
        (
            "content.xml",
            "<doc><body><text><p>앞<soft-page-break/>뒤</p></text></body></doc>",
        ),
    ]);
    let mut d = convert(&data, false).unwrap();
    assert_eq!(d.sections.len(), 2);
    d.select("2").unwrap();
    assert_eq!(md(&d).trim(), "뒤");
}
#[test]
fn xlsx_cached_formula_dates_sparse_cells_and_merge() {
    let data = package(&[
        (
            "[Content_Types].xml",
            "<Types><Override PartName='/xl/book.xml' ContentType='application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml'/></Types>",
        ),
        (
            "_rels/.rels",
            "<Relationships><Relationship Id='r' Type='rel/officeDocument' Target='xl/book.xml'/></Relationships>",
        ),
        (
            "xl/book.xml",
            "<workbook><sheets><sheet id='s' name='월별' state='veryHidden'/></sheets></workbook>",
        ),
        (
            "xl/_rels/book.xml.rels",
            "<Relationships><Relationship Id='s' Type='rel/worksheet' Target='sheet.xml'/><Relationship Id='f' Type='rel/styles' Target='styles.xml'/></Relationships>",
        ),
        (
            "xl/styles.xml",
            "<styles><borders><border><right style='mediumDashed'><color rgb='FF123456'/></right></border></borders><fills><fill><patternFill patternType='solid'><fgColor rgb='FFF0E0D0'/></patternFill></fill></fills><fonts><font><b/><sz val='14'/><color rgb='FF001122'/></font></fonts><cellXfs><xf numFmtId='14' borderId='0' fillId='0' fontId='0'><alignment horizontal='right' vertical='center'/></xf></cellXfs></styles>",
        ),
        (
            "xl/sheet.xml",
            "<worksheet><cols><col min='3' max='3' hidden='true'/></cols><sheetData><row r='1'><c r='A1' t='inlineStr'><is><t>날짜</t></is></c><c r='C1' s='0'><v>61</v></c></row><row r='3' hidden='1'><c r='A3'><f>SUM(1,2)</f><v>3</v></c></row></sheetData><mergeCells><mergeCell ref='A1:B1'/></mergeCells></worksheet>",
        ),
    ]);
    let d = convert(&data, false).unwrap();
    assert_eq!(d.sections[0].properties["hidden"], "true");
    assert_eq!(d.sections[0].properties["hidden_columns"], "3");
    assert_eq!(d.sections[0].properties["hidden_rows"], "3");
    assert_eq!(d.sections[0].blocks.len(), 2);
    let Block::Table { rows } = &d.sections[0].blocks[0] else {
        panic!()
    };
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][0].colspan, Some(2));
    assert_eq!(rows[0][1].runs[0].text, "1900-03-01");
    assert_eq!(rows[0][1].properties["hidden_column"], "true");
    let hits = to_value(Query::new("1900-03-01", false, 1).unwrap().find(&d)).unwrap();
    assert_eq!(hits["matches"][0]["address"], "C1");
    let Block::Table { rows } = &d.sections[0].blocks[1] else {
        panic!()
    };
    assert_eq!(rows[0][0].properties["formula"], "SUM(1,2)");
    assert_eq!(rows[0][0].properties["cached_value"], "3");
    assert_eq!(rows[0][0].properties["hidden_row"], "true");
    let html = render(&d, "html", "hybrid", &Properties::new()).unwrap();
    assert!(html.contains("background-color:#F0E0D0;border-right:2px dashed #123456;text-align:right;vertical-align:middle;"));
    assert!(html.contains("font-size:14pt;color:#001122;\"><strong>1900-03-01</strong>"));
}

#[test]
fn xlsx_csv_selects_sheets_and_quotes_fields() {
    let data = package(&[
        (
            "[Content_Types].xml",
            "<Types><Override PartName='/xl/book.xml' ContentType='application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml'/></Types>",
        ),
        (
            "_rels/.rels",
            "<Relationships><Relationship Id='r' Type='rel/officeDocument' Target='xl/book.xml'/></Relationships>",
        ),
        (
            "xl/book.xml",
            "<workbook><sheets><sheet id='a' name='매출'/><sheet id='b' name='Summary'/></sheets></workbook>",
        ),
        (
            "xl/_rels/book.xml.rels",
            "<Relationships><Relationship Id='a' Type='rel/worksheet' Target='a.xml'/><Relationship Id='b' Type='rel/worksheet' Target='b.xml'/></Relationships>",
        ),
        (
            "xl/a.xml",
            "<worksheet><sheetData><row r='1'><c r='A1' t='inlineStr'><is><t>이름</t></is></c><c r='B1' t='inlineStr'><is><t>값, 단위</t></is></c></row></sheetData></worksheet>",
        ),
        (
            "xl/b.xml",
            "<worksheet><sheetData><row r='1'><c r='A1'><v>42</v></c></row></sheetData></worksheet>",
        ),
    ]);
    let d = convert(&data, false).unwrap();
    let all = render(&d, "csv", "hybrid", &Properties::new()).unwrap();
    assert!(
        all.starts_with("# Sheet: 매출\n이름,\"값, 단위\"\n"),
        "{all}"
    );
    assert!(all.ends_with("# Sheet: Summary\n42\n"), "{all}");
    let d = convert_sheets(&data, false, None, &["summary".into()]).unwrap();
    assert_eq!(d.sections.len(), 1);
    assert_eq!(d.sections[0].title, "Summary");
    assert_eq!(
        render(&d, "csv", "hybrid", &Properties::new()).unwrap(),
        "42\n"
    );
    let d = convert_sheets(&data, false, None, &["1".into()]).unwrap();
    assert_eq!(d.sections.len(), 1);
    assert_eq!(d.sections[0].title, "매출");
    let d = convert_sheets(&data, false, None, &["2".into()]).unwrap();
    assert_eq!(d.sections.len(), 1);
    assert_eq!(d.sections[0].title, "Summary");
    assert!(convert_sheets(&data, false, None, &["0".into()]).is_err());
    assert!(convert_sheets(&data, false, None, &["3".into()]).is_err());
    assert!(convert_sheets(&data, false, None, &["없는 시트".into()]).is_err());
}

#[test]
fn pdf_outline_titles_become_markdown_headings() {
    use lopdf::{Document as Pdf, Object, Stream, dictionary};

    let mut pdf = Pdf::with_version("1.4");
    let tree = pdf.new_object_id();
    let font =
        pdf.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let resources = dictionary! {"Font"=>dictionary! {"F"=>font}};
    let content = "BT /F 16 Tf 1 0 0 1 30 180 Tm (Chapter One) Tj ET\n\
                   BT /F 10 Tf 1 0 0 1 30 150 Tm (First body paragraph.) Tj ET\n\
                   BT /F 14 Tf 1 0 0 1 30 100 Tm (Section A) Tj ET\n\
                   BT /F 10 Tf 1 0 0 1 30 70 Tm (Second body paragraph.) Tj ET";
    let stream = pdf.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page = pdf.add_object(dictionary! {"Type"=>"Page","Parent"=>tree,"Contents"=>stream,"Resources"=>resources,"MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]});
    pdf.objects.insert(tree, dictionary! {"Type"=>"Pages","Kids"=>vec![Object::Reference(page)],"Count"=>1,"MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]}.into());
    // A two-level bookmark tree, which is what most PDF exporters write.
    let child = pdf.new_object_id();
    let parent = pdf.new_object_id();
    pdf.objects.insert(child, dictionary! {"Title"=>Object::string_literal("Section A"),"Dest"=>vec![Object::Reference(page).into(),"XYZ".into(),0.into(),100.into(),0.into()]}.into());
    pdf.objects.insert(parent, dictionary! {"Title"=>Object::string_literal("Chapter One"),"Dest"=>vec![Object::Reference(page).into(),"XYZ".into(),0.into(),180.into(),0.into()],"First"=>Object::Reference(child)}.into());
    let outlines =
        pdf.add_object(dictionary! {"Type"=>"Outlines","First"=>Object::Reference(parent)});
    let catalog =
        pdf.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree,"Outlines"=>outlines});
    pdf.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();

    let s = md(&convert(&bytes, false).unwrap());
    assert!(s.contains("# Chapter One"), "{s}");
    assert!(s.contains("## Section A"), "{s}");
    assert!(s.contains("First body paragraph."), "{s}");
}

#[test]
fn pdf_raster_images_are_exported_as_png() {
    use lopdf::{Document as Pdf, Object, Stream, dictionary};

    let mut pdf = Pdf::with_version("1.4");
    let tree = pdf.new_object_id();
    let font =
        pdf.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // 2x2 DeviceRGB samples: red, green, blue, white.
    encoder
        .write_all(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255])
        .unwrap();
    let data = encoder.finish().unwrap();
    let image = pdf.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>2,"Height"=>2,"ColorSpace"=>"DeviceRGB","BitsPerComponent"=>8,"Filter"=>"FlateDecode"}, data));
    let resources =
        dictionary! {"Font"=>dictionary! {"F"=>font},"XObject"=>dictionary! {"Im"=>image}};
    let stream = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 10 0 0 10 30 150 cm /Im Do Q".to_vec(),
    ));
    let page = pdf.add_object(dictionary! {"Type"=>"Page","Parent"=>tree,"Contents"=>stream,"Resources"=>resources,"MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]});
    pdf.objects.insert(tree, dictionary! {"Type"=>"Pages","Kids"=>vec![Object::Reference(page)],"Count"=>1,"MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]}.into());
    let catalog = pdf.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree});
    pdf.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();

    let d = convert(&bytes, true).unwrap();
    let png = d.assets.values().next().expect("image asset");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG");
    assert_eq!(&png[16..20], &[0, 0, 0, 2], "width");
    assert_eq!(png[24], 8, "bit depth");
    assert_eq!(png[25], 2, "colour type");
}

/// Pixels of the filter-0, 8-bit RGB PNG files the converter writes.
fn png_pixels(png: &[u8]) -> (usize, usize, Vec<u8>) {
    use std::io::Read;

    let (mut width, mut height) = (0, 0);
    let mut data = Vec::new();
    let mut pos = 8;
    while pos + 8 <= png.len() {
        let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &png[pos + 4..pos + 8];
        let body = &png[pos + 8..pos + 8 + len];
        if kind == b"IHDR".as_slice() {
            width = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
            height = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
        } else if kind == b"IDAT".as_slice() {
            data.extend_from_slice(body);
        }
        pos += len + 12;
    }
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&data[..])
        .read_to_end(&mut raw)
        .unwrap();
    let stride = width * 3 + 1;
    assert_eq!(raw.len(), stride * height, "unexpected PNG payload");
    let pixels = raw
        .chunks_exact(stride)
        .flat_map(|row| &row[1..])
        .copied()
        .collect();
    (width, height, pixels)
}

#[test]
fn pdf_indexed_images_keep_their_palette_indices() {
    use lopdf::{Document as Pdf, Object, Stream, dictionary};

    // A 1 bit Indexed image whose eight pixels alternate between index 0 (red)
    // and index 1 (blue). Scaling the indices to 0-255 would push index 1 out
    // of the two entry palette, so those pixels would come out black.
    let mut pdf = Pdf::with_version("1.4");
    let tree = pdf.new_object_id();
    let lookup = pdf.add_object(Stream::new(dictionary! {}, vec![255, 0, 0, 0, 0, 255]));
    let image = pdf.add_object(Stream::new(
        dictionary! {
            "Type"=>"XObject","Subtype"=>"Image","Width"=>8,"Height"=>1,
            "BitsPerComponent"=>1,
            "ColorSpace"=>vec![
                "Indexed".into(),
                "DeviceRGB".into(),
                1.into(),
                Object::Reference(lookup),
            ],
        },
        vec![0b0101_0101],
    ));
    let resources = dictionary! {"XObject"=>dictionary! {"Im"=>image}};
    let stream = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 80 0 0 10 30 150 cm /Im Do Q".to_vec(),
    ));
    let page = pdf.add_object(dictionary! {"Type"=>"Page","Parent"=>tree,"Contents"=>stream,"Resources"=>resources,"MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]});
    pdf.objects.insert(tree, dictionary! {"Type"=>"Pages","Kids"=>vec![Object::Reference(page)],"Count"=>1,"MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]}.into());
    let catalog = pdf.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree});
    pdf.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();

    let d = convert(&bytes, true).unwrap();
    let png = d.assets.values().next().expect("image asset");
    let (width, height, pixels) = png_pixels(png);
    assert_eq!((width, height), (8, 1));
    for (i, pixel) in pixels.chunks_exact(3).enumerate() {
        let expected = if i % 2 == 0 { [255, 0, 0] } else { [0, 0, 255] };
        assert_eq!(pixel, &expected[..], "pixel {i}");
    }
}

#[test]
fn docx_comments_and_tracked_deletions_are_reported() {
    let data = package(&[
        (
            "[Content_Types].xml",
            "<Types><Override PartName='/word/document.xml' ContentType='application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'/></Types>",
        ),
        (
            "_rels/.rels",
            "<Relationships><Relationship Id='r1' Target='word/document.xml' Type='rel/officeDocument'/></Relationships>",
        ),
        (
            "word/_rels/document.xml.rels",
            "<Relationships><Relationship Id='C' Target='comments.xml' Type='http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments'/></Relationships>",
        ),
        (
            "word/comments.xml",
            "<comments><comment id='1' author='Kim' date='2024-01-02T03:04:05Z'><p><r><t>Please revise this paragraph.</t></r></p></comment></comments>",
        ),
        (
            "word/document.xml",
            "<document><body><p><r><t>Kept text</t></r><del><r><delText>Removed text</delText></r></del><commentRangeStart id='1'/><r><t>Reviewed text</t></r><commentRangeEnd id='1'/></p><sectPr/></body></document>",
        ),
    ]);
    let d = convert(&data, false).unwrap();
    let s = md(&d);
    assert!(
        s.contains("Kept text") && s.contains("Reviewed text"),
        "{s}"
    );
    assert!(!s.contains("Removed text"), "{s}");
    assert!(
        d.sections.iter().any(|s| s.kind == "comments"),
        "comments section"
    );
    assert!(s.contains("Please revise this paragraph."), "{s}");
    assert!(s.contains("Kim"), "{s}");
    assert!(
        d.warnings.iter().any(|w| w.contains("tracked deletions")),
        "{:?}",
        d.warnings
    );
}
