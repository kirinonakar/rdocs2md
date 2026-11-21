use crate::{
    batch, load_range, model::*, output, parallel, recover_document, render::render, search,
    support,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::{self, BufRead, Write},
    panic::AssertUnwindSafe,
    path::{Component, Path, PathBuf},
};
const INLINE: usize = 512 * 1024;
const FRAME: u64 = 1024 * 1024;
fn definitions(allow_outside_root: bool) -> Vec<Value> {
    let scope = if allow_outside_root {
        "Local paths may be outside the root. Relative paths start at the root."
    } else {
        "Paths must be within the configured root."
    };
    let mut tools = Vec::new();
    for (name, description) in [
        (
            "convert_document",
            "Convert a local document to Markdown, HTML, or JSON. Supply a new output_path for large results.",
        ),
        (
            "inspect_document",
            "Read format, metadata, warnings and the section index.",
        ),
        (
            "extract_document_range",
            "Convert a 1-based page, slide, sheet, chapter or section range.",
        ),
        (
            "search_document",
            "Search extracted paragraph and cell text for a literal query. Return snippets and locations.",
        ),
        (
            "convert_documents",
            "Convert a batch of files, directories and wildcard patterns. Save each result and report per-file success or failure.",
        ),
        (
            "list_supported_formats",
            "List the implemented formats, features and limitations.",
        ),
    ] {
        let mut props = json!({});
        let mut required = vec![];
        if name == "convert_documents" {
            props["paths"] = json!({"type":"array","items":{"type":"string"},"minItems":1,"description":format!("Files, directories or wildcard patterns supporting *, ?, [], and **. {scope}")});
            props["recursive"] = json!({"type":"boolean","description":"Scan subdirectories for directory and wildcard inputs."});
            props["output_dir"] = json!({"type":"string","description":format!("Output directory. Its parent must exist if the directory is new. {scope}")});
            props["jobs"] = json!({"type":"integer","minimum":1,"maximum":parallel::MAX_JOBS,"description":"Parallel conversion workers. Defaults to the available CPU count."});
            required.extend(["paths", "output_dir"]);
        } else if name != "list_supported_formats" {
            props["path"] =
                json!({"type":"string","description":format!("Input document. {scope}")});
            required.push("path")
        }
        if matches!(
            name,
            "convert_document" | "extract_document_range" | "convert_documents"
        ) {
            props["format"] = json!({
                "type":"string",
                "enum":["md","html","json"],
                "default":"md",
                "description":"Output format: Markdown, HTML, or JSON."
            });
            props["mode"] = json!({"type":"string","enum":["semantic","hybrid","layout"]});
            props["extract_images"] = json!({"type":"boolean"});
            props["image_dir"] = json!({"type":"string","description":format!("Image directory. Its parent must exist. {scope}")});
            props["range"] =
                json!({"type":"string","description":"1-based inclusive ranges, e.g. 1-3,5."});
            if name != "convert_documents" {
                props["output_path"] = json!({"type":"string","description":format!("New filename with an extension that matches format. {scope}")});
            }
        }
        if name == "search_document" {
            props["query"] = json!({"type":"string","minLength":1,"maxLength":1024});
            props["case_sensitive"] = json!({"type":"boolean","default":false});
            props["limit"] = json!({"type":"integer","minimum":1,"maximum":search::MAX_LIMIT,"default":search::DEFAULT_LIMIT});
            props["range"] = json!({"type":"string","description":"Only search these 1-based sections, e.g. 1-3,5."});
            required.push("query");
        }
        if name == "extract_document_range" {
            required.push("range")
        }
        tools.push(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":props,"required":required,"additionalProperties":false}}));
    }
    tools
}
struct Access {
    root: PathBuf,
    allow_outside_root: bool,
}
impl Access {
    fn input_path(&self, path: &str) -> Result<PathBuf> {
        if path.is_empty()
            || path
                .as_bytes()
                .get(..2)
                .is_some_and(|p| p.iter().all(|c| matches!(c, b'\\' | b'/')))
            || path.contains('\0')
        {
            return Err("expected a local filesystem path".into());
        }
        let p = Path::new(path);
        #[cfg(windows)]
        for c in p.components() {
            if let Component::Normal(v) = c {
                let n = v.to_string_lossy();
                let base = n
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .trim_end()
                    .to_ascii_uppercase();
                if n.contains(':')
                    || n.ends_with([' ', '.'])
                    || matches!(
                        base.as_str(),
                        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                    )
                    || (["COM", "LPT"].iter().any(|p| {
                        base.strip_prefix(p)
                            .is_some_and(|v| v.len() == 1 && v.as_bytes()[0].is_ascii_digit())
                    }))
                {
                    return Err("invalid Windows file name".into());
                }
            }
        }
        Ok(self.root.join(p))
    }

    fn scoped(&self, path: PathBuf) -> Result<PathBuf> {
        if !self.allow_outside_root && !path.starts_with(&self.root) {
            return Err("path is outside the MCP root".into());
        }
        Ok(path)
    }

    fn local(&self, path: &str, new: bool) -> Result<PathBuf> {
        let full = self.input_path(path)?;
        let path = if new {
            let name = full.file_name().ok_or("expected output name")?;
            output::parent(&full).canonicalize()?.join(name)
        } else {
            full.canonicalize()?
        };
        let path = self.scoped(path)?;
        if !self.allow_outside_root
            && new
            && path.exists()
            && !path.canonicalize()?.starts_with(&self.root)
        {
            return Err("output resolves outside the MCP root".into());
        }
        Ok(path)
    }

    fn directory(&self, path: &str) -> Result<PathBuf> {
        let path = self.local(path, true)?;
        fs::create_dir_all(&path)?;
        self.scoped(path.canonicalize()?)
    }
}

struct Conversion<'a> {
    format: &'a str,
    mode: &'a str,
    range: Option<&'a str>,
}

