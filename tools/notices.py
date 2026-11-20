"""Regenerate the Windows target's embedded dependency notices from Cargo.lock."""
import gzip
import json
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--locked", "--offline", "--format-version", "1",
    "--filter-platform", "x86_64-pc-windows-msvc",
], cwd=root))
packages = {p["name"]: p for p in metadata["packages"] if p["source"]}
parts = ["rdocs2md third-party notices (Windows x64 build)\n"]
for name, package in sorted(packages.items()):
    directory = Path(package["manifest_path"]).parent
    files = [p for p in directory.rglob("*") if p.is_file() and
             p.name.upper().startswith(("LICENSE", "LICENCE", "COPYRIGHT", "NOTICE", "COPYING"))
             and p.suffix not in (".rs", ".toml")]
    if name == "alloc-stdlib":
        files.append(Path(packages["alloc-no-stdlib"]["manifest_path"]).with_name("LICENSE"))
    if not files:
        raise ValueError(f"No license found in {directory}")
    parts.append(f'\n{name} {package["version"]} | {package["license"]}\n{package["repository"]}\n')
    for path in sorted(files):
        parts.append(f'\n{path.name}\n{path.read_text(encoding="utf-8-sig")}\n')
(root / "THIRD-PARTY-NOTICES.txt").write_text("".join(parts), encoding="utf-8", newline="\n")
sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
stdlib = (sysroot / "share/doc/rust/COPYRIGHT-library.html").read_bytes()
notices = (root / "LICENSE").read_bytes() + b"\n" + "".join(parts).encode("utf-8")
destination = root / "licenses"
destination.mkdir(exist_ok=True)
(destination / "distribution-notices.gz").write_bytes(
    gzip.compress(notices + b"\nRust standard library notices (HTML)\n" + stdlib, mtime=0)
)
