use rdocs2md::{
    batch, load_range, model::*, output, parallel, recover_document, render::render, search,
};
use serde::Serialize;
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Write},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
};
const HELP: &str = r#"rdocs2md — offline document conversion
Usage: rdocs2md INPUT... [options]
       rdocs2md mcp [--root DIRECTORY] [--allow-outside-root]
       rdocs2md --list-formats | --licenses | --version | --help

Example: rdocs2md "report.pdf" -o "report.md"

  -o, --output PATH        Destination file, or a directory for multiple inputs
  --out-dir DIRECTORY      Destination directory for batch conversion
  --stdout                 Write the document, or JSON Lines for multiple inputs
  --format md|html|json    Select output format (default: md)
  --mode semantic|layout|hybrid   Markdown rendering mode (default: hybrid)
  --pages RANGE            Select 1-based section indexes, e.g. 1-3,5
  --slides, --sheets, --chapters, --range   Aliases for --pages
  --json PATH              Also save JSON IR to a file, or a directory for multiple inputs
  --extract-images         Extract supported embedded raster images
  --image-dir DIRECTORY    Select image destination (implies extraction)
  -r, --recursive          Scan subfolders for directory and wildcard inputs
  -j, --jobs NUMBER        Parallel workers for batch conversion (1-64, default: CPU count)
  --max-inflight-bytes SIZE  Cap concurrent batch input bytes (default: 1 GiB, 0: none)
  --search QUERY           Search extracted document text and return JSON matches
  --case-sensitive         Perform case-sensitive search
  --limit NUMBER           Maximum search matches per input (default: 50)
  --allow-outside-root     MCP only: permit local input and output outside the root
  --                       Treat remaining arguments as input files
"#;
#[derive(Default)]
struct Args {
    inputs: Vec<(PathBuf, bool)>,
    out: Option<PathBuf>,
    dir: Option<PathBuf>,
    meta: Option<PathBuf>,
    images: bool,
    image_dir: Option<PathBuf>,
    stdout: bool,
    format: String,
    mode: String,
    range: Option<String>,
    recursive: bool,
    jobs: Option<usize>,
    inflight: u64,
    query: Option<search::Query>,
}
fn parse(args: Vec<String>) -> Result<Args> {
    let mut a = Args {
        format: "md".into(),
        mode: "hybrid".into(),
        inflight: parallel::DEFAULT_INFLIGHT_BYTES,
        ..Args::default()
    };
    let mut query = None;
    let mut case_sensitive = false;
    let mut limit = None;
    let mut it = args.into_iter();
    while let Some(s) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("missing value for {s}"));
        match s.as_str() {
            "-o" | "--output" => a.out = Some(val()?.into()),
            "--out-dir" => a.dir = Some(val()?.into()),
            "--json" => a.meta = Some(val()?.into()),
            "--extract-images" => a.images = true,
            "--image-dir" => {
                a.image_dir = Some(val()?.into());
                a.images = true
            }
            "--stdout" => a.stdout = true,
            "-r" | "--recursive" => a.recursive = true,
            "-j" | "--jobs" => {
                let jobs = val()?.parse()?;
                if !(1..=parallel::MAX_JOBS).contains(&jobs) {
                    return Err(format!("--jobs must be 1-{}", parallel::MAX_JOBS).into());
                }
                a.jobs = Some(jobs)
            }
            "--search" => query = Some(val()?),
            "--max-inflight-bytes" => a.inflight = parse_size(&val()?)?,
            "--case-sensitive" => case_sensitive = true,
            "--limit" => limit = Some(val()?.parse()?),
            "--format" => a.format = val()?,
            "--mode" => a.mode = val()?,
            "--range" | "--pages" | "--slides" | "--sheets" | "--chapters" => {
                a.range = Some(val()?)
            }
            "--" => {
                a.inputs.extend(it.map(|s| (PathBuf::from(s), true)));
                break;
            }
            _ if s.starts_with('-') => return Err(format!("unknown option {s}").into()),
            _ => a.inputs.push((s.into(), false)),
        }
    }
    if a.inputs.is_empty() {
        return Err("missing input file (see --help)".into());
    }
    if a.stdout && (a.dir.is_some() || a.out.is_some()) || a.out.is_some() && a.dir.is_some() {
        return Err("choose one output destination".into());
    }
    if a.format == "markdown" {
        a.format = "md".into()
    }
    if !matches!(a.format.as_str(), "md" | "json" | "html")
        || !matches!(a.mode.as_str(), "semantic" | "layout" | "hybrid")
    {
        return Err("invalid format or mode (see --help)".into());
    }
    if let Some(query) = query {
        if a.out.is_some() || a.dir.is_some() || a.meta.is_some() || a.images {
            return Err("--search writes JSON to stdout and cannot be combined with conversion destinations or image extraction".into());
        }
        a.query = Some(search::Query::new(
            &query,
            case_sensitive,
            limit.unwrap_or(search::DEFAULT_LIMIT),
        )?);
    } else if case_sensitive || limit.is_some() {
        return Err("--case-sensitive and --limit require --search".into());
    }
    Ok(a)
}

