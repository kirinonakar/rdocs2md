mod stdio;
use stdio::Progress;
pub use stdio::{serve, serve_with_access};

use crate::{
    batch, limits, load_sheets, model::*, output, parallel, recover_document, render::render,
    search,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    panic::AssertUnwindSafe,
    path::{Component, Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};
const INLINE: usize = 512 * 1024;
/// Parsed documents kept for repeated read-only MCP requests.
const CACHE_ENTRIES: usize = 8;
const CACHE_BYTES: usize = 128 * MIB;
const RANGE_HINT: &str = "1-based inclusive sections, e.g. 1-3,5.";

fn path_context(root: &Path, allow_outside_root: bool) -> String {
    let root = root.display().to_string();
    // Advertise an ordinary drive path that the client can pass back as input.
    #[cfg(windows)]
    let root = root
        .strip_prefix(r"\\?\")
        .filter(|path| path.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&root);
    let access = if allow_outside_root {
        "Absolute local paths may be outside this root."
    } else {
        "Keep paths within this root."
    };
    format!(
        "MCP root: {root}. Use this root for relative paths and, if your workspace root differs, use absolute paths. {access}"
    )
}

fn definitions(root: &Path, allow_outside_root: bool) -> Vec<Value> {
    // Some clients forward tools/list without initialize.instructions.
    let scope = path_context(root, allow_outside_root);
    let mut tools = Vec::new();
    for (name, description) in [
        (
            "convert_document",
            "Convert one document. Use output_path for large results.",
        ),
        (
            "inspect_document",
            "Get format, metadata, warnings and section indexes.",
        ),
        (
            "extract_document_range",
            "Convert selected pages, slides, sheets, chapters or sections.",
        ),
        (
            "search_document",
            "Find literal text with snippets and locations in one document.",
        ),
        (
            "search_documents",
            "Find literal text across documents. Return hits and errors by file.",
        ),
        (
            "convert_documents",
            "Convert documents to an output directory. Report results by file.",
        ),
        (
            "list_supported_formats",
            "List supported formats and limitations.",
        ),
    ] {
        let mut props = json!({});
        let mut required = vec![];
        if matches!(name, "convert_documents" | "search_documents") {
            props["paths"] = json!({"type":"array","items":{"type":"string"},"minItems":1,"description":"Files, directories or globs (*, ?, [], **)."});
            props["recursive"] = json!({"type":"boolean","description":"Include subdirectories."});
            required.push("paths");
        } else if name != "list_supported_formats" {
            props["path"] = json!({"type":"string"});
            required.push("path");
        }
        if name == "convert_documents" {
            props["output_dir"] = json!({"type":"string","description":"Parent must exist."});
            props["jobs"] = json!({"type":"integer","minimum":1,"maximum":parallel::MAX_JOBS,"description":"Defaults to the CPU count."});
            props["max_inflight_bytes"] = json!({"type":"integer","minimum":0,"description":"Concurrent input bytes. Default 1 GiB, 0 unlimited."});
            required.push("output_dir");
        }
        if name != "list_supported_formats" {
            props["timeout_seconds"] = json!({"type":"integer","minimum":0,"description":"Seconds including queue time. Default 300, 0 unlimited."});
        }
        if matches!(
            name,
            "convert_document" | "extract_document_range" | "convert_documents"
        ) {
            props["format"] = json!({
                "type":"string",
                "enum":["md","html","json","csv"],
                "default":"md",
                "description":"CSV requires XLS, XLSX or ODS."
            });
            props["mode"] = json!({"type":"string","enum":["semantic","hybrid","layout"]});
            props["extract_images"] = json!({"type":"boolean"});
            props["include_images"] = json!({"type":"boolean","description":"Keep images embedded in HTML when exporting copies."});
            props["image_dir"] =
                json!({"type":"string","description":"Export images here. Parent must exist."});
            props["range"] = json!({"type":"string","description":RANGE_HINT});
            props["sheet"] = json!({"type":"string","minLength":1,"description":"Workbook sheet title or 1-based number."});
            if name != "convert_documents" {
                props["output_path"] = json!({"type":"string","description":"New file with extension matching format."});
            }
        }
        if matches!(name, "search_document" | "search_documents") {
            props["query"] = json!({"type":"string","minLength":1,"maxLength":1024});
            props["case_sensitive"] = json!({"type":"boolean","default":false});
            props["limit"] = json!({"type":"integer","minimum":1,"maximum":search::MAX_LIMIT,"default":search::DEFAULT_LIMIT,"description":"Total matches per call."});
            props["range"] = json!({"type":"string","description":RANGE_HINT});
            required.push("query");
        }
        if name == "extract_document_range" {
            required.push("range")
        }
        let description = if name == "list_supported_formats" {
            description.to_owned()
        } else {
            format!("{description} {scope}")
        };
        tools.push(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":props,"required":required,"additionalProperties":false}}));
    }
    tools
}
struct Access {
    root: PathBuf,
    allow_outside_root: bool,
    cache: DocumentCache,
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
        limits::check()?;
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
    sheets: Vec<String>,
}

struct CachedDocument {
    key: CacheKey,
    document: Rc<Document>,
    bytes: usize,
}

/// Recently parsed documents, owned by the MCP document worker.
#[derive(Default)]
struct DocumentCache {
    entries: Vec<CachedDocument>,
    bytes: usize,
}

impl DocumentCache {
    fn load(
        &mut self,
        path: &Path,
        range: Option<&str>,
        sheets: &[String],
    ) -> Result<Rc<Document>> {
        let key = Self::key(path, range, sheets)?;
        // A rewritten file must not keep its earlier version cached, whichever
        // range that entry was parsed from.
        self.drop_stale(&key);
        let exact = self.entries.iter().position(|entry| entry.key == key);
        let full_pdf = || {
            if range.is_none() || !sheets.is_empty() {
                return None;
            }
            self.entries.iter().position(|entry| {
                entry.key.path == key.path
                    && entry.key.range.is_none()
                    && entry.key.sheets.is_empty()
                    && entry.document.format == "pdf"
                    && entry
                        .document
                        .sections
                        .iter()
                        .all(|s| !s.properties.contains_key("page_error"))
            })
        };
        if let Some(position) = exact.or_else(full_pdf) {
            // Move the hit to the back so that the first entry is the oldest.
            let entry = self.entries.remove(position);
            let document = Rc::clone(&entry.document);
            self.entries.push(entry);
            return if exact.is_some() {
                Ok(document)
            } else {
                Ok(Rc::new(crate::pdf::cached_range(
                    &document,
                    range.unwrap(),
                )?))
            };
        }
        let document = Rc::new(load_sheets(path, false, range, sheets)?);
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

    #[cfg(test)]
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
    fn key(path: &Path, range: Option<&str>, sheets: &[String]) -> Result<CacheKey> {
        let metadata = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(CacheKey {
            path: path.to_path_buf(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            range: range.map(str::to_owned),
            sheets: sheets.to_vec(),
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
        list.iter()
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
            blocks(&section.blocks)
                + section.properties.len() * 32
                + section.title.len()
                + section
                    .pdf_warnings
                    .iter()
                    .map(|w| w.len() + size_of::<String>())
                    .sum::<usize>()
                + 128
        })
        .sum::<usize>()
        + d.metadata.len() * 32
        + d.assets.values().map(Vec::len).sum::<usize>()
}

struct Conversion<'a> {
    format: &'a str,
    mode: &'a str,
    range: Option<&'a str>,
    sheet: Option<&'a str>,
    inline_images: bool,
}

impl Conversion<'_> {
    /// Named sheet filter in the form the loader and the cache key expect.
    fn sheets(&self) -> Vec<String> {
        self.sheet.map(str::to_owned).into_iter().collect()
    }

    fn save(&self, input: &Path, out: &Path, dir: Option<&Path>, root: &Path) -> Result<Value> {
        limits::check()?;
        self.validate_output(out)?;
        let sheets = self.sheets();
        let document = load_sheets(
            input,
            dir.is_some() || self.format == "html",
            self.range,
            &sheets,
        )?;
        self.save_document(&document, out, dir, root)
    }

    fn render_document(
        &self,
        document: &Document,
        out: Option<&Path>,
        dir: Option<&Path>,
        root: &Path,
    ) -> Result<String> {
        limits::check()?;
        let mut links = if let Some(dir) = dir {
            output::extract(document, dir, out.map(output::parent).unwrap_or(root))?
        } else {
            Properties::new()
        };
        if self.inline_images && !links.is_empty() {
            links = output::inline(document, &links)?;
        }
        render(document, self.format, self.mode, &links)
    }

    fn validate_output(&self, out: &Path) -> Result<()> {
        if out.exists()
            || out
                .extension()
                .is_none_or(|v| !v.eq_ignore_ascii_case(self.format))
        {
            return Err(format!("output_path must be a new .{} file", self.format).into());
        }
        Ok(())
    }

    fn save_document(
        &self,
        document: &Document,
        out: &Path,
        dir: Option<&Path>,
        root: &Path,
    ) -> Result<Value> {
        let value = self.render_document(document, Some(out), dir, root)?;
        output::save(out, value.as_bytes(), true)?;
        Ok(json!({"output_path":out,"bytes":value.len(),"warnings":document.warnings}))
    }
}

#[derive(Default)]
struct BatchReport {
    succeeded: usize,
    failed: usize,
    results: BTreeMap<usize, (Value, usize)>,
    bytes: usize,
    first_omitted: Option<usize>,
}

impl BatchReport {
    fn len(&self) -> usize {
        self.succeeded + self.failed
    }

    fn push(&mut self, index: usize, entry: Value) {
        if entry.get("error").is_some() {
            self.failed += 1;
        } else {
            self.succeeded += 1;
        }
        if self.first_omitted.is_some_and(|cutoff| index >= cutoff) {
            return;
        }
        // Count both JSON layers: the report is itself a JSON-RPC text block.
        let bytes = json!(entry.to_string()).to_string().len() - 1;
        self.results.insert(index, (entry, bytes));
        self.bytes += bytes;
        while self.bytes > INLINE - 256 {
            let (index, (_, bytes)) = self.results.pop_last().unwrap();
            self.bytes -= bytes;
            self.first_omitted = Some(index);
        }
    }

    fn reply(self, search_truncated: Option<bool>) -> Result<Reply> {
        let omitted = self.len() - self.results.len();
        let results: Vec<_> = self.results.into_values().map(|(entry, _)| entry).collect();
        let mut report = json!({"succeeded":self.succeeded,"failed":self.failed,"omitted_results":omitted,"results":results});
        if let Some(truncated) = search_truncated {
            report["truncated"] = json!(truncated || omitted > 0);
        }
        Reply::json(&report)
    }
}

/// A tool reply: the text result plus optional conversion warnings, which are
/// sent as a second text block. No tool returns `structuredContent`, so a JSON
/// report is the text result itself.
enum Reply {
    Text(String),
    Report { text: String, warnings: Vec<String> },
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

fn batch_inputs(access: &Access, args: &Value, report: &mut BatchReport) -> Result<Vec<PathBuf>> {
    let paths = args["paths"].as_array().ok_or("missing paths")?;
    let recursive = args["recursive"].as_bool().unwrap_or(false);
    let mut inputs = Vec::new();
    let mut seen = HashSet::new();
    for path in paths.iter().filter_map(Value::as_str) {
        let expanded = access.input_path(path).and_then(|spec| {
            batch::expand(&spec, recursive, false, |p| {
                access.scoped(p.canonicalize()?)
            })
        });
        limits::check()?;
        match expanded {
            Ok(found) => {
                inputs.extend(found.files.into_iter().filter(|p| seen.insert(p.clone())));
                for (path, error) in found.errors {
                    report.push(report.len(), json!({"input":path,"error":error}));
                }
            }
            Err(e) => report.push(report.len(), json!({"input":path,"error":e.to_string()})),
        }
    }
    Ok(inputs)
}

fn convert_batch(
    access: &Access,
    args: &Value,
    conversion: &Conversion<'_>,
    budget: &limits::Budget,
    progress: &mut Progress<'_>,
) -> Result<Reply> {
    let jobs = args["jobs"].as_u64().map(|n| n as usize);
    let max_bytes = args["max_inflight_bytes"]
        .as_u64()
        .unwrap_or(parallel::DEFAULT_INFLIGHT_BYTES);
    let mut report = BatchReport::default();
    let inputs = batch_inputs(access, args, &mut report)?;
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
            limits::check()?;
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
        // The document cache belongs to the request worker. Batch workers only need the root.
        let root = access.root.as_path();
        let offset = report.len();
        parallel::unordered(
            &tasks,
            parallel::jobs_within(jobs, &sizes, max_bytes),
            |(input, out, dir)| {
                let _budget = budget.enter();
                if limits::expired() {
                    return None;
                }
                let result = recover_document(AssertUnwindSafe(|| {
                    conversion.save(input, out, dir.as_deref(), root)
                }));
                Some(match result {
                    Ok(mut summary) => {
                        summary["input"] = json!(input);
                        summary
                    }
                    Err(e) => json!({"input":input,"error":e.to_string()}),
                })
            },
            |index, entry| {
                report.push(offset + index, entry);
                progress.advance(report.len() - offset, tasks.len());
            },
        );
    }
    limits::check()?;
    report.reply(None)
}

fn search_batch(
    access: &mut Access,
    args: &Value,
    query: &search::Query,
    range: Option<&str>,
    progress: &mut Progress<'_>,
) -> Result<Reply> {
    if let Some(range) = range {
        SectionRange::parse(range)?;
    }
    let mut report = BatchReport::default();
    let inputs = batch_inputs(access, args, &mut report)?;
    let mut remaining = query.limit();
    let mut truncated = false;
    for (index, input) in inputs.iter().enumerate() {
        limits::check()?;
        let result = recover_document(AssertUnwindSafe(|| {
            let doc = access.cache.load(input, range, &[])?;
            let result = query.find_up_to(&doc, remaining);
            limits::check()?;
            remaining -= result.match_count();
            truncated |= result.truncated;
            Ok(json!({"input":input,"result":result}))
        }));
        limits::check()?;
        let entry = result.unwrap_or_else(|e| json!({"input":input,"error":e.to_string()}));
        report.push(report.len(), entry);
        progress.advance(index + 1, inputs.len());
    }
    report.reply(Some(truncated))
}

fn call(
    access: &mut Access,
    name: &str,
    args: &Value,
    budget: &limits::Budget,
    progress: &mut Progress<'_>,
) -> Result<Reply> {
    limits::check()?;
    let text = |k: &str| args.get(k).and_then(Value::as_str);
    let format = text("format").unwrap_or("md");
    let conversion = Conversion {
        format,
        mode: text("mode").unwrap_or("hybrid"),
        range: text("range"),
        sheet: text("sheet"),
        // Inlining only applies to HTML; other formats keep plain file links.
        inline_images: format == "html" && args["include_images"].as_bool().unwrap_or(false),
    };
    if name == "convert_documents" {
        return convert_batch(access, args, &conversion, budget, progress);
    }
    if matches!(name, "search_document" | "search_documents") {
        let query = search::Query::new(
            text("query").ok_or("missing query")?,
            args["case_sensitive"].as_bool().unwrap_or(false),
            args["limit"]
                .as_u64()
                .map_or(search::DEFAULT_LIMIT, |n| n as usize),
        )?;
        if name == "search_documents" {
            return search_batch(access, args, &query, conversion.range, progress);
        }
        let input = access.local(text("path").ok_or("missing path")?, false)?;
        let document = access.cache.load(&input, conversion.range, &[])?;
        return Reply::json(&query.find(&document));
    }
    let input = access.local(text("path").ok_or("missing path")?, false)?;
    if name == "inspect_document" {
        let document = access.cache.load(&input, None, &[])?;
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
    let extract_images =
        args["extract_images"].as_bool().unwrap_or(false) || text("image_dir").is_some();
    let mut dir = None;
    if extract_images {
        let default_dir = format!(
            "{}_images",
            input.file_stem().unwrap_or_default().to_string_lossy()
        );
        dir = Some(access.local(text("image_dir").unwrap_or(&default_dir), true)?);
    }
    if let Some(out) = &out {
        conversion.validate_output(out)?;
    }
    // Cache conversions that can reuse a document parsed without image assets.
    let sheets = conversion.sheets();
    let document = if dir.is_none() && conversion.format != "html" {
        access.cache.load(&input, conversion.range, &sheets)?
    } else {
        Rc::new(load_sheets(&input, true, conversion.range, &sheets)?)
    };
    if let Some(out) = out {
        return Reply::json(&conversion.save_document(
            &document,
            &out,
            dir.as_deref(),
            &access.root,
        )?);
    }
    let content = conversion.render_document(&document, None, dir.as_deref(), &access.root)?;
    // Conversion tools keep the document in the text block only; warnings still
    // travel as a second text block, and no body is sent twice.
    Ok(Reply::Report {
        text: content,
        warnings: document.warnings.clone(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_pdf_ranges_preserve_page_contents_and_scoped_warnings() {
        use lopdf::{Document as Pdf, Stream, dictionary};

        let mut pdf = Pdf::with_version("1.4");
        let tree = pdf.new_object_id();
        let font =
            pdf.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
        let pages: Vec<_> = [
            b"q Q".as_slice(),
            b"BT /F 12 Tf 30 100 Td (Visible text) Tj ET",
        ]
        .into_iter()
        .map(|content| {
            let stream = pdf.add_object(Stream::new(dictionary! {}, content.to_vec()));
            pdf.add_object(
                dictionary! {"Type"=>"Page","Parent"=>tree,"Contents"=>stream,
                "Resources"=>dictionary! {"Font"=>dictionary! {"F"=>font}}},
            )
            .into()
        })
        .collect();
        pdf.objects.insert(
            tree,
            dictionary! {"Type"=>"Pages","Kids"=>pages,"Count"=>2,
            "MediaBox"=>vec![0.into(),0.into(),320.into(),220.into()]}
            .into(),
        );
        let catalog = pdf.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree});
        pdf.trailer.set("Root", catalog);
        let path =
            std::env::temp_dir().join(format!("rdocs2md-cached-range-{}.pdf", std::process::id()));
        pdf.save(&path).unwrap();
        let mut cache = DocumentCache::default();
        let full = cache.load(&path, None, &[]).unwrap();
        assert_eq!(full.sections.len(), 2);
        assert!(
            full.warnings
                .iter()
                .any(|warning| warning.contains("No text was extracted"))
        );
        for range in ["1", "2", "1-2"] {
            let cached = cache.load(&path, Some(range), &[]).unwrap();
            let fresh = load_sheets(&path, false, Some(range), &[]).unwrap();
            assert_eq!(
                serde_json::to_value(&*cached).unwrap(),
                serde_json::to_value(&fresh).unwrap()
            );
            assert_eq!(cache.entries.len(), 1);
        }
        assert!(cache.load(&path, Some("3"), &[]).is_err());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn batch_report_keeps_the_same_bounded_prefix_in_completion_order() {
        fn report(order: [usize; 3]) -> Value {
            let mut report = BatchReport::default();
            for index in order {
                report.push(
                    index,
                    json!({"input":index,"warning": "\\\"".repeat(INLINE / 12)}),
                );
            }
            let (text, _) = report.reply(None).unwrap().into_parts();
            serde_json::from_str(&text).unwrap()
        }
        let forward = report([0, 1, 2]);
        assert_eq!(forward, report([2, 0, 1]));
        assert_eq!(forward["succeeded"], 3);
        assert_eq!(forward["omitted_results"], 2);
        assert_eq!(forward["results"][0]["input"], 0);
    }

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
        let first = cache.load(&path, None, &[]).unwrap();
        assert_eq!(first.sections.len(), 1);
        assert!(Rc::ptr_eq(&first, &cache.load(&path, None, &[]).unwrap()));
        fs::write(&path, br"{\rtf1 second\page third}").unwrap();
        let changed = cache.load(&path, None, &[]).unwrap();
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
        let complete = cache.load(&path, None, &[]).unwrap();
        cache.clear();
        assert_eq!(complete.sections.len(), 2);
        crate::limits::expire();
        let truncated = cache.load(&path, None, &[]).unwrap();
        assert!(!Rc::ptr_eq(&complete, &truncated));
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn path_descriptions_name_the_root() {
        for allow in [false, true] {
            let root = Path::new("C:/docs-root");
            let scope = path_context(root, allow);
            for tool in definitions(root, allow) {
                let description = tool["description"].as_str().unwrap();
                if tool["name"] == "list_supported_formats" {
                    assert!(!tool.to_string().contains("MCP root:"));
                } else {
                    assert!(description.contains(&scope));
                    assert!(
                        description.contains("if your workspace root differs, use absolute paths.")
                    );
                    assert_eq!(tool.to_string().matches("MCP root:").count(), 1);
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_context_uses_a_client_usable_drive_path() {
        let scope = path_context(Path::new(r"\\?\C:\docs-root"), false);
        assert!(scope.starts_with(r"MCP root: C:\docs-root."));
    }
}
