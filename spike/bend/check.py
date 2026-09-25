#!/usr/bin/env python3
"""The spike's gate: the laws, the build, and the port held to `prov check`.

    python3 check.py            # everything
    python3 check.py --corpus   # the corpus only, against an existing build

1. `bend PROOF.bend` must print "All terms check."
2. The Rust shim is built and tested, `main.bend` is emitted to C and linked
   against it with clang (`bend -o` links nothing of ours).
3. Every workspace in the corpus — written into a temporary directory outside
   the repository, never committed, since a fixture's `part_of: /README.md`
   anywhere under prov's own tree would claim prov's root — is checked by `prov check --json` and by the port. The
   findings of the kinds the port models must be the same lines in the same
   order. A workspace where prov reports a kind the port leaves to prov is
   compared on the modelled kinds alone, and says so; one where prov reports
   a `missing_containment` island is compared without orphans, which islands
   subtract from.
4. The same, on prov's own repository and on a generated workspace of a few
   thousand documents, timed.
"""
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
BUILD = HERE / "build"
BEND = os.environ.get("BEND", shutil.which("bend") or str(Path.home() / ".bend/bin/bend"))
CC = os.environ.get("CC_BEND", "clang")
MODELLED = {"broken_link", "case_mismatch", "duplicate_containment", "missing_inverse", "unreadable", "orphan"}


def run(*cmd, cwd=None, timeout=1800, check=True):
    env = dict(os.environ, BEND_NO_TELEMETRY="1")
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout, env=env)
    if check and r.returncode != 0:
        sys.exit(f"failed: {' '.join(map(str, cmd))}\n{r.stdout}\n{r.stderr}")
    return r


# --- the corpus -----------------------------------------------------------------
#
# Each case is a workspace: a map from path to text (bytes where the case is
# about bytes). Every root is `README.md`.

def fm(**keys):
    lines = ["---"]
    for k, v in keys.items():
        if isinstance(v, list):
            lines.append(f"{k}:")
            lines += [f"- {json.dumps(x) if isinstance(x, str) else x}" for x in v]
        else:
            lines.append(f"{k}: {json.dumps(v) if isinstance(v, str) else v}")
    lines.append("---")
    return "\n".join(lines) + "\n"


