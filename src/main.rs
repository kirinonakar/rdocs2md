use rdocs2md::{batch, load_range, model::*, output, recover_document, render::render, search};
use serde::Serialize;
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    io::{self, Write},
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
    query: Option<search::Query>,
}
fn parse(args: Vec<String>) -> Result<Args> {
    let mut a = Args {
        format: "md".into(),
        mode: "hybrid".into(),
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
            "--search" => query = Some(val()?),
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

fn print_json(value: impl Serialize) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    writeln!(stdout)?;
    Ok(())
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

fn convert(
    a: &Args,
    input: &Path,
    stem: &OsStr,
    path: &Path,
    meta: Option<&Path>,
    multi: bool,
) -> Result<()> {
    let d = load_range(input, a.images, a.range.as_deref())?;
    if let Some(query) = &a.query {
        let result = query.find(&d);
        return if multi {
            print_json(BatchSearch { input, result })
        } else {
            print_json(result)
        };
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
    if a.stdout {
        if multi {
            print_json(BatchContent {
                input,
                content: &text,
            })?;
        } else {
            io::stdout().lock().write_all(text.as_bytes())?;
        }
    } else {
        output::save(path, text.as_bytes(), false)?;
        eprintln!("{} -> {}", input.display(), path.display())
    }
    for w in &d.warnings {
        eprintln!("{}: {w}", input.display())
    }
    Ok(())
}

fn process(a: &Args) -> Result<i32> {
    let mut failed = false;
    let mut inputs = Vec::new();
    let mut names = HashSet::new();
    let mut scanned = false;
    for (spec, literal) in &a.inputs {
        match batch::expand(spec, a.recursive, *literal, |p| Ok(p.canonicalize()?)) {
            Ok(found) => {
                scanned |= found.scanned;
                inputs.extend(found.files.into_iter().filter(|p| names.insert(p.clone())));
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
    let mut stems = batch::OutputNames::default();
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
        let result = recover_document(|| {
            let mut targets = Vec::new();
            for p in (a.query.is_none() && !a.stdout)
                .then_some(&path)
                .into_iter()
                .chain(&meta)
            {
                let absolute = output::absolute(p)?;
                if names.contains(&absolute) || targets.contains(&absolute) {
                    return Err(format!("input/output path collision: {}", p.display()).into());
                }
                targets.push(absolute);
            }
            convert(a, &input, &stem, &path, meta.as_deref(), multi)?;
            Ok(targets)
        });
        match result {
            Ok(targets) => names.extend(targets),
            Err(e) => {
                eprintln!("{}: {e}", input.display());
                failed = true;
            }
        }
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
