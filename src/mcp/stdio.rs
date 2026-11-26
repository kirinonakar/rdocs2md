use super::{Access, INLINE, Reply, call, definitions, path_context};
use crate::{limits, model::Result, recover_document, support};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{self, BufRead, Write},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const FRAME: u64 = 1024 * 1024;
const TIMEOUT: u64 = 300;
const QUEUED_CALLS: usize = 32;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

fn oversized(name: &str) -> &'static str {
    if matches!(name, "search_document" | "search_documents") {
        "Result exceeds 512 KiB. Use a smaller range or search limit."
    } else {
        "Result exceeds 512 KiB. Use a smaller range or save with output_path."
    }
}

fn rpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
#[derive(Clone)]
struct Request {
    id: Value,
    budget: limits::Budget,
    token: Option<Value>,
}

struct Job {
    request: Request,
    name: String,
    args: Value,
}

enum Event {
    Input(Value),
    InputError(Value),
    Closed,
    Progress { done: usize, total: usize },
    Done(Value),
}

pub(super) struct Progress<'a> {
    events: Option<&'a mpsc::SyncSender<Event>>,
    last: Instant,
}

impl Progress<'_> {
    pub(super) fn advance(&mut self, done: usize, total: usize) {
        if let Some(events) = self.events
            && !limits::expired()
        {
            if done == total {
                let _ = events.send(Event::Progress { done, total });
            } else if self.last.elapsed() >= PROGRESS_INTERVAL
                && events.try_send(Event::Progress { done, total }).is_ok()
            {
                self.last = Instant::now();
            }
        }
    }
}

struct Session {
    root: PathBuf,
    allow_outside_root: bool,
    defs: Vec<Value>,
    phase: u8,
    queued: VecDeque<Job>,
    active: Option<Request>,
    worker_busy: bool,
}

fn request_id(value: &Value) -> bool {
    value.is_string() || value.is_i64() || value.is_u64()
}

impl Session {
    fn new(root: PathBuf, allow_outside_root: bool) -> Self {
        Self {
            defs: definitions(&root, allow_outside_root),
            root,
            allow_outside_root,
            phase: 0,
            queued: VecDeque::new(),
            active: None,
            worker_busy: false,
        }
    }

