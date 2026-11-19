#!/usr/bin/env python3
"""Build one benchmark harness per git ref.

usage: build.py OUTDIR name=ref [name=ref ...]

Each ref is checked out into a git worktree, the harness in bench/hx is copied into
it (path deps on ../crates/...), and the release binary is copied to OUTDIR/hx_<name>.
The thread count from HX_THREADS goes through DecoderOptions::set_num_threads, or through
jpeg_set_threads on older refs of the thread branches that have it.
"""
import os, pathlib, shutil, subprocess, sys

root = pathlib.Path(__file__).resolve().parent.parent
out = pathlib.Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
exe = ".exe" if os.name == "nt" else ""
src = (root / "bench" / "hx" / "src" / "main.rs").read_text()
num_threads = src.replace(".jpeg_set_threads(", ".set_num_threads(")
for spec in sys.argv[2:]:
    name, ref = spec.split("=", 1)
    wt = root / "_wt" / name
    if wt.exists():
        subprocess.run(["git", "worktree", "remove", "--force", str(wt)], cwd=root, check=True)
    subprocess.run(["git", "worktree", "add", "--detach", str(wt), ref], cwd=root, check=True)
    hx = wt / "hx"
    shutil.copytree(root / "bench" / "hx", hx, dirs_exist_ok=True)
    opts = (wt / "crates" / "zune-core" / "src" / "options" / "decoder.rs").read_text()
    (hx / "src" / "main.rs").write_text(src if "fn jpeg_set_threads" in opts else num_threads)
    env = dict(os.environ, CARGO_TARGET_DIR=str(root / "_target" / name))
    if name.endswith("_aa"):
        # A/A control: same source, different symbol hashes, so a different code layout.
        env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " -C metadata=aa").strip()
    subprocess.run(["cargo", "build", "--release", "--manifest-path", str(hx / "Cargo.toml")], env=env, check=True)
    shutil.copy2(root / "_target" / name / "release" / f"hx{exe}", out / f"hx_{name}{exe}")
    rev = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=wt, capture_output=True, text=True).stdout.strip()
    print(f"built {name} = {ref} @ {rev}", flush=True)