impl Conversion<'_> {
    fn render(
        &self,
        input: &Path,
        out: Option<&Path>,
        dir: Option<&Path>,
        root: &Path,
    ) -> Result<(String, Vec<String>)> {
        let d = load_range(input, dir.is_some(), self.range)?;
        let links = if let Some(dir) = dir {
            output::extract(&d, dir, out.map(output::parent).unwrap_or(root))?
        } else {
            Properties::new()
        };
        let value = render(&d, self.format, self.mode, &links)?;
        Ok((value, d.warnings))
    }

    fn save(&self, input: &Path, out: &Path, dir: Option<&Path>, root: &Path) -> Result<Value> {
        if out.exists()
            || out
                .extension()
                .is_none_or(|v| !v.eq_ignore_ascii_case(self.format))
        {
            return Err(format!("output_path must be a new .{} file", self.format).into());
        }
        let (value, warnings) = self.render(input, Some(out), dir, root)?;
        output::save(out, value.as_bytes(), true)?;
        Ok(json!({"output_path":out,"bytes":value.len(),"warnings":warnings}))
    }
}

#[derive(Default, Serialize)]
struct BatchReport {
    succeeded: usize,
    failed: usize,
    omitted_results: usize,
    results: Vec<Value>,
    #[serde(skip)]
    bytes: usize,
}

impl BatchReport {
    fn push(&mut self, entry: Value) {
        if entry.get("error").is_some() {
            self.failed += 1;
        } else {
            self.succeeded += 1;
        }
        // Leave room for summary fields and JSON delimiters.
        let bytes = entry.to_string().len() + 1;
        if self.bytes + bytes <= INLINE - 256 {
            self.bytes += bytes;
            self.results.push(entry);
        } else {
            self.omitted_results += 1;
        }
    }
}