    fn dispatch(&mut self, req: Value) -> Option<Value> {
        let id = &req["id"];
        let method = req["method"].as_str();
        if req["jsonrpc"] != "2.0" || method.is_none() || req.get("id").is_some() && !request_id(id)
        {
            return Some(rpc_error(&Value::Null, -32600, "Invalid Request"));
        }
        let method = method.unwrap_or("");
        if req.get("id").is_none() {
            if method == "notifications/initialized" && self.phase == 1 {
                self.phase = 2;
            } else if method == "notifications/cancelled" {
                let params = &req["params"];
                if request_id(&params["requestId"])
                    && params.get("reason").is_none_or(Value::is_string)
                {
                    self.cancel(&params["requestId"]);
                }
            }
            return None;
        }
        let p = &req["params"];
        let result = match method {
            "ping" => json!({}),
            "initialize" if self.phase == 0 => {
                let Some(version) = p["protocolVersion"].as_str() else {
                    return Some(rpc_error(id, -32602, "protocolVersion is required"));
                };
                let v = match version {
                    "2024-11-05" | "2025-03-26" | "2025-06-18" => version,
                    _ => "2025-11-25",
                };
                self.phase = 1;
                let scope = path_context(&self.root, self.allow_outside_root);
                let instructions = format!("{scope} Use ranges or output_path for large results.");
                json!({"protocolVersion":v,"capabilities":{"tools":{}},"serverInfo":{"name":"rdocs2md","version":env!("CARGO_PKG_VERSION")},"instructions":instructions})
            }
            _ if self.phase != 2 => {
                return Some(rpc_error(id, -32002, "Server is not initialized"));
            }
            "tools/list" => json!({"tools":self.defs}),
            "tools/call" => {
                let name = p["name"].as_str().unwrap_or("");
                let Some(def) = self.defs.iter().find(|d| d["name"] == name) else {
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
                if name == "list_supported_formats" {
                    return Some(tool_response(id, name, Reply::json(&support())));
                }
                if self.queued.len() >= QUEUED_CALLS {
                    return Some(rpc_error(id, -32000, "Too many pending tool calls"));
                }
                if self.active.as_ref().is_some_and(|r| r.id == *id)
                    || self.queued.iter().any(|job| job.request.id == *id)
                {
                    return Some(rpc_error(
                        &Value::Null,
                        -32600,
                        "Request ID is already in use",
                    ));
                }
                let seconds = args["timeout_seconds"].as_u64().unwrap_or(TIMEOUT);
                let token = &p["_meta"]["progressToken"];
                self.queued.push_back(Job {
                    request: Request {
                        id: id.clone(),
                        budget: limits::Budget::request(seconds),
                        token: request_id(token).then(|| token.clone()),
                    },
                    name: name.into(),
                    args: args.clone(),
                });
                return None;
            }
            _ => return Some(rpc_error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }

    fn cancel(&mut self, id: &Value) {
        if self.active.as_ref().is_some_and(|r| r.id == *id) {
            self.active.take().unwrap().budget.cancel();
        } else {
            self.queued.retain(|job| job.request.id != *id);
        }
    }

    fn expire(&mut self, out: &mut impl Write) -> Result<()> {
        let now = Instant::now();
        let expired = |r: &Request| r.budget.deadline().is_some_and(|t| t <= now);
        if self.active.as_ref().is_some_and(expired) {
            let request = self.active.take().unwrap();
            write_frame(out, &tool_error(&request.id, "time limit exceeded"))?;
        }
        let mut ids = Vec::new();
        self.queued.retain(|job| {
            if expired(&job.request) {
                ids.push(job.request.id.clone());
                false
            } else {
                true
            }
        });
        for id in ids {
            write_frame(out, &tool_error(&id, "time limit exceeded"))?;
        }
        Ok(())
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.active
            .iter()
            .chain(self.queued.iter().map(|job| &job.request))
            .filter_map(|request| request.budget.deadline())
            .min()
    }

    fn run(
        &mut self,
        events: mpsc::Receiver<Event>,
        jobs: mpsc::Sender<Job>,
        out: &mut impl Write,
    ) -> Result<()> {
        let mut closed = false;
        loop {
            self.expire(out)?;
            if !self.worker_busy
                && let Some(job) = self.queued.pop_front()
            {
                self.active = Some(job.request.clone());
                self.worker_busy = true;
                self.send_progress(out, 0, None)?;
                jobs.send(job).map_err(|_| "MCP document worker stopped")?;
            }
            if closed && self.active.is_none() && self.queued.is_empty() {
                return Ok(());
            }
            let event = if let Some(deadline) = self.next_deadline() {
                match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                }
            } else {
                let Ok(event) = events.recv() else {
                    return Ok(());
                };
                event
            };
            match event {
                Event::Input(request) => {
                    if let Some(response) = self.dispatch(request) {
                        write_frame(out, &response)?;
                    }
                }
                Event::InputError(error) => write_frame(out, &error)?,
                Event::Closed => closed = true,
                Event::Progress { done, total } => {
                    self.expire(out)?;
                    self.send_progress(out, done, Some(total))?;
                }
                Event::Done(response) => {
                    self.expire(out)?;
                    self.worker_busy = false;
                    if self.active.take().is_some() {
                        write_frame(out, &response)?;
                    }
                }
            }
        }
    }

    fn send_progress(&self, out: &mut impl Write, done: usize, total: Option<usize>) -> Result<()> {
        if let Some(token) = self.active.as_ref().and_then(|r| r.token.as_ref()) {
            let mut params = json!({"progressToken":token,"progress":done});
            if let Some(total) = total {
                params["total"] = json!(total);
            }
            write_frame(
                out,
                &json!({"jsonrpc":"2.0","method":"notifications/progress","params":params}),
            )?;
        }
        Ok(())
    }
}

fn tool_error(id: &Value, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":message}],"isError":true}})
}

fn tool_response(id: &Value, name: &str, response: Result<Reply>) -> Value {
    let (text, warnings, bad) = match response {
        Ok(reply) => {
            let (text, warnings) = reply.into_parts();
            (text, warnings, false)
        }
        Err(e) => (e.to_string(), Vec::new(), true),
    };
    if text.len() > INLINE {
        return tool_error(id, oversized(name));
    }
    let mut content = vec![json!({"type":"text","text":text})];
    if !warnings.is_empty() {
        content.push(json!({
            "type":"text",
            "text":format!("Conversion warnings (the result may be incomplete):\n- {}", warnings.join("\n- ")),
        }));
    }
    let result = json!({"content":content,"isError":bad});
    if result.to_string().len() > INLINE {
        return tool_error(id, oversized(name));
    }
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn write_frame(out: &mut impl Write, frame: &Value) -> Result<()> {
    serde_json::to_writer(&mut *out, frame)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}
pub fn serve(path: &Path) -> Result<()> {
    serve_with_access(path, false)
}
pub fn serve_with_access(path: &Path, allow_outside_root: bool) -> Result<()> {
    let root = path.canonicalize()?;
    if !root.is_dir() {
        return Err("MCP root must be a directory".into());
    }
    let mut session = Session::new(root.clone(), allow_outside_root);
    let (events, receive) = mpsc::sync_channel(16);
    let (jobs, work) = mpsc::channel();
    let reader = events.clone();
    thread::Builder::new()
        .name("mcp-stdin".into())
        .spawn(move || {
            read_input(&reader);
            let _ = reader.send(Event::Closed);
        })?;
    let worker = thread::Builder::new()
        .name("mcp-documents".into())
        .spawn(move || {
            let mut access = Access {
                root,
                allow_outside_root,
                cache: Default::default(),
            };
            run_worker(&mut access, work, &events);
        })?;
    let result = session.run(receive, jobs, &mut io::stdout().lock());
    if let Some(request) = session.active.take() {
        request.budget.cancel();
    }
    // A cancelled or timed-out worker may still be inside an uninterruptible
    // decoder. Wait for an idle worker to release its cache, but allow exit otherwise.
    if !session.worker_busy {
        let _ = worker.join();
    }
    result
}

fn run_worker(access: &mut Access, jobs: mpsc::Receiver<Job>, events: &mpsc::SyncSender<Event>) {
    for job in jobs {
        let _budget = job.request.budget.enter();
        let mut progress = Progress {
            events: job.request.token.as_ref().map(|_| events),
            last: Instant::now(),
        };
        // The cache is only mutated after parsing, so a caught panic leaves it intact.
        let response = recover_document(AssertUnwindSafe(|| {
            call(
                access,
                &job.name,
                &job.args,
                &job.request.budget,
                &mut progress,
            )
        }));
        let response = limits::check().and(response);
        if response.is_ok()
            && !matches!(job.name.as_str(), "convert_documents" | "search_documents")
        {
            progress.advance(1, 1);
        }
        if events
            .send(Event::Done(tool_response(
                &job.request.id,
                &job.name,
                response,
            )))
            .is_err()
        {
            break;
        }
    }
}

fn read_input(events: &mpsc::SyncSender<Event>) {
    let mut stdin = io::stdin().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        // A closed or broken input stream means the client disconnected.
        let Ok(n) = std::io::Read::take(&mut stdin, FRAME + 1).read_until(b'\n', &mut line) else {
            break;
        };
        if n == 0 {
            break;
        }
        let event = if n as u64 > FRAME {
            if line.last() != Some(&b'\n') && stdin.skip_until(b'\n').is_err() {
                break;
            }
            Event::InputError(rpc_error(
                &Value::Null,
                -32600,
                "Request frame exceeds 1 MiB",
            ))
        } else {
            match serde_json::from_slice::<Value>(&line) {
                Ok(r) => Event::Input(r),
                Err(_) => Event::InputError(rpc_error(&Value::Null, -32700, "Parse error")),
            }
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_instructions_name_the_root() {
        let mut session = Session::new(PathBuf::from("C:/docs-root"), false);
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        });
        let reply = session.dispatch(request).unwrap();
        let instructions = reply["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("MCP root: C:/docs-root"));
        assert!(instructions.contains("if your workspace root differs, use absolute paths."));
        assert!(instructions.contains("Keep paths within this root."));
    }

    fn call(id: u32, seconds: u64, token: Value) -> Event {
        Event::Input(
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                "name":"inspect_document","arguments":{"path":"test.pdf","timeout_seconds":seconds},
                "_meta":{"progressToken":token}
            }}),
        )
    }