/// Parse a byte count such as `512`, `4MiB` or `1.5GiB`. Zero removes a cap.
fn parse_size(text: &str) -> Result<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    let value: f64 = number.parse()?;
    let scale = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.,
        "k" | "kb" | "kib" => 1024.,
        "m" | "mb" | "mib" => 1024. * 1024.,
        "g" | "gb" | "gib" => 1024. * 1024. * 1024.,
        _ => return Err(format!("invalid size {text}").into()),
    };
    let bytes = value * scale;
    if !bytes.is_finite() || !(0. ..=u64::MAX as f64).contains(&bytes) {
        return Err(format!("invalid size {text}").into());
    }
    Ok(bytes as u64)
}

fn dest(a: &Args, input: &Path, stem: &OsStr, multi: bool) -> PathBuf {
    if !multi && let Some(path) = &a.out {
        return path.clone();
    }
    let dir = a
        .dir
        .as_deref()
        .or(a.out.as_deref())
        .unwrap_or_else(|| output::parent(input));
    dir.join(batch::output_name(stem, &a.format))
}

#[derive(Serialize)]
struct BatchContent<'a> {
    input: &'a Path,
    content: &'a str,
}

#[derive(Serialize)]
struct BatchSearch<'a, 'doc> {
    input: &'a Path,
    result: search::SearchResult<'doc>,
}

/// One converted input. `emit` is returned instead of printed so that workers
/// never interleave their stdout.
#[derive(Default)]
struct Completed {
    ok: bool,
    emit: Option<String>,
    notes: Vec<String>,
}

fn convert(
    a: &Args,
    input: &Path,
    stem: &OsStr,
    path: &Path,
    meta: Option<&Path>,
    multi: bool,
) -> Result<Completed> {
    let d = load_range(input, a.images, a.range.as_deref())?;
    if let Some(query) = &a.query {
        let result = query.find(&d);
        let mut emit = if multi {
            serde_json::to_string(&BatchSearch { input, result })?
        } else {
            serde_json::to_string(&result)?
        };
        emit.push('\n');
        return Ok(Completed {
            ok: true,
            emit: Some(emit),
            notes: Vec::new(),
        });
    }
    let base = if a.stdout {
        Path::new(".")
    } else {
        output::parent(path)
    };
    let mut links = Properties::new();
    if a.images {
        let dir = match &a.image_dir {
            Some(dir) if multi => dir.join(stem),
            Some(dir) => dir.clone(),
            None => {
                let mut name = stem.to_os_string();
                name.push("_images");
                base.join(name)
            }
        };
        links = output::extract(&d, &dir, base)?;
    }
    let text = render(&d, &a.format, &a.mode, &links)?;
    if let Some(p) = meta {
        output::save(p, serde_json::to_string_pretty(&d)?.as_bytes(), false)?
    }
    let mut notes = Vec::new();
    let emit = if a.stdout {
        if multi {
            let mut line = serde_json::to_string(&BatchContent {
                input,
                content: &text,
            })?;
            line.push('\n');
            Some(line)
        } else {
            Some(text)
        }
    } else {
        output::save(path, text.as_bytes(), false)?;
        notes.push(format!("{} -> {}", input.display(), path.display()));
        None
    };
    for w in &d.warnings {
        notes.push(format!("{}: {w}", input.display()))
    }
    Ok(Completed {
        ok: true,
        emit,
        notes,
    })
}

struct Plan {
    input: PathBuf,
    stem: OsString,
    path: PathBuf,
    meta: Option<PathBuf>,
    /// Problem detected up front so that no worker writes a colliding file.
    error: Option<String>,
}