CASES = {
    "clean": {
        "README.md": fm(title="Home", contents=["[A](a.md)", "[B](sub/b.md)"]) + "\nSee [A](a.md).\n",
        "a.md": fm(title="A", part_of="[Home](README.md)") + "\nBack [home](README.md).\n",
        "sub/b.md": fm(title="B", part_of="[Home](/README.md)", contents=["c.md"]),
        "sub/c.md": fm(title="C", part_of="b.md"),
    },
    "broken": {
        "README.md": fm(title="Home", contents=["[A](a.md)", "[Gone](gone.md)", "sub/missing.md"],
                        links=["[Nowhere](nowhere.md)", "https://example.org/x"], about="about.md")
        + "\nA [dead link](dead.md), ![a picture](img/pic.png), and [[Ghost Page]].\n",
        "a.md": fm(title="A", part_of="README.md") + "\n[same place](#top) and [elsewhere](a.md#x)\n",
    },
    "case": {
        "README.md": fm(title="Home", contents=["A.md", "b.md"], links=["B.md"]) + "\n[x](SUB/c.md)\n",
        "a.md": fm(title="A", part_of="README.md"),
        "b.md": fm(title="B", part_of="README.md"),
        "sub/c.md": "no metadata\n",
    },
    "containment": {
        "README.md": fm(title="Home", contents=["a.md", "b.md", "a.md"]),
        "a.md": fm(title="A", part_of="README.md", contents=["shared.md", "README.md"]),
        "b.md": fm(title="B", part_of="README.md", contents=["shared.md"]),
        "shared.md": fm(title="Shared", part_of="a.md"),
    },
    "inverse": {
        "README.md": fm(title="Home", contents=["a.md", "b.md", "c.md", "d.md", "e.md"]),
        "a.md": fm(title="A", part_of="elsewhere.md"),
        "b.md": fm(title="B"),
        "c.md": "no metadata at all\n",
        "d.md": fm(title="D", part_of=["x.md", "README.md"]),
        "e.md": fm(title="E", part_of="/README.md#top"),
    },
    "unreadable": {
        "README.md": fm(title="Home", contents=["bad.md", "../outside.md", "latin.md", "ok.md"]),
        "bad.md": "---\ntitle: [unclosed\npart_of: README.md\n---\n",
        "latin.md": b"---\ntitle: caf\xe9\npart_of: README.md\n---\n",
        "ok.md": fm(title="OK", part_of="README.md"),
    },
    "orphans": {
        "README.md": fm(title="Home", contents=["sub/a.md"]),
        "sub/a.md": fm(title="A", part_of="/README.md"),
        "loose.md": "# loose\n",
        "sub/also.dj": "# djot\n",
        "sub/page.html": "<p>html</p>\n",
        "sub/.hidden.md": "hidden\n",
        "sub/notes.txt": "not a document\n",
        "sub-z.md": "# sorts after sub/ by component\n",
        "unlinked/deep.md": "# a directory nothing reaches\n",
    },
    "ids": {
        "README.md": fm(title="Home", config="prov.yaml", registry="registry.yaml",
                        contents=["id:n0t30bm", "id:gh0st0t", "id:org/kv2bv2m", "id:self/n0t30ct", "id:n0t30d1",
                                  "id:", "id:a/b/c", "id:aaaaaaa"]),
        "prov.yaml": "workspace_id: self\n",
        "registry.yaml": "registry:\n  r00t002: README.md\n  n0t30bm: notes/a.md\n  n0t30ct: notes/b.md\n"
                         "  n0t30d1: notes/gone.md\n",
        "notes/a.md": fm(title="A", part_of="id:r00t002"),
        "notes/b.md": fm(title="B", part_of="[Home](id:self/r00t002)"),
        "notes/c.md": fm(title="C", part_of="id:gh0st0t"),
    },
    "aliases": {
        "README.md": fm(title="Home", contents=["a.md", "b.md", "LICENSE-TEXT"])
        + "\n[[Alpha]] [[b]] [[Twin]] [[Nobody]] [licence](LICENSE-TEXT) [[Alpha#part]]\n",
        "a.md": fm(title="Alpha", part_of="[[Home]]"),
        "b.md": fm(title="Twin", part_of="README.md"),
        "c.md": fm(title="Twin"),
        "LICENSE-TEXT": "not a document\n",
    },
    "attachments": {
        "README.md": fm(title="Home", contents=["pic.md.yaml", "notes/report.yaml"]) + "\n[[Pic]] [[report]]\n",
        "pic.md.yaml": 'title: Pic\npart_of: README.md\ncontent: pic.md\nattachment: true\n',
        "pic.md": "the payload\n",
        "notes/report.yaml": 'title: Report\npart_of: /README.md\ncontent: Report.MD\n',
        "notes/report.md": "a separated body under another case\n",
    },
    "paths": {
        "README.md": fm(title="Home", contents=["[P](<with space (1).md>)", "[Q](dir/./q.md)", "dir/../r.md",
                                                "/dir/s.md", "[T](t.md) trailing words", "[unterminated](u.md"])
        + "\n[paren](dir/with (a (b)).md) [angle](<dir/x y.md>)\n",
        "with space (1).md": fm(title="P", part_of="README.md"),
        "dir/q.md": fm(title="Q", part_of="../README.md"),
        "r.md": fm(title="R", part_of="./README.md"),
        "dir/s.md": fm(title="S", part_of="/README.md"),
        "t.md": fm(title="T", part_of="README.md"),
        "dir/with (a (b)).md": "# parens\n",
    },
    "scoped": {
        "README.md": fm(title="Home", config="prov.yaml", contents=["a.md", "vendor/v.md"]) + "\n[[Vendored]] [[Local]]\n",
        "prov.yaml": "out_of_scope:\n- vendor/\n",
        "a.md": fm(title="Local", part_of="README.md"),
        "vendor/v.md": fm(title="Vendored", part_of="../README.md"),
    },
    "deep": {
        "README.md": fm(title="Home", contents=["1.md", "2.md", "3.md"]),
        "1.md": fm(title="1", part_of="README.md", contents=["1/a.md", "1/b.md", "gone-1.md"]),
        "1/a.md": fm(title="1a", part_of="../1.md", contents=["../2.md"]),
        "1/b.md": fm(title="1b", part_of="../1.md", links=["missing.md"]),
        "2.md": fm(title="2", part_of="README.md", contents=["2/a.md"]),
        "2/a.md": fm(title="2a", part_of="elsewhere.md"),
        "3.md": fm(title="3", part_of="README.md", contents=["3/x.md"]),
    },
}


def write_case(root, files):
    if root.exists():
        shutil.rmtree(root)
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(text, bytes):
            path.write_bytes(text)
        else:
            path.write_text(text)


