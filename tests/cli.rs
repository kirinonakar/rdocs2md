use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};
static N: AtomicUsize = AtomicUsize::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "rdocs2md-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_rdocs2md"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(Path::new(name))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn stdout_json_batch_and_input_collision() {
    let work = Scratch::new();
    let bytes = br"{\rtf1\ansi first\page second}";
    for name in ["한글.unknown", "second.rtf"] {
        fs::write(work.path(name), bytes).unwrap()
    }
    let one = work.run(&[
        "한글.unknown",
        "--pages",
        "2",
        "--stdout",
        "--json",
        "data.json",
    ]);
    assert!(one.status.success(), "{:?}", one.stderr);
    assert_eq!(String::from_utf8(one.stdout).unwrap().trim(), "second");
    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(work.path("data.json")).unwrap()).unwrap();
    assert_eq!(doc["format"], "rtf");
    assert_eq!(doc["sections"].as_array().unwrap().len(), 1);
    assert!(
        work.run(&[
            "한글.unknown",
            "second.rtf",
            "--out-dir",
            "results",
            "--format",
            "html"
        ])
        .status
        .success()
    );
    assert!(work.path("results/한글.html").is_file());
    assert!(work.path("results/second.html").is_file());
    assert!(
        !work
            .run(&["second.rtf", "-o", "second.rtf"])
            .status
            .success()
    );
    assert_eq!(fs::read(work.path("second.rtf")).unwrap(), bytes);
    assert_eq!(work.run(&["--bad"]).status.code(), Some(2));
}
#[test]
fn plain_text_hwp_keeps_korean_and_line_breaks() {
    let work = Scratch::new();
    let bytes = b"\xc7\xd1\xb1\xdb \x8c\x63\r\n\r\n <tag> & text\r\n";
    fs::write(work.path("novel.HWP"), bytes).unwrap();
    let result = work.run(&[
        "novel.HWP",
        "--format",
        "html",
        "--layout",
        "document",
        "--stdout",
    ]);
    assert!(result.status.success(), "{:?}", result.stderr);
    let html = String::from_utf8(result.stdout).unwrap();
    assert!(html.contains("data-rdocs-layout=\"document\" data-format=\"txt\""));
    assert!(html.contains("한글 똠<br><br> &lt;tag&gt; &amp; text<br>"));

    fs::write(work.path("same.bin"), bytes).unwrap();
    fs::write(work.path("binary.hwp"), b"text\0hidden").unwrap();
    for name in ["same.bin", "binary.hwp"] {
        assert_eq!(work.run(&[name, "--stdout"]).status.code(), Some(1));
    }
}

