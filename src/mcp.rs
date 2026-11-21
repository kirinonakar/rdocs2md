use crate::{
    batch, load_range, model::*, output, parallel, recover_document, render::render, search,
    support,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::HashSet,
    fs,
    io::{self, BufRead, Write},
    panic::AssertUnwindSafe,
    path::{Component, Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};
const INLINE: usize = 512 * 1024;
const FRAME: u64 = 1024 * 1024;
/// Default per-call budget, so that one pathological document cannot block the
/// single-threaded stdio loop (and the `ping` that follows it) indefinitely.
const TIMEOUT: u64 = 300;
/// Parsed documents kept for repeated read-only MCP requests.
const CACHE_ENTRIES: usize = 8;
const CACHE_BYTES: usize = 128 * MIB;
fn definitions(root: &Path, allow_outside_root: bool) -> Vec<Value> {
    // The agent cannot infer the working directory of this process, so every
    // path argument states the resolved root instead of only naming a scope.
    let root = root.display();
    let scope = if allow_outside_root {
        format!(
            "MCP root: {root}. Relative paths are resolved against this root; \
             absolute local paths are also accepted."
        )
    } else {
        format!(
            "MCP root: {root}. Relative paths are resolved against this root; \
             paths outside it are rejected."
        )
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
            props["jobs"] = json!({"type":"integer","minimum":1,"maximum":parallel::MAX_JOBS,"description":"Defaults to the CPU count."});
            props["max_inflight_bytes"] = json!({"type":"integer","minimum":0,"description":"Defaults to 1 GiB; 0 removes the cap."});
            required.extend(["paths", "output_dir"]);
        } else if name != "list_supported_formats" {
            props["path"] =
                json!({"type":"string","description":format!("Input document. {scope}")});
            required.push("path")
        }
        if name != "list_supported_formats" {
            props["timeout_seconds"] = json!({"type":"integer","minimum":0,"description":"Time limit for this call in seconds; 0 disables it. Defaults to 300."});
        }
        if matches!(
            name,
            "convert_document" | "extract_document_range" | "convert_documents"
        ) {
            props["format"] = json!({
                "type":"string",
                "enum":["md","html","json"],
                "default":"md"
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
    cache: RefCell<DocumentCache>,
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

#[derive(PartialEq, Eq)]
struct CacheKey {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    range: Option<String>,
}

struct CachedDocument {
    key: CacheKey,
    document: Rc<Document>,
    bytes: usize,
}

/// Recently parsed documents for the read-only MCP tools. Requests are handled
/// one at a time, so an ordinary `RefCell` is enough, and the CLI never caches.
#[derive(Default)]
struct DocumentCache {
    entries: Vec<CachedDocument>,
    bytes: usize,
}

impl DocumentCache {
    fn load(&mut self, path: &Path, range: Option<&str>) -> Result<Rc<Document>> {
        let key = Self::key(path, range)?;
        // A rewritten file must not keep its earlier version cached, whichever
        // range that entry was parsed from.
        self.drop_stale(&key);
        if let Some(position) = self.entries.iter().position(|entry| entry.key == key) {
            // Move the hit to the back so that the first entry is the oldest.
            let entry = self.entries.remove(position);
            let document = Rc::clone(&entry.document);
            self.entries.push(entry);
            return Ok(document);
        }
        let document = Rc::new(load_range(path, false, range)?);
        // A parse that ran out of time returns fewer pages, so it must not be
        // reused: the next request may allow more time and would otherwise get
        // the truncated copy back immediately.
        if crate::limits::expired() {
            return Ok(document);
        }
        let bytes = document_bytes(&document);
        // A document that fills the cache on its own would evict everything else.
        if bytes <= CACHE_BYTES / 2 {
            while !self.entries.is_empty()
                && (self.entries.len() >= CACHE_ENTRIES || self.bytes + bytes > CACHE_BYTES)
            {
                let evicted = self.entries.remove(0);
                self.bytes -= evicted.bytes;
            }
            self.bytes += bytes;
            self.entries.push(CachedDocument {
                key,
                document: Rc::clone(&document),
                bytes,
            });
        }
        Ok(document)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Evict cached versions of this file whose size or timestamp is stale, so
    /// that a rewrite cannot leave two entries for one path.
    fn drop_stale(&mut self, key: &CacheKey) {
        let mut stale = 0;
        self.entries.retain(|entry| {
            let old = entry.key.path == key.path
                && (entry.key.len != key.len || entry.key.modified != key.modified);
            if old {
                stale += entry.bytes;
            }
            !old
        });
        self.bytes -= stale;
    }

    /// Size and modification time invalidate a cached document in place.
    fn key(path: &Path, range: Option<&str>) -> Result<CacheKey> {
        let metadata = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(CacheKey {
            path: path.to_path_buf(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            range: range.map(str::to_owned),
        })
    }
}

/// Rough retained size of a parsed document, used only to bound the cache.
fn document_bytes(d: &Document) -> usize {
    fn runs(list: &[Run]) -> usize {
        list.iter()
            .map(|run| {
                run.text.len()
                    + run.mathml.as_ref().map_or(0, String::len)
                    + run.latex.as_ref().map_or(0, String::len)
                    + 64
            })
            .sum()
    }
    fn blocks(list: &[Block]) -> usize {
        list
            .iter()
            .map(|block| match block {
                Block::Paragraph {
                    runs: text,
                    properties,
                    ..
                } => runs(text) + properties.len() * 32,
                Block::Table { rows } => rows
                    .iter()
                    .flatten()
                    .map(|cell| runs(&cell.runs) + blocks(&cell.blocks) + 64)
                    .sum(),
            })
            .sum()
    }
    d.sections
        .iter()
        .map(|section| {
            blocks(&section.blocks) + section.properties.len() * 32 + section.title.len() + 128
        })
        .sum::<usize>()
        + d.metadata.len() * 32
        + d.assets.values().map(Vec::len).sum::<usize>()
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

/// A tool reply: the text result plus optional conversion warnings, which are
/// sent as a second text block. No tool returns `structuredContent`, so a JSON
/// report is the text result itself.
enum Reply {
    Text(String),
    Report {
        text: String,
        warnings: Vec<String>,
    },
}

impl Reply {
    fn json(value: &impl Serialize) -> Result<Self> {
        Ok(Self::Text(serde_json::to_string(value)?))
    }

    fn into_parts(self) -> (String, Vec<String>) {
        match self {
            Self::Text(text) => (text, Vec::new()),
            Self::Report { text, warnings } => (text, warnings),
        }
    }
}

fn convert_batch(
    access: &Access,
    args: &Value,
    conversion: &Conversion<'_>,
    timeout: Option<u64>,
) -> Result<Reply> {
    let paths = args["paths"].as_array().ok_or("missing paths")?;
    let recursive = args["recursive"].as_bool().unwrap_or(false);
    let jobs = args["jobs"].as_u64().map(|n| n as usize);
    let budget = args["max_inflight_bytes"]
        .as_u64()
        .unwrap_or(parallel::DEFAULT_INFLIGHT_BYTES);
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
        // Size the pool before parsing starts so that a batch of large inputs
        // does not run on every core at once.
        let sizes: Vec<u64> = inputs
            .iter()
            .map(|path| fs::metadata(path).map_or(0, |m| m.len()))
            .collect();
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
        // Borrow only the root: the cache is not `Sync`, so workers must not
        // capture the whole `Access`.
        let root = access.root.as_path();
        parallel::ordered(
            &tasks,
            parallel::jobs_within(jobs, &sizes, budget),
            |(input, out, dir)| {
                // Each worker thread needs its own budget; the caller's guard
                // does not cross the thread boundary.
                let _budget = crate::limits::start(timeout);
                // Errors cross the worker boundary as text; the report shape stays the same.
                let result = recover_document(AssertUnwindSafe(|| {
                    conversion.save(input, out, dir.as_deref(), root)
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
    Reply::json(&report)
}

fn call(access: &Access, name: &str, args: &Value) -> Result<Reply> {
    if name == "list_supported_formats" {
        return Ok(Reply::Text(serde_json::to_string(&support())?));
    }
    let seconds = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(TIMEOUT);
    let timeout = (seconds > 0).then_some(seconds);
    let _budget = crate::limits::start(timeout);
    let text = |k: &str| args.get(k).and_then(Value::as_str);
    let conversion = Conversion {
        format: text("format").unwrap_or("md"),
        mode: text("mode").unwrap_or("hybrid"),
        range: text("range"),
    };
    if name == "convert_documents" {
        return convert_batch(access, args, &conversion, timeout);
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
        let document = access.cache.borrow_mut().load(&input, conversion.range)?;
        return Reply::json(&query.find(&document));
    }
    if name == "inspect_document" {
        let document = access.cache.borrow_mut().load(&input, None)?;
        let document = &*document;
        let sections: Vec<_> = document
            .sections
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({
                    "index": s.index.unwrap_or(i + 1),
                    "kind": s.kind,
                    "title": s.title,
                    "source": s.source,
                    "blocks": s.blocks.len(),
                    "properties": s.properties,
                })
            })
            .collect();
        return Reply::json(&json!({
            "format": document.format,
            "metadata": document.metadata,
            "sections": sections,
            "warnings": document.warnings,
        }));
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
        // The summary is already the text result, so a structured copy would
        // only repeat it.
        return Ok(Reply::Text(serde_json::to_string(&summary)?));
    }
    // Without images the parsed document can be reused by later read-only
    // requests; image extraction changes the returned links, so it is not cached.
    let document = if dir.is_none() {
        access.cache.borrow_mut().load(&input, conversion.range)?
    } else {
        Rc::new(load_range(&input, true, conversion.range)?)
    };
    let document = &*document;
    let links = match dir.as_deref() {
        Some(dir) => output::extract(document, dir, &access.root)?,
        None => Properties::new(),
    };
    let content = render(document, conversion.format, conversion.mode, &links)?;
    // Conversion tools keep the document in the text block only; warnings still
    // travel as a second text block, and no body is sent twice.
    Ok(Reply::Report {
        text: content,
        warnings: document.warnings.clone(),
    })
}
fn rpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
/// Message returned when a reply does not fit the inline limit.
fn oversized(name: &str) -> &'static str {
    if name == "search_document" {
        "Result exceeds 512 KiB. Use a smaller range or search limit."
    } else {
        "Result exceeds 512 KiB. Use a smaller range or save with output_path."
    }
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
            // Clients that forward `instructions` to the model learn the same
            // root as the tool schemas, so the agent does not probe for it.
            let root = access.root.display();
            let instructions = if access.allow_outside_root {
                format!(
                    "Document root: {root}. Relative file paths are resolved against \
                     this directory; absolute local paths outside it are also accepted."
                )
            } else {
                format!(
                    "Document root: {root}. Relative file paths are resolved against \
                     this directory; paths outside it are rejected."
                )
            };
            json!({"protocolVersion":v,"capabilities":{"tools":{}},"serverInfo":{"name":"rdocs2md","version":env!("CARGO_PKG_VERSION")},"instructions":instructions})
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
            // The cache uses a `RefCell`, which is not `UnwindSafe`; it is only
            // mutated after parsing succeeds, so a caught panic leaves it intact.
            let response = recover_document(AssertUnwindSafe(|| call(access, name, args)));
            let (text, warnings, bad): (String, Vec<String>, bool) = match response {
                Ok(reply) => {
                    let (text, warnings) = reply.into_parts();
                    if text.len() <= INLINE {
                        (text, warnings, false)
                    } else {
                        (oversized(name).into(), Vec::new(), true)
                    }
                }
                Err(e) => (e.to_string(), Vec::new(), true),
            };
            let mut content = vec![json!({"type":"text","text":text})];
            if !warnings.is_empty() {
                // Warnings are a second text block, so no client misses them.
                content.push(json!({
                    "type":"text",
                    "text":format!(
                        "Conversion warnings (the result may be incomplete):\n- {}",
                        warnings.join("\n- ")
                    ),
                }));
            }
            let mut result = json!({"content":content,"isError":bad});
            // The limit covers the whole reply: JSON escaping and the warning
            // block can grow it past the text measured above.
            if !bad && serde_json::to_string(&result).is_ok_and(|reply| reply.len() > INLINE) {
                result = json!({
                    "content":[{"type":"text","text":oversized(name)}],
                    "isError":true,
                });
            }
            result
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
        cache: RefCell::default(),
    };
    let defs = definitions(&access.root, allow_outside_root);
    loop {
        line.clear();
        // A closed or broken input stream means the client disconnected.
        let Ok(n) = std::io::Read::take(&mut stdin, FRAME + 1).read_until(b'\n', &mut line) else {
            break;
        };
        if n == 0 {
            break;
        }
        let reply = if n as u64 > FRAME {
            if line.last() != Some(&b'\n') && stdin.skip_until(b'\n').is_err() {
                break;
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
            // A failed write means the client is gone; nothing is left to serve.
            if serde_json::to_writer(&mut out, &r).is_err()
                || out.write_all(b"\n").is_err()
                || out.flush().is_err()
            {
                break;
            }
        }
    }
    // Release cached documents before returning so that the memory is reclaimed
    // even when this server runs inside a longer-lived process.
    access.cache.borrow_mut().clear();
    drop(defs);
    line.clear();
    let _ = out.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_reloads_changed_documents_and_clears() {
        let path = std::env::temp_dir().join(format!(
            "rdocs2md-cache-{}-{}.rtf",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        fs::write(&path, br"{\rtf1 first}").unwrap();
        let mut cache = DocumentCache::default();
        let first = cache.load(&path, None).unwrap();
        assert_eq!(first.sections.len(), 1);
        assert!(Rc::ptr_eq(&first, &cache.load(&path, None).unwrap()));
        fs::write(&path, br"{\rtf1 second\page third}").unwrap();
        let changed = cache.load(&path, None).unwrap();
        assert_eq!(changed.sections.len(), 2);
        assert!(!Rc::ptr_eq(&first, &changed));
        // The stale version is dropped as soon as the newer one is loaded.
        assert_eq!(cache.entries.len(), 1);
        assert!(Rc::ptr_eq(&cache.entries[0].document, &changed));
        cache.clear();
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cache_keeps_no_document_that_ran_out_of_time() {
        let path = std::env::temp_dir().join(format!(
            "rdocs2md-cache-timeout-{}-{}.rtf",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        fs::write(&path, br"{\rtf1 first\page second}").unwrap();
        let mut cache = DocumentCache::default();
        // Without a budget this document caches normally.
        let _budget = crate::limits::start(None);
        let complete = cache.load(&path, None).unwrap();
        cache.clear();
        assert_eq!(complete.sections.len(), 2);
        crate::limits::expire();
        let truncated = cache.load(&path, None).unwrap();
        assert!(!Rc::ptr_eq(&complete, &truncated));
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn path_descriptions_name_the_root() {
        for allow in [false, true] {
            let defs = definitions(Path::new("C:/docs-root"), allow);
            let text = serde_json::to_string(&defs).unwrap();
            assert!(text.contains("MCP root: C:/docs-root"));
            assert_eq!(text.contains("paths outside it are rejected"), !allow);
            // No path argument may leave the agent guessing where relative paths start.
            for tool in &defs {
                for (key, prop) in tool["inputSchema"]["properties"].as_object().unwrap() {
                    if matches!(
                        key.as_str(),
                        "path" | "paths" | "output_dir" | "output_path" | "image_dir"
                    ) {
                        let description = prop["description"].as_str().unwrap_or("");
                        assert!(
                            description.contains("MCP root: C:/docs-root"),
                            "{key} does not name the root: {description}"
                        );
                    }
                }
            }
        }
    }
}
