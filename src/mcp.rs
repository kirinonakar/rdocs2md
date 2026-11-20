use crate::{load, model::*, output, render::render, support};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
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
            "list_supported_formats",
            "List the implemented formats, features and limitations.",
        ),
    ] {
        let mut props = json!({});
        let mut required = vec![];
        if name != "list_supported_formats" {
            props["path"] =
                json!({"type":"string","description":format!("Input document. {scope}")});
            required.push("path")
        }
        if matches!(name, "convert_document" | "extract_document_range") {
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
            props["output_path"] = json!({"type":"string","description":format!("New filename with an extension that matches format. {scope}")});
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
    fn local(&self, path: &str, new: bool) -> Result<PathBuf> {
        if path.is_empty()
            || path.starts_with(['\\', '/'])
                && path.get(..2).is_some_and(|p| matches!(p, "//" | "\\\\"))
            || path.contains('\0')
        {
            return Err("expected a local filesystem path".into());
        }
        let p = Path::new(path);
        let full = self.root.join(p);
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
        let path = if new {
            let name = full.file_name().ok_or("expected output name")?;
            output::parent(&full).canonicalize()?.join(name)
        } else {
            full.canonicalize()?
        };
        if !self.allow_outside_root && !path.starts_with(&self.root) {
            return Err("path is outside the MCP root".into());
        }
        if !self.allow_outside_root
            && new
            && path.exists()
            && !path.canonicalize()?.starts_with(&self.root)
        {
            return Err("output resolves outside the MCP root".into());
        }
        Ok(path)
    }
}
fn call(access: &Access, name: &str, args: &Value) -> Result<String> {
    if name == "list_supported_formats" {
        return Ok(serde_json::to_string(&support())?);
    }
    let text = |k: &str| args.get(k).and_then(Value::as_str);
    let input = access.local(text("path").ok_or("missing path")?, false)?;
    let mode = text("mode").unwrap_or("hybrid");
    let images =
        args["extract_images"].as_bool().unwrap_or(false) || args.get("image_dir").is_some();
    let mut d = load(&input, images)?;
    if name == "inspect_document" {
        let sections:Vec<_>=d.sections.iter().enumerate().map(|(i,s)|json!({"index":i+1,"kind":s.kind,"title":s.title,"source":s.source,"blocks":s.blocks.len(),"properties":s.properties})).collect();
        return Ok(serde_json::to_string(
            &json!({"format":d.format,"metadata":d.metadata,"sections":sections,"warnings":d.warnings}),
        )?);
    }
    if let Some(r) = text("range") {
        d.select(r)?
    }
    let format = text("format").unwrap_or("md");
    let out = text("output_path")
        .map(|p| access.local(p, true))
        .transpose()?;
    if let Some(p) = &out
        && (p.exists()
            || p.extension()
                .is_none_or(|v| !v.eq_ignore_ascii_case(format)))
    {
        return Err(format!("output_path must be a new .{format} file").into());
    }
    let mut links = Properties::new();
    if images {
        let default_dir = format!(
            "{}_images",
            input.file_stem().unwrap_or_default().to_string_lossy()
        );
        let dir = access.local(text("image_dir").unwrap_or(&default_dir), true)?;
        links = output::extract(
            &d,
            &dir,
            out.as_ref()
                .map(|p| output::parent(p))
                .unwrap_or(&access.root),
        )?;
    }
    let value = render(&d, format, mode, &links)?;
    if let Some(p) = out {
        output::save(&p, value.as_bytes(), true)?;
        Ok(serde_json::to_string(
            &json!({"output_path":p,"bytes":value.len(),"warnings":d.warnings}),
        )?)
    } else {
        Ok(value)
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
            let response =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(access, name, args)));
            let (s, bad) = match response {
                Ok(Ok(s)) if s.len() <= INLINE => (s, false),
                Ok(Ok(_)) => (
                    "Result exceeds 512 KiB. Use a smaller range or save with output_path.".into(),
                    true,
                ),
                Ok(Err(e)) => (e.to_string(), true),
                Err(_) => ("Document parser failed".into(), true),
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
