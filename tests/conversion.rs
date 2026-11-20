use rdocs2md::{convert, model::*, render::render};
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

#[test]
fn pdf_spaced_glyphs_and_ruled_headings_form_a_table() {
    use lopdf::{Document as Pdf, Stream, dictionary};

    let mut pdf = Pdf::with_version("1.4");
    let tree = pdf.new_object_id();
    let font =
        pdf.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let resources = dictionary! {"Font"=>dictionary! {"F"=>font}};
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
    let stream = pdf.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let page = pdf.add_object(dictionary! {
        "Type"=>"Page","Parent"=>tree,"Contents"=>stream,"Resources"=>resources,
        "MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]
    });
    pdf.objects.insert(
        tree,
        dictionary! {"Type"=>"Pages","Kids"=>vec![lopdf::Object::Reference(page)],"Count"=>1}
            .into(),
    );
    let catalog = pdf.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree});
    pdf.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();

    let doc = convert(&bytes, false).unwrap();
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
    let values = |row: &[Cell]| {
        row.iter()
            .map(|cell| {
                cell.runs
                    .iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(values(&rows[0]), ["", "Reader A", "Reader B"]);
    assert_eq!(
        values(&rows[3]),
        ["9", "0", "1", "2", "3", "4", "5", "6", "7"]
    );
    assert_eq!(
        values(&rows[4]),
        ["10", "7", "6", "5", "4", "3", "2", "1", "0"]
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
        record(out, 72, level, &data);
    }
    fn paragraph(out: &mut Vec<u8>, level: u32, text: &str) {
        record(out, 66, level, &[]);
        let text: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        record(out, 67, level + 1, &text);
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
    table(&mut raw, 3, 1, 1);
    cell(&mut raw, 4, [0, 0, 1, 1]);
    paragraph(&mut raw, 4, "Nested answer");
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
    let doc = convert(&file.into_inner().into_inner(), false).unwrap();
    let text = md(&doc);
    assert!(text.contains("<td rowspan=\"2\">1</td><td colspan=\"2\">Question and answer</td>"));
    assert!(text.contains("<td>Question</td><td><p>Answer</p>\n<table>"));
    assert!(text.contains("<td>Nested answer</td>"));
    assert!(text.contains("</table>\n<p>Answer continued</p>\n</td>"));
    assert_eq!(text.matches("\n\n").count(), 3);
    assert!(text.starts_with("Before\n\n<table>") && text.ends_with("After\n\n"));
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
            "<Relationships><Relationship Id='H' Target='header1.xml' Type='rel/header'/></Relationships>",
        ),
        ("word/header1.xml", "<hdr><p><r><t>머리글</t></r></p></hdr>"),
        (
            "word/document.xml",
            "<document><body><tbl><tr><tc><tcPr><vMerge val='restart'/></tcPr><p><r><t>위</t></r></p></tc><tc><tbl><tr><tc><p><r><t>안쪽</t></r></p></tc></tr></tbl></tc></tr><tr><tc><tcPr><vMerge/></tcPr></tc><tc><p><r><t>아래</t></r></p></tc></tr></tbl><sectPr><headerReference id='H'/></sectPr></body></document>",
        ),
    ]);
    let d = convert(&data, false).unwrap();
    let m = md(&d);
    assert_eq!(m.matches("<table>").count(), 2);
    assert!(m.contains("rowspan=\"2\""));
    assert!(m.find("머리글").unwrap() < m.find("안쪽").unwrap());
}
#[test]
fn odf_math_preserves_fraction_without_active_markup() {
    let d=convert(&package(&[("mimetype","application/vnd.oasis.opendocument.text"),
        ("content.xml","<doc><body><text><p><object href='./Object 1'/></p></text></body></doc>"),
        ("Object 1/content.xml","<math><semantics><mfrac><mi>a</mi><mi>b</mi></mfrac><annotation-xml><script>ignored</script></annotation-xml></semantics></math>")]),false).unwrap();
    let s = md(&d);
    assert!(s.contains("<mfrac><mi>a</mi><mi>b</mi></mfrac>"));
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
            "<workbook><sheets><sheet id='s' name='월별'/></sheets></workbook>",
        ),
        (
            "xl/_rels/book.xml.rels",
            "<Relationships><Relationship Id='s' Type='rel/worksheet' Target='sheet.xml'/><Relationship Id='f' Type='rel/styles' Target='styles.xml'/></Relationships>",
        ),
        (
            "xl/styles.xml",
            "<styles><cellXfs><xf numFmtId='14'/></cellXfs></styles>",
        ),
        (
            "xl/sheet.xml",
            "<worksheet><sheetData><row r='1'><c r='A1' t='inlineStr'><is><t>날짜</t></is></c><c r='C1' s='0'><v>61</v></c></row><row r='3'><c r='A3'><f>SUM(1,2)</f><v>3</v></c></row></sheetData><mergeCells><mergeCell ref='A1:B1'/></mergeCells></worksheet>",
        ),
    ]);
    let d = convert(&data, false).unwrap();
    assert_eq!(d.sections[0].blocks.len(), 2);
    let Block::Table { rows } = &d.sections[0].blocks[0] else {
        panic!()
    };
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][0].colspan, Some(2));
    assert_eq!(rows[0][1].runs[0].text, "1900-03-01");
    let Block::Table { rows } = &d.sections[0].blocks[1] else {
        panic!()
    };
    assert_eq!(rows[0][0].properties["formula"], "SUM(1,2)");
    assert_eq!(rows[0][0].properties["cached_value"], "3");
}
