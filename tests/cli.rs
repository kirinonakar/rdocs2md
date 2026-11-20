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
fn mcp_advertises_and_renders_output_formats() {
    let work = Scratch::new();
    fs::write(
        work.path("sample.rtf"),
        br"{\rtf1\ansi First page\page Second page}",
    )
    .unwrap();
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
    assert_eq!(replies.len(), 8);
    let result = |id: usize| &replies[id - 1]["result"];
    let content = |id| result(id)["content"][0]["text"].as_str().unwrap();
    for tool in result(2)["tools"].as_array().unwrap() {
        let schema = &tool["inputSchema"];
        let format = &schema["properties"]["format"];
        match tool["name"].as_str().unwrap() {
            "convert_document" | "extract_document_range" => {
                assert_eq!(format["enum"], json!(["md", "html", "json"]));
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
    }
    assert_eq!(result(3)["isError"], false);
    assert!(content(3).starts_with("First page"));
    assert_eq!(result(4)["isError"], false);
    let html = fs::read_to_string(work.path("sample.html")).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("First page"));
    assert_eq!(result(5)["isError"], false);
    let document: Value = serde_json::from_str(content(5)).unwrap();
    assert_eq!(document["sections"].as_array().unwrap().len(), 1);
    assert!(content(5).contains("Second page"));
    assert!(!content(5).contains("First page"));
    assert_eq!(replies[5]["error"]["code"], -32602);
    assert_eq!(result(7)["isError"], true);
    assert!(content(7).contains("new .json file"));
    assert!(!work.path("wrong.md").exists());
    assert_eq!(result(8)["isError"], true);
}
