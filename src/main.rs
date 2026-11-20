use rdocs2md::{load, model::*, output, render::render};
use std::{
    collections::HashSet,
    io::{self, Write},
    path::{Path, PathBuf},
};
const HELP: &str = r#"rdocs2md — offline document conversion
Usage: rdocs2md INPUT... [options]
       rdocs2md mcp [--root DIRECTORY] [--allow-outside-root]
       rdocs2md --list-formats | --licenses | --version | --help

Example: rdocs2md "report.pdf" -o "report.md"

  -o, --output FILE        Destination for one input (default: beside input)
  --out-dir DIRECTORY      Destination directory for batch conversion
  --stdout                 Write a single document to standard output
  --format md|html|json    Select output format (default: md)
  --mode semantic|layout|hybrid   Markdown rendering mode (default: hybrid)
  --pages RANGE            Select 1-based section indexes, e.g. 1-3,5
  --slides, --sheets, --chapters, --range   Aliases for --pages
  --json FILE              Also save the selected document IR as UTF-8 JSON
  --extract-images         Extract supported embedded raster images
  --image-dir DIRECTORY    Select image destination (implies extraction)
  --allow-outside-root     MCP only: permit local input and output outside the root
  --                       Treat remaining arguments as input files
"#;
#[derive(Default)]
struct Args {
    inputs: Vec<PathBuf>,
    out: Option<PathBuf>,
    dir: Option<PathBuf>,
    meta: Option<PathBuf>,
    images: bool,
    image_dir: Option<PathBuf>,
    stdout: bool,
    format: String,
    mode: String,
    range: Option<String>,
}
fn parse(args: Vec<String>) -> Result<Args> {
    let mut a = Args {
        format: "md".into(),
        mode: "hybrid".into(),
        ..Args::default()
    };
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
            "--format" => a.format = val()?,
            "--mode" => a.mode = val()?,
            "--range" | "--pages" | "--slides" | "--sheets" | "--chapters" => {
                a.range = Some(val()?)
            }
            "--" => {
                a.inputs.extend(it.map(PathBuf::from));
                break;
            }
            _ if s.starts_with('-') => return Err(format!("unknown option {s}").into()),
            _ => a.inputs.push(s.into()),
        }
    }
    if a.inputs.is_empty() {
        return Err("missing input file (see --help)".into());
    }
    if a.inputs.len() > 1
        && (a.stdout || a.meta.is_some() || a.out.is_some() || a.image_dir.is_some())
    {
        return Err("use --out-dir for multiple input files".into());
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
    Ok(a)
}
fn dest(a: &Args, input: &Path) -> PathBuf {
    a.out.clone().unwrap_or_else(|| {
        let file = input.with_extension(&a.format);
        if let Some(dir) = &a.dir {
            dir.join(file.file_name().unwrap_or_default())
        } else {
            file
        }
    })
}
fn convert(a: &Args, input: &Path) -> Result<()> {
    let mut d = load(input, a.images)?;
    if let Some(r) = &a.range {
        d.select(r)?
    }
    let path = dest(a, input);
    let base = if a.stdout {
        Path::new(".")
    } else {
        output::parent(&path)
    };
    let mut links = Properties::new();
    if a.images {
        let dir = a.image_dir.clone().unwrap_or_else(|| {
            base.join(format!(
                "{}_images",
                input.file_stem().unwrap_or_default().to_string_lossy()
            ))
        });
        links = output::extract(&d, &dir, base)?;
    }
    let text = render(&d, &a.format, &a.mode, &links)?;
    if let Some(p) = &a.meta {
        output::save(p, serde_json::to_string_pretty(&d)?.as_bytes(), false)?
    }
    if a.stdout {
        io::stdout().lock().write_all(text.as_bytes())?
    } else {
        output::save(&path, text.as_bytes(), false)?;
        eprintln!("{} -> {}", input.display(), path.display())
    }
    for w in &d.warnings {
        eprintln!("{}: {w}", input.display())
    }
    Ok(())
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
            if let Some(dir) = &a.dir {
                std::fs::create_dir_all(dir)?
            }
            let mut names = HashSet::new();
            for input in &a.inputs {
                if input.exists() {
                    names.insert(output::absolute(input)?);
                }
            }
            for p in a
                .inputs
                .iter()
                .filter(|_| !a.stdout)
                .map(|p| dest(&a, p))
                .chain(a.meta.iter().cloned())
            {
                if !names.insert(output::absolute(&p)?) {
                    return Err(format!("input/output path collision: {}", p.display()).into());
                }
            }
            let mut failed = false;
            for input in &a.inputs {
                if let Err(e) = convert(&a, input) {
                    eprintln!("{}: {e}", input.display());
                    failed = true
                }
            }
            if failed {
                return Ok(1);
            }
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