    fn cancel(id: u32) -> Event {
        Event::Input(
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}}),
        )
    }

    fn session() -> Session {
        let mut session = Session::new(PathBuf::from("."), false);
        session.phase = 2;
        session
    }

    fn frames(bytes: &[u8]) -> Vec<Value> {
        str::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn handles_ping_and_cancellation_while_waiting_for_worker() {
        let (events, receive) = mpsc::channel();
        let (jobs, work) = mpsc::channel();
        let mut out = Vec::new();
        for event in [
            call(1, 0, json!("active")),
            call(2, 0, json!("queued")),
            Event::Input(json!({"jsonrpc":"2.0","id":3,"method":"ping"})),
            cancel(2),
            cancel(1),
            Event::Progress { done: 1, total: 1 },
            Event::Done(json!({"id":1})),
            call(4, 0, Value::Null),
            Event::Done(json!({"id":4})),
            Event::Closed,
        ] {
            events.send(event).unwrap();
        }
        session().run(receive, jobs, &mut out).unwrap();
        let messages = frames(&out);
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[0]["params"],
            json!({"progressToken":"active","progress":0})
        );
        assert_eq!(messages[1], json!({"jsonrpc":"2.0","id":3,"result":{}}));
        assert_eq!(messages[2]["id"], 4);
        let active = work.recv().unwrap();
        assert_eq!(active.request.id, 1);
        {
            let _budget = active.request.budget.enter();
            assert!(limits::request_interrupted());
        }
        assert_eq!(work.recv().unwrap().request.id, 4);
        assert!(work.try_recv().is_err());
    }

    #[test]
    fn times_out_running_and_queued_calls_without_waiting_for_the_decoder() {
        let (events, receive) = mpsc::channel();
        let (jobs, work) = mpsc::channel();
        let mut out = Vec::new();
        let mut session = session();
        events.send(call(1, 1, Value::Null)).unwrap();
        events.send(call(2, 1, Value::Null)).unwrap();
        events.send(Event::Closed).unwrap();
        session.run(receive, jobs, &mut out).unwrap();
        let messages = frames(&out);
        assert_eq!(
            messages,
            vec![
                tool_error(&json!(1), "time limit exceeded"),
                tool_error(&json!(2), "time limit exceeded")
            ]
        );
        assert_eq!(work.recv().unwrap().request.id, 1);
        assert!(session.worker_busy);
        assert!(work.try_recv().is_err());
    }
}