fn process(a: &Args) -> Result<i32> {
    let mut failed = false;
    let mut inputs = Vec::new();
    let mut taken = HashSet::new();
    let mut scanned = false;
    for (spec, literal) in &a.inputs {
        match batch::expand(spec, a.recursive, *literal, |p| Ok(p.canonicalize()?)) {
            Ok(found) => {
                scanned |= found.scanned;
                inputs.extend(found.files.into_iter().filter(|p| taken.insert(p.clone())));
                for (path, error) in found.errors {
                    eprintln!("{}: {error}", path.display());
                    failed = true;
                }
            }
            Err(e) => {
                eprintln!("{}: {e}", spec.display());
                failed = true;
            }
        }
    }
    if inputs.is_empty() {
        return Ok(1);
    }
    let multi = scanned || a.inputs.len() > 1;
    for dir in a
        .dir
        .iter()
        .chain(a.out.iter().filter(|_| multi))
        .chain(a.meta.iter().filter(|_| multi))
    {
        fs::create_dir_all(dir)?;
    }
    // Assign names and destinations before conversion: workers then own distinct
    // files and the reported results keep the input order.
    let mut stems = batch::OutputNames::default();
    let mut plans = Vec::with_capacity(inputs.len());
    for input in inputs {
        let stem = stems.next(&input);
        let path = dest(a, &input, &stem, multi);
        let meta = a.meta.as_ref().map(|p| {
            if multi {
                p.join(batch::output_name(&stem, "json"))
            } else {
                p.clone()
            }
        });
        let mut error = None;
        for target in (a.query.is_none() && !a.stdout)
            .then_some(&path)
            .into_iter()
            .chain(&meta)
        {
            match output::absolute(target) {
                Ok(absolute) if taken.contains(&absolute) => {
                    error = Some(format!("input/output path collision: {}", target.display()));
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        plans.push(Plan {
            input,
            stem,
            path,
            meta,
            error,
        });
    }
    // Bound the bytes handed to workers so that a large batch does not parse on
    // every core at once.
    let sizes: Vec<u64> = plans
        .iter()
        .map(|plan| fs::metadata(&plan.input).map_or(0, |m| m.len()))
        .collect();
    let mut write_error = None;
    parallel::ordered(
        &plans,
        parallel::jobs_within(a.jobs, &sizes, a.inflight),
        |plan| match &plan.error {
            Some(error) => Completed {
                ok: false,
                emit: None,
                notes: vec![format!("{}: {error}", plan.input.display())],
            },
            None => recover_document(AssertUnwindSafe(|| {
                convert(
                    a,
                    &plan.input,
                    &plan.stem,
                    &plan.path,
                    plan.meta.as_deref(),
                    multi,
                )
            }))
            .unwrap_or_else(|e| Completed {
                ok: false,
                emit: None,
                notes: vec![format!("{}: {e}", plan.input.display())],
            }),
        },
        |done| {
            failed |= !done.ok;
            if let Some(text) = done.emit
                && let Err(e) = io::stdout().lock().write_all(text.as_bytes())
            {
                write_error.get_or_insert(e);
            }
            for note in done.notes {
                eprintln!("{note}")
            }
        },
    );
    if let Some(e) = write_error {
        return Err(e.into());
    }
    Ok(i32::from(failed))
}
fn run() -> Result<i32> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => println!("{HELP}"),
        Some("-V" | "--version") => println!("rdocs2md {}", env!("CARGO_PKG_VERSION")),
        Some("--list-formats") => {
            println!("{}", serde_json::to_string_pretty(&rdocs2md::support())?)
        }
        Some("--licenses") => {
            let mut gz = flate2::read::GzDecoder::new(
                &include_bytes!("../licenses/distribution-notices.gz")[..],
            );
            io::copy(&mut gz, &mut io::stdout().lock())?;
        }
        Some("mcp") => {
            let mut root = None;
            let mut allow_outside_root = false;
            let mut flags = args[1..].iter();
            while let Some(flag) = flags.next() {
                match flag.as_str() {
                    "--root" if root.is_none() => {
                        let dir = flags
                            .next()
                            .filter(|s| !s.is_empty() && !s.starts_with("--"))
                            .ok_or("--root requires a directory")?;
                        root = Some(Path::new(dir));
                    }
                    "--allow-outside-root" if !allow_outside_root => allow_outside_root = true,
                    _ => {
                        return Err(
                            "usage: rdocs2md mcp [--root DIRECTORY] [--allow-outside-root]".into(),
                        );
                    }
                }
            }
            rdocs2md::mcp::serve_with_access(root.unwrap_or(Path::new(".")), allow_outside_root)?
        }
        _ => {
            let a = match parse(args) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("{e}");
                    return Ok(2);
                }
            };
            return process(&a);
        }
    }
    Ok(0)
}
fn main() {
    match run() {
        Ok(n) => std::process::exit(n),
        Err(e) => {
            eprintln!("rdocs2md: {e}");
            std::process::exit(1)
        }
    }
}