def synthetic(root, n_dirs=40, n_notes=60, seed=7):
    """A workspace of a few thousand documents with every modelled finding
    sprinkled through it."""
    rng = random.Random(seed)
    files = {"README.md": fm(title="Synth", contents=[f"[D{d}](d{d}/index.md)" for d in range(n_dirs)])}
    for d in range(n_dirs):
        kids = [f"n{n}.md" for n in range(n_notes)]
        if d % 7 == 3:
            kids.append("gone.md")
        if d % 11 == 5:
            kids.append(f"../d{(d + 1) % n_dirs}/n0.md")
        files[f"d{d}/index.md"] = fm(title=f"D{d}", part_of="../README.md", contents=kids)
        for n in range(n_notes):
            links = []
            for _ in range(4):
                dd, nn = rng.randrange(n_dirs), rng.randrange(n_notes + 2)
                links.append(f"See [N{nn}](/d{dd}/n{nn}.md).")
            if rng.random() < 0.05:
                links.append("[[Nobody Here]]")
            parent = "index.md" if rng.random() > 0.02 else "elsewhere.md"
            files[f"d{d}/n{n}.md"] = fm(title=f"Note {d}.{n}", part_of=parent) + "\n" + "\n\n".join(links) + "\n"
        if d % 5 == 0:
            files[f"d{d}/stray.md"] = "# nothing links here\n"
    write_case(root, files)


# --- comparing --------------------------------------------------------------------

def prov_lines(prov, ws):
    r = run(prov, "-C", str(ws), "check", "--json", check=False)
    findings = json.loads(r.stdout or "[]")
    kinds = {f["kind"] for f in findings}
    return findings, kinds


def compare(prov, port, ws, label):
    findings, kinds = prov_lines(prov, ws)
    islands = "missing_containment" in kinds
    keep = MODELLED - ({"orphan"} if islands else set())
    want = [f["message"] for f in findings if f["kind"] in keep]
    t0 = time.perf_counter()
    r = run(port, str(ws), "README.md", check=False)
    took = time.perf_counter() - t0
    if r.returncode == 3:
        return False, f"{label}: refused — {r.stderr.strip()}", took
    got = [l for l in r.stdout.splitlines() if l.strip()]
    if islands:
        got = [l for l in got if not l.endswith(": orphan — on disk but not linked into the workspace")]
    notes = []
    leftover = sorted(kinds - MODELLED)
    if leftover:
        notes.append("compared on modelled kinds; prov also reports " + ", ".join(leftover))
    if islands:
        notes.append("an island: orphans not compared")
    if got == want:
        return True, f"{label}: ok, {len(want)} finding(s)" + (f" ({'; '.join(notes)})" if notes else ""), took
    diff = [f"  prov: {w}" for w in want if w not in got] + [f"  port: {g}" for g in got if g not in want]
    if not diff:
        diff = ["  same findings, different order:"] + [f"  prov {i}: {w}" for i, w in enumerate(want)] + \
               [f"  port {i}: {g}" for i, g in enumerate(got)]
    return False, f"{label}: DIFFERS\n" + "\n".join(diff), took


def build():
    BUILD.mkdir(exist_ok=True)
    r = run(BEND, "PROOF.bend", cwd=HERE, timeout=3600, check=False)
    if "All terms check." not in r.stdout + r.stderr:
        sys.exit("PROOF.bend does not check:\n" + r.stdout + r.stderr)
    print("laws: all terms check")
    run("cargo", "test", "-q", "--release", cwd=HERE / "ffi", timeout=1800)
    run("cargo", "build", "-q", "--release", cwd=HERE / "ffi", timeout=1800)
    run(BEND, "main.bend", "-o", str(BUILD / "prov-bend.c"), cwd=HERE, timeout=3600)
    run(CC, "-O3", "-w", "-o", str(BUILD / "prov-bend"), str(BUILD / "prov-bend.c"),
        str(HERE / "ffi/target/release/libprov_bend_ffi.a"), "-lm", "-lpthread", "-ldl", timeout=3600)
    print("build: build/prov-bend")


def main():
    if "--corpus" not in sys.argv:
        build()
    prov = os.environ.get("PROV")
    if not prov:
        run("cargo", "build", "-q", "--release", "-p", "prov-cli", cwd=REPO, timeout=3600)
        prov = str(REPO / "target/release/prov")
    port = str(BUILD / "prov-bend")
    corpus = Path(tempfile.mkdtemp(prefix="prov-bend-corpus-"))
    ok = True
    for name, files in CASES.items():
        write_case(corpus / name, files)
        good, line, _ = compare(prov, port, corpus / name, name)
        ok &= good
        print(line)
    good, line, took = compare(prov, port, REPO, "prov itself")
    ok &= good
    print(f"{line} [{took:.2f}s]")
    synthetic(corpus / "synthetic")
    t0 = time.perf_counter()
    run(prov, "-C", str(corpus / "synthetic"), "check", check=False)
    prov_took = time.perf_counter() - t0
    good, line, took = compare(prov, port, corpus / "synthetic", "synthetic")
    ok &= good
    print(f"{line} [port {took:.2f}s, prov {prov_took:.2f}s]")
    shutil.rmtree(corpus)
    if not ok:
        sys.exit(1)


if __name__ == "__main__":
    main()