#[test]
fn document_layout_preserves_page_breaks_and_requires_html() {
    let work = Scratch::new();
    fs::write(work.path("pages.rtf"), br"{\rtf1 first\page second}").unwrap();
    let flow = work.run(&["pages.rtf", "--format", "html", "--stdout"]);
    assert_eq!(
        flow.stdout,
        work.run(&[
            "pages.rtf",
            "--format",
            "html",
            "--layout",
            "flow",
            "--stdout"
        ])
        .stdout
    );
    let paged = work.run(&[
        "pages.rtf",
        "--format",
        "html",
        "--layout",
        "document",
        "--stdout",
    ]);
    assert!(paged.status.success(), "{:?}", paged.stderr);
    let html = String::from_utf8(paged.stdout).unwrap();
    assert!(html.contains("data-rdocs-layout=\"document\""));
    assert_eq!(html.matches("<section class=\"document-page\">").count(), 2);
    assert!(html.contains("first</p>") && html.contains("second</p>"));
    assert!(!html.contains("<hr class=\"page-break\">"));
    assert_eq!(
        work.run(&["pages.rtf", "--layout", "document"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        work.run(&["pages.rtf", "--format", "html", "--layout", "invalid"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn extract_raster_and_reuse_existing_result() {
    let d = Scratch::new();
    fs::write(d.path("pic.rtf"),br"{\rtf1{\pict 47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b}}").unwrap();
    for _ in 0..2 {
        let result = d.run(&["pic.rtf", "--image-dir", "이미지", "-o", "pic.md"]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let s = fs::read_to_string(d.path("pic.md")).unwrap();
    assert!(s.contains("이미지/image-1.gif"));
    assert!(
        fs::read(d.path("이미지/image-1.gif"))
            .unwrap()
            .starts_with(b"GIF89a")
    );
    assert_eq!(fs::read_dir(d.path("이미지")).unwrap().count(), 1);
}

#[test]
fn include_images_embeds_html_pictures_and_only_saves_explicit_extractions() {
    let d = Scratch::new();
    fs::write(d.path("pic.rtf"),br"{\rtf1{\pict 47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b}}").unwrap();
    let result = d.run(&[
        "pic.rtf",
        "--format",
        "html",
        "--include-images",
        "-o",
        "pic.html",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let html = fs::read_to_string(d.path("pic.html")).unwrap();
    assert!(
        html.contains("src=\"data:image/gif;base64,R0lGOD"),
        "{html}"
    );
    assert!(!html.contains("data-path="), "{html}");
    assert!(!d.path("pic_images").exists());
    let result = d.run(&[
        "pic.rtf",
        "--format",
        "html",
        "--include-images",
        "--extract-images",
        "-o",
        "extracted.html",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let html = fs::read_to_string(d.path("extracted.html")).unwrap();
    assert!(
        html.contains("src=\"data:image/gif;base64,R0lGOD"),
        "{html}"
    );
    assert!(
        html.contains("data-path=\"pic_images/image-1.gif\""),
        "{html}"
    );
    assert!(d.path("pic_images/image-1.gif").is_file());
    let result = d.run(&["pic.rtf", "--format", "html", "--stdout"]);
    assert!(result.status.success());
    let inline = String::from_utf8(result.stdout).unwrap();
    assert!(inline.contains("src=\"data:image/gif;base64,R0lGOD"));
    assert!(!inline.contains("data-path="));
}

#[test]
fn csv_and_sheet_options_require_a_workbook() {
    let work = Scratch::new();
    fs::write(work.path("note.rtf"), br"{\rtf1 text}").unwrap();
    let csv = work.run(&["note.rtf", "--format", "csv", "--stdout"]);
    assert_eq!(csv.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&csv.stderr).contains("xls, xlsx and ods"),
        "{}",
        String::from_utf8_lossy(&csv.stderr)
    );
    let sheet = work.run(&["note.rtf", "--sheet", "Sheet1", "--stdout"]);
    assert_eq!(sheet.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&sheet.stderr).contains("sheet selection"),
        "{}",
        String::from_utf8_lossy(&sheet.stderr)
    );
    assert_eq!(
        work.run(&["note.rtf", "--format", "tsv"]).status.code(),
        Some(2)
    );
}

#[test]
fn recursive_batch_with_destinations_and_stdout() {
    let work = Scratch::new();
    fs::create_dir_all(work.path("docs/sub")).unwrap();
    let picture = br"{\rtf1 report {\pict 47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b}}";
    fs::write(work.path("docs/same.rtf"), picture).unwrap();
    fs::write(work.path("docs/sub/same.rtf"), picture).unwrap();
    fs::write(work.path("docs/sub/notes.txt"), "not a document").unwrap();
    let result = work.run(&[
        "docs",
        "--recursive",
        "-o",
        "converted",
        "--json",
        "ir",
        "--image-dir",
        "images",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for name in ["same", "same-2"] {
        let markdown = fs::read_to_string(work.path(&format!("converted/{name}.md"))).unwrap();
        assert!(markdown.contains(&format!("../images/{name}/image-1.gif")));
        assert!(work.path(&format!("ir/{name}.json")).is_file());
    }
    fs::write(work.path("docs/sub/bad.rtf"), "corrupt").unwrap();
    let result = work.run(&["docs/**/*.rtf", "--stdout", "--json", "stdout-ir"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("bad.rtf"));
    let lines: Vec<Value> = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|line| line["content"].as_str().unwrap().contains("report"))
    );
    assert_eq!(fs::read_dir(work.path("stdout-ir")).unwrap().count(), 2);

    let one = work.run(&["docs/s?me.[r]tf", "-o", "one-match"]);
    assert!(
        one.status.success(),
        "{}",
        String::from_utf8_lossy(&one.stderr)
    );
    assert!(work.path("one-match/same.md").is_file());
}

#[test]
fn parallel_batch_keeps_input_order() {
    let work = Scratch::new();
    let names = ["a.rtf", "b.rtf", "c.rtf", "d.rtf"];
    for (i, name) in names.iter().enumerate() {
        fs::write(work.path(name), format!("{{\\rtf1 document {i}}}")).unwrap();
    }
    let result = work.run(&[
        "a.rtf", "b.rtf", "c.rtf", "d.rtf", "--stdout", "--jobs", "4",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), names.len());
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line["input"].as_str().unwrap().ends_with(names[i]),
            "{line}"
        );
        assert!(
            line["content"]
                .as_str()
                .unwrap()
                .contains(&format!("document {i}")),
            "{line}"
        );
    }
    assert_eq!(work.run(&["a.rtf", "--jobs", "0"]).status.code(), Some(2));
    assert_eq!(work.run(&["a.rtf", "--jobs", "65"]).status.code(), Some(2));
    let one = work.run(&["a.rtf", "b.rtf", "-o", "out", "--jobs", "1"]);
    assert!(
        one.status.success(),
        "{}",
        String::from_utf8_lossy(&one.stderr)
    );
    assert!(work.path("out/a.md").is_file() && work.path("out/b.md").is_file());
    assert_eq!(
        work.run(&["a.rtf", "--max-inflight-bytes", "bogus"])
            .status
            .code(),
        Some(2)
    );
    // A tiny budget still converts every input, just with fewer workers.
    let limited = work.run(&[
        "a.rtf",
        "b.rtf",
        "-o",
        "limited",
        "--jobs",
        "8",
        "--max-inflight-bytes",
        "1",
    ]);
    assert!(
        limited.status.success(),
        "{}",
        String::from_utf8_lossy(&limited.stderr)
    );
    assert!(work.path("limited/a.md").is_file() && work.path("limited/b.md").is_file());
}

#[test]
fn cli_search_reports_original_page_and_context() {
    let work = Scratch::new();
    fs::write(
        work.path("sample.rtf"),
        br"{\rtf1 skipped\page Before {\b Tar}get after\page Target again}",
    )
    .unwrap();
    let result = work.run(&[
        "sample.rtf",
        "--search",
        "target",
        "--pages",
        "2-3",
        "--limit",
        "1",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let response: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["truncated"], true);
    assert_eq!(response["matches"][0]["text"], "Target");
    assert_eq!(response["matches"][0]["before"], "Before ");
    assert_eq!(response["matches"][0]["after"], " after");
    assert_eq!(response["matches"][0]["section_index"], 2);
    assert_eq!(response["matches"][0]["path"], json!([{"block":1}]));
}

#[test]
fn mcp_advertises_and_renders_output_formats() {
    let work = Scratch::new();
    fs::write(
        work.path("sample.rtf"),
        br"{\rtf1\ansi First page\page Second page}",
    )
    .unwrap();
    fs::create_dir_all(work.path("docs/sub")).unwrap();
    fs::copy(work.path("sample.rtf"), work.path("docs/sub/copy.rtf")).unwrap();
    fs::write(work.path("docs/broken.rtf"), "corrupt").unwrap();
    fs::write(work.path("pic.rtf"), br"{\rtf1{\pict 47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b}}").unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_rdocs2md"))
        .current_dir(&work.0)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request =
        |id, method, params| json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
    let convert = |id, args| {
        request(
            id,
            "tools/call",
            json!({"name":"convert_document","arguments":args}),
        )
    };
    let mut stdin = server.stdin.take().unwrap();
    for message in [
        request(1, "initialize", json!({"protocolVersion":"2025-11-25"})),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        request(2, "tools/list", json!({})),
        convert(3, json!({"path":"sample.rtf"})),
        convert(
            4,
            json!({"path":"sample.rtf","format":"html","output_path":"sample.html"}),
        ),
        request(
            5,
            "tools/call",
            json!({"name":"extract_document_range","arguments":{
                "path":"sample.rtf","range":"2","format":"json"
            }}),
        ),
        convert(6, json!({"path":"sample.rtf","format":"txt"})),
        convert(
            7,
            json!({"path":"sample.rtf","format":"json","output_path":"wrong.md"}),
        ),
        convert(
            8,
            json!({"path":"sample.rtf","format":"html","output_path":"sample.html"}),
        ),
        request(
            9,
            "tools/call",
            json!({"name":"search_document","arguments":{
                "path":"sample.rtf","query":"SECOND","range":"2","limit":1
            }}),
        ),
        request(
            10,
            "tools/call",
            json!({"name":"search_document","arguments":{
                "path":"sample.rtf","query":"text","limit":0
            }}),
        ),
        request(
            11,
            "tools/call",
            json!({"name":"convert_documents","arguments":{
                "paths":["docs/*.rtf","sample.rtf","../*.rtf"],"recursive":true,"output_dir":"batch","format":"json","jobs":2
            }}),
        ),
        convert(
            12,
            json!({"path":"pic.rtf","format":"html","include_images":true}),
        ),
        convert(
            13,
            json!({"path":"pic.rtf","format":"html","include_images":true,"output_path":"pic.html"}),
        ),
        request(
            14,
            "tools/call",
            json!({"name":"convert_documents","arguments":{
                "paths":["pic.rtf"],"output_dir":"html-batch","format":"html","include_images":true
            }}),
        ),
        convert(
            15,
            json!({"path":"pic.rtf","format":"html","include_images":true,"image_dir":"saved-images"}),
        ),
        request(
            16,
            "tools/call",
            json!({"name":"search_documents","arguments":{
                "paths":["docs/**/*.rtf","docs/sub/copy.rtf","sample.rtf","../*.rtf"],
                "query":"second","range":"2","recursive":true,"limit":1
            },"_meta":{"progressToken":"batch-search"}}),
        ),
    ] {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);

    let output = server.wait_with_output().unwrap();
    assert!(output.status.success());
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let progress: Vec<_> = replies
        .iter()
        .filter(|reply| reply["method"] == "notifications/progress")
        .collect();
    assert!(progress.len() >= 2);
    assert!(progress.windows(2).all(|pair| {
        pair[0]["params"]["progress"].as_u64() < pair[1]["params"]["progress"].as_u64()
    }));
    assert_eq!(
        progress[0]["params"],
        json!({"progressToken":"batch-search","progress":0})
    );
    assert_eq!(
        progress.last().unwrap()["params"],
        json!({"progressToken":"batch-search","progress":3,"total":3})
    );
    assert_eq!(
        replies
            .iter()
            .filter(|reply| reply.get("id").is_some())
            .count(),
        16
    );
    let reply = |id: usize| replies.iter().find(|reply| reply["id"] == id).unwrap();
    let result = |id: usize| &reply(id)["result"];
    let content = |id| result(id)["content"][0]["text"].as_str().unwrap();
    for tool in result(2)["tools"].as_array().unwrap() {
        let schema = &tool["inputSchema"];
        let format = &schema["properties"]["format"];
        match tool["name"].as_str().unwrap() {
            "convert_document" | "extract_document_range" | "convert_documents" => {
                assert_eq!(format["enum"], json!(["md", "html", "json", "csv"]));
                assert_eq!(format["default"], "md");
                assert!(
                    !schema["required"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("format"))
                );
            }
            _ => assert!(format.is_null()),
        }
        if tool["name"] == "convert_documents" {
            let jobs = &schema["properties"]["jobs"];
            assert_eq!(jobs["type"], "integer");
            let bounds = (jobs["minimum"].as_u64(), jobs["maximum"].as_u64());
            assert_eq!(bounds, (Some(1), Some(64)));
        }
    }
    assert_eq!(result(3)["isError"], false);
    assert!(content(3).starts_with("First page"));
    // Conversion tools answer with text only, so the body is not sent twice.
    assert!(result(3).get("structuredContent").is_none());
    assert_eq!(result(4)["isError"], false);
    assert!(result(4).get("structuredContent").is_none());
    let html = fs::read_to_string(work.path("sample.html")).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("First page"));
    assert_eq!(result(5)["isError"], false);
    let document: Value = serde_json::from_str(content(5)).unwrap();
    assert_eq!(document["sections"].as_array().unwrap().len(), 1);
    assert!(content(5).contains("Second page"));
    assert!(!content(5).contains("First page"));
    // Section indexes survive range extraction in the JSON IR.
    assert_eq!(document["sections"][0]["index"], 2);
    // Range extraction is text-only as well.
    assert!(result(5).get("structuredContent").is_none());
    assert_eq!(reply(6)["error"]["code"], -32602);
    assert_eq!(result(7)["isError"], true);
    assert!(content(7).contains("new .json file"));
    assert!(!work.path("wrong.md").exists());
    assert_eq!(result(8)["isError"], true);
    assert_eq!(result(9)["isError"], false);
    let search: Value = serde_json::from_str(content(9)).unwrap();
    assert_eq!(search["matches"][0]["section_index"], 2);
    assert_eq!(search["matches"][0]["text"], "Second");
    // Citations carry the source section, a stable block id and the page.
    assert_eq!(search["matches"][0]["source_section_index"], 2);
    assert_eq!(search["matches"][0]["block_id"], "s2-b1");
    assert!(result(9).get("structuredContent").is_none());
    assert_eq!(reply(10)["error"]["code"], -32602);
    assert_eq!(result(11)["isError"], false);
    let batch: Value = serde_json::from_str(content(11)).unwrap();
    assert_eq!(batch["succeeded"], 2, "{batch}");
    assert_eq!(batch["failed"], 2, "{batch}");
    assert!(result(11).get("structuredContent").is_none());
    assert!(content(11).contains("outside the MCP root"));
    assert!(work.path("batch/copy.json").is_file());
    assert!(work.path("batch/sample.json").is_file());
    assert!(!work.path("batch/broken.json").exists());
    for id in 12..=15 {
        assert_eq!(result(id)["isError"], false, "{}", content(id));
    }
    let saved = fs::read_to_string(work.path("pic.html")).unwrap();
    let batch = fs::read_to_string(work.path("html-batch/pic.html")).unwrap();
    for html in [content(12), &saved, &batch] {
        assert!(
            html.contains("src=\"data:image/gif;base64,R0lGOD"),
            "{html}"
        );
        assert!(!html.contains("data-path="), "{html}");
    }
    assert!(!work.path("pic_images").exists());
    assert!(!work.path("html-batch/pic_images").exists());
    assert!(content(15).contains("src=\"data:image/gif;base64,R0lGOD"));
    assert!(content(15).contains("data-path=\"saved-images/image-1.gif\""));
    assert!(work.path("saved-images/image-1.gif").is_file());
    let batch_search: Value = serde_json::from_str(content(16)).unwrap();
    assert_eq!(batch_search["succeeded"], 2);
    assert_eq!(batch_search["failed"], 2);
    assert_eq!(batch_search["truncated"], true);
    let files = batch_search["results"].as_array().unwrap();
    assert!(files.iter().any(|file| {
        file["error"]
            .as_str()
            .is_some_and(|error| error.contains("outside the MCP root"))
    }));
    let matched: Vec<_> = files
        .iter()
        .filter(|file| file.get("result").is_some())
        .collect();
    assert!(matched[0]["input"].as_str().unwrap().ends_with("copy.rtf"));
    assert_eq!(
        matched[0]["result"]["matches"][0]["source_section_index"],
        2
    );
    assert_eq!(matched[1]["result"]["matches"], json!([]));
    assert_eq!(matched[1]["result"]["truncated"], true);
    // No tool returns structuredContent, so no result is sent twice.
    for id in 1..=16 {
        assert!(result(id).get("structuredContent").is_none(), "id {id}");
    }
}