fn convert_batch(access: &Access, args: &Value, conversion: &Conversion<'_>) -> Result<String> {
    let paths = args["paths"].as_array().ok_or("missing paths")?;
    let recursive = args["recursive"].as_bool().unwrap_or(false);
    let jobs = parallel::jobs(args["jobs"].as_u64().map(|n| n as usize));
    let mut inputs = Vec::new();
    let mut seen = HashSet::new();
    let mut report = BatchReport::default();
    for path in paths.iter().filter_map(Value::as_str) {
        let expanded = access.input_path(path).and_then(|spec| {
            batch::expand(&spec, recursive, false, |p| {
                access.scoped(p.canonicalize()?)
            })
        });
        match expanded {
            Ok(found) => {
                inputs.extend(found.files.into_iter().filter(|p| seen.insert(p.clone())));
                for (path, error) in found.errors {
                    report.push(json!({"input":path,"error":error}));
                }
            }
            Err(e) => report.push(json!({"input":path,"error":e.to_string()})),
        }
    }
    if !inputs.is_empty() {
        let out_dir = access.directory(args["output_dir"].as_str().ok_or("missing output_dir")?)?;
        let image_parent = if let Some(dir) = args["image_dir"].as_str() {
            Some(access.directory(dir)?)
        } else if args["extract_images"].as_bool().unwrap_or(false) {
            Some(out_dir.clone())
        } else {
            None
        };
        // Name results and image folders first so that workers write distinct
        // files and the report keeps the input order.
        let mut stems = batch::OutputNames::default();
        let mut tasks = Vec::with_capacity(inputs.len());
        for input in inputs {
            let stem = stems.next(&input);
            let out = out_dir.join(batch::output_name(&stem, conversion.format));
            let dir = image_parent
                .as_ref()
                .map(|parent| {
                    let mut name = stem.clone();
                    if args["image_dir"].is_null() {
                        name.push("_images");
                    }
                    let path = parent.join(name);
                    if path.exists() {
                        access.scoped(path.canonicalize()?)
                    } else {
                        Ok(path)
                    }
                })
                .transpose()?;
            tasks.push((input, out, dir));
        }
        parallel::ordered(
            &tasks,
            jobs,
            |(input, out, dir)| {
                // Errors cross the worker boundary as text; the report shape stays the same.
                let result = recover_document(AssertUnwindSafe(|| {
                    conversion.save(input, out, dir.as_deref(), &access.root)
                }))
                .map_err(|e| e.to_string());
                (input.clone(), result)
            },
            |(input, result)| match result {
                Ok(mut summary) => {
                    summary["input"] = json!(input);
                    report.push(summary)
                }
                Err(e) => report.push(json!({"input":input,"error":e})),
            },
        );
    }
    Ok(serde_json::to_string(&report)?)
}

