"""Measure the release CLI. Fetch the optional 14-format corpus first."""
from pathlib import Path
from statistics import median
import subprocess
import tempfile
from time import perf_counter

root = Path(__file__).resolve().parents[1]
exe = root / "target/release/rdocs2md.exe"
corpus = sorted((root / "target/corpus").iterdir())
assert len(corpus) == 14, "run tests/fetch-samples.ps1 first"

def elapsed(paths):
    start = perf_counter()
    for path in paths:
        subprocess.run([exe,path,"--mode","hybrid","--stdout"], stdout=subprocess.DEVNULL,
                       check=True)
    return (perf_counter() - start) * 1000

def bench(label, paths):
    elapsed(paths)
    times = [elapsed(paths) for _ in range(7)]
    size = sum(p.stat().st_size for p in paths)
    print(f"{label}: {size:,} bytes, median {median(times):.1f} ms "
          f"(range {min(times):.1f}–{max(times):.1f} ms)")

print(f"exe: {exe.stat().st_size:,} bytes")
bench("14 documents / 14 processes",corpus)
with tempfile.TemporaryDirectory(dir=root/"target") as directory:
    large = Path(directory)/"large.rtf"
    with large.open("wb") as f:
        f.write(br"{\rtf1\ansi ")
        line = b"A" * 1018 + br"\par " + b"\n"
        for _ in range(16384):
            f.write(line)
        f.write(b"}")
    bench("16 MiB RTF / 1 process",[large])