fn call(access: &Access, name: &str, args: &Value) -> Result<String> {
    if name == "list_supported_formats" {
        return Ok(serde_json::to_string(&support())?);
    }
    let text = |k: &str| args.get(k).and_then(Value::as_str);
    let conversion = Conversion {
        format: text("format").unwrap_or("md"),
        mode: text("mode").unwrap_or("hybrid"),
        range: text("range"),
    };
    if name == "convert_documents" {
        return convert_batch(access, args, &conversion);
    }
    let input = access.local(text("path").ok_or("missing path")?, false)?;
    if name == "search_document" {
        let query = search::Query::new(
            text("query").ok_or("missing query")?,
            args["case_sensitive"].as_bool().unwrap_or(false),
            args["limit"]
                .as_u64()
                .map_or(search::DEFAULT_LIMIT, |n| n as usize),
        )?;
        let d = load_range(&input, false, conversion.range)?;
        return Ok(serde_json::to_string(&query.find(&d))?);
    }
    if name == "inspect_document" {
        let d = load_range(&input, false, None)?;
        let sections:Vec<_>=d.sections.iter().enumerate().map(|(i,s)|json!({"index":i+1,"kind":s.kind,"title":s.title,"source":s.source,"blocks":s.blocks.len(),"properties":s.properties})).collect();
        return Ok(serde_json::to_string(
            &json!({"format":d.format,"metadata":d.metadata,"sections":sections,"warnings":d.warnings}),
        )?);
    }
    let out = text("output_path")
        .map(|p| access.local(p, true))
        .transpose()?;
    let images = args["extract_images"].as_bool().unwrap_or(false) || text("image_dir").is_some();
    let mut dir = None;
    if images {
        let default_dir = format!(
            "{}_images",
            input.file_stem().unwrap_or_default().to_string_lossy()
        );
        dir = Some(access.local(text("image_dir").unwrap_or(&default_dir), true)?);
    }
    if let Some(out) = out {
        let summary = conversion.save(&input, &out, dir.as_deref(), &access.root)?;
        Ok(serde_json::to_string(&summary)?)
    } else {
        let (text, _) = conversion.render(&input, None, dir.as_deref(), &access.root)?;
        Ok(text)
    }
}
fn rpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn dispatch(access: &Access, req: Value, phase: &mut u8, defs: &[Value]) -> Option<Value> {
    let id = &req["id"];
    let method = req["method"].as_str();
    if req["jsonrpc"] != "2.0"
        || method.is_none()
        || req.get("id").is_some() && !(id.is_string() || id.is_i64() || id.is_u64())
    {
        return Some(rpc_error(&Value::Null, -32600, "Invalid Request"));
    }
    let method = method.unwrap_or("");
    if req.get("id").is_none() {
        if method == "notifications/initialized" && *phase == 1 {
            *phase = 2
        }
        return None;
    }
    let p = &req["params"];
    let result = match method {
        "ping" => json!({}),
        "initialize" if *phase == 0 => {
            let Some(version) = p["protocolVersion"].as_str() else {
                return Some(rpc_error(id, -32602, "protocolVersion is required"));
            };
            let v = match version {
                "2024-11-05" | "2025-03-26" | "2025-06-18" => version,
                _ => "2025-11-25",
            };
            *phase = 1;
            json!({"protocolVersion":v,"capabilities":{"tools":{}},"serverInfo":{"name":"rdocs2md","version":env!("CARGO_PKG_VERSION")}})
        }
        _ if *phase != 2 => return Some(rpc_error(id, -32002, "Server is not initialized")),
        "tools/list" => json!({"tools":defs}),
        "tools/call" => {
            let name = p["name"].as_str().unwrap_or("");
            let Some(def) = defs.iter().find(|d| d["name"] == name) else {
                return Some(rpc_error(id, -32602, "Unknown tool"));
            };
            let empty = json!({});
            let args = p.get("arguments").unwrap_or(&empty);
            let Some(obj) = args.as_object() else {
                return Some(rpc_error(id, -32602, "arguments must be an object"));
            };
            let schema = &def["inputSchema"];
            if schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|k| obj.get(k.as_str().unwrap()).is_none())
                || obj.iter().any(|(k, v)| {
                    let spec = &schema["properties"][k];
                    spec.is_null()
                        || match spec["type"].as_str() {
                            Some("boolean") => !v.is_boolean(),
                            Some("integer") => v.as_u64().is_none_or(|n| {
                                n < spec["minimum"].as_u64().unwrap_or(0)
                                    || n > spec["maximum"].as_u64().unwrap_or(u64::MAX)
                            }),
                            Some("array") => v.as_array().is_none_or(|items| {
                                items.is_empty() || items.iter().any(|v| !v.is_string())
                            }),
                            _ => !v.is_string(),
                        }
                        || spec["enum"].as_array().is_some_and(|a| !a.contains(v))
                })
            {
                return Some(rpc_error(
                    id,
                    -32602,
                    "Arguments do not match the tool input schema",
                ));
            }
            let response = recover_document(|| call(access, name, args));
            let (s, bad) = match response {
                Ok(s) if s.len() <= INLINE => (s, false),
                Ok(_) => (
                    if name == "search_document" {
                        "Result exceeds 512 KiB. Use a smaller range or search limit."
                    } else {
                        "Result exceeds 512 KiB. Use a smaller range or save with output_path."
                    }
                    .into(),
                    true,
                ),
                Err(e) => (e.to_string(), true),
            };
            json!({"content":[{"type":"text","text":s}],"isError":bad})
        }
        _ => return Some(rpc_error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
pub fn serve(path: &Path) -> Result<()> {
    serve_with_access(path, false)
}
pub fn serve_with_access(path: &Path, allow_outside_root: bool) -> Result<()> {
    let root = path.canonicalize()?;
    if !root.is_dir() {
        return Err("MCP root must be a directory".into());
    }
    let mut stdin = io::stdin().lock();
    let mut out = io::stdout().lock();
    let mut line = Vec::new();
    let mut phase = 0;
    let access = Access {
        root,
        allow_outside_root,
    };
    let defs = definitions(allow_outside_root);
    loop {
        line.clear();
        let n = std::io::Read::take(&mut stdin, FRAME + 1).read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        let reply = if n as u64 > FRAME {
            if line.last() != Some(&b'\n') {
                stdin.skip_until(b'\n')?;
            }
            Some(rpc_error(
                &Value::Null,
                -32600,
                "Request frame exceeds 1 MiB",
            ))
        } else {
            match serde_json::from_slice::<Value>(&line) {
                Ok(r) => dispatch(&access, r, &mut phase, &defs),
                Err(_) => Some(rpc_error(&Value::Null, -32700, "Parse error")),
            }
        };
        if let Some(r) = reply {
            serde_json::to_writer(&mut out, &r)?;
            out.write_all(b"\n")?;
            out.flush()?
        }
    }
    Ok(())
}
