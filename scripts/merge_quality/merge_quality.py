#!/usr/bin/env python3
"""foch merge-quality harness.

Measures foch's structural-merge quality against community-authored *compatibility
patches* ("compatches"), which serve as human-written ground truth for "what a good
merge of mod A + mod B looks like".

Pipeline:
  1. discover      Steam Web API QueryFiles -> all EU4 "Compatch" workshop items.
  2. resolve-pairs GetPublishedFileDetails -> regex each description for the patched
                   mod IDs (compatches embed the two mods they patch as workshop URLs).
                   Cached to corpus.json (network only happens here).
  3. filter-local  keep cases where the compatch + all patched mods exist locally in
                   the Steam workshop dir (files cannot be API-downloaded -- ownership).
  4. run           synthesise a 2-mod playset {modA, modB} and run `foch merge`.
  5. score         for every file the compatch hand-merged (ground truth), classify
                   foch's result structurally + semantically and aggregate.

Secrets: the Steam Web API key is read from STEAM_API_KEY (env or .env at repo root).
It is never written to output or committed.

Usage:
  python merge_quality.py discover                 # build/refresh corpus.json (needs key)
  python merge_quality.py run                       # filter-local + merge + score + report
  python merge_quality.py all                        # discover then run
Options: see --help.
"""
from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass, field, asdict
from pathlib import Path

EU4_APPID = 236850
WORKSHOP_URL_RE = re.compile(r"filedetails/\?id=(\d+)")
TOP_KEY_RE = re.compile(r"^([A-Za-z_][\w.\-]*)\s*=\s*\{", re.MULTILINE)
# files in a compatch that are not merged script content
SKIP_NAMES = {"descriptor.mod", "thumbnail.png"}
SKIP_EXTS = {".bak", ".jpg", ".jpeg", ".png", ".dds", ".tga", ".mod"}

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent


# --------------------------------------------------------------------------- env
def load_dotenv(repo_root: Path) -> None:
    """Populate os.environ from repo-root .env (does not overwrite existing env)."""
    env_path = repo_root / ".env"
    if not env_path.is_file():
        return
    for line in env_path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, val = line.partition("=")
        key, val = key.strip(), val.strip().strip('"').strip("'")
        os.environ.setdefault(key, val)


# ------------------------------------------------------------------------- steam
def steam_get(path: str, params: dict) -> dict:
    url = f"https://api.steampowered.com/{path}?" + urllib.parse.urlencode(params)
    with urllib.request.urlopen(url, timeout=40) as resp:
        return json.loads(resp.read().decode("utf-8"))


def steam_post(path: str, params: dict) -> dict:
    data = urllib.parse.urlencode(params).encode("utf-8")
    url = f"https://api.steampowered.com/{path}"
    with urllib.request.urlopen(url, data=data, timeout=60) as resp:
        return json.loads(resp.read().decode("utf-8"))


def discover_compatch_ids(key: str, max_items: int) -> list[str]:
    ids: list[str] = []
    cursor = "*"
    while len(ids) < max_items:
        r = steam_get(
            "IPublishedFileService/QueryFiles/v1/",
            {
                "key": key,
                "appid": EU4_APPID,
                "search_text": "Compatch",
                "numperpage": 100,
                "query_type": 12,
                "return_metadata": 1,
                "cursor": cursor,
            },
        ).get("response", {})
        page = r.get("publishedfiledetails", []) or []
        ids.extend(str(d["publishedfileid"]) for d in page)
        nxt = r.get("next_cursor")
        if not page or not nxt or nxt == cursor:
            break
        cursor = nxt
    seen, out = set(), []
    for i in ids:
        if i not in seen:
            seen.add(i)
            out.append(i)
    return out[:max_items]


def fetch_details(ids: list[str]) -> dict[str, dict]:
    """Batch GetPublishedFileDetails; returns id -> detail dict."""
    out: dict[str, dict] = {}
    for chunk_start in range(0, len(ids), 50):
        chunk = ids[chunk_start : chunk_start + 50]
        params = {"itemcount": len(chunk)}
        for i, fid in enumerate(chunk):
            params[f"publishedfileids[{i}]"] = fid
        resp = steam_post(
            "ISteamRemoteStorage/GetPublishedFileDetails/v1/", params
        ).get("response", {})
        for d in resp.get("publishedfiledetails", []) or []:
            out[str(d["publishedfileid"])] = d
        time.sleep(0.2)
    return out


# -------------------------------------------------------------------------- model
@dataclass
class Case:
    compatch_id: str
    title: str
    patched: list[str]  # workshop ids of the mods this compatch patches


def build_corpus(key: str, max_items: int) -> list[Case]:
    ids = discover_compatch_ids(key, max_items)
    details = fetch_details(ids)
    cases: list[Case] = []
    for cid in ids:
        d = details.get(cid)
        if not d:
            continue
        desc = d.get("description") or ""
        patched = [m for m in dict.fromkeys(WORKSHOP_URL_RE.findall(desc)) if m != cid]
        cases.append(Case(cid, d.get("title", ""), patched))
    return cases


# ----------------------------------------------------------------------- scoring
def top_level_keys(text: str) -> set[str]:
    return set(TOP_KEY_RE.findall(text))


def normalise(text: str) -> list[str]:
    """Whitespace/comment-insensitive line list for similarity scoring."""
    out = []
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        line = re.sub(r"\s+", " ", line)
        if line:
            out.append(line)
    return out


def similarity(a: str, b: str) -> float:
    return difflib.SequenceMatcher(None, normalise(a), normalise(b)).ratio()


def read(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


def ground_truth_files(compatch_dir: Path) -> list[str]:
    out = []
    for p in compatch_dir.rglob("*"):
        if not p.is_file():
            continue
        if p.name in SKIP_NAMES or p.suffix.lower() in SKIP_EXTS:
            continue
        out.append(str(p.relative_to(compatch_dir)).replace("\\", "/"))
    return sorted(out)


@dataclass
class FileScore:
    rel: str
    in_a: bool
    in_b: bool
    overlap: bool
    foch_emitted: bool
    foch_conflict: bool
    similarity: float | None
    keys_match: bool | None
    dropped_keys: list[str] = field(default_factory=list)
    verdict: str = ""


def score_file(
    rel: str,
    mod_a: Path,
    mod_b: Path,
    compatch: Path,
    out_dir: Path,
    conflict_paths: set[str],
) -> FileScore:
    in_a = (mod_a / rel).is_file()
    in_b = (mod_b / rel).is_file()
    overlap = in_a and in_b
    foch_path = out_dir / rel
    foch_emitted = foch_path.is_file()
    foch_conflict = rel in conflict_paths

    compatch_text = read(compatch / rel) or ""
    foch_text = read(foch_path) if foch_emitted else None

    sim = keys_match = None
    dropped: list[str] = []
    if foch_text is not None:
        sim = round(similarity(foch_text, compatch_text), 3)
        fk, ck = top_level_keys(foch_text), top_level_keys(compatch_text)
        keys_match = fk == ck
        union_ab = top_level_keys(read(mod_a / rel) or "") | top_level_keys(
            read(mod_b / rel) or ""
        )
        dropped = sorted(union_ab - fk)

    if foch_conflict:
        verdict = "conflict_withheld"  # foch surfaced it; human resolved by hand
    elif not foch_emitted:
        verdict = "not_emitted"
    elif keys_match and sim is not None and sim >= 0.92:
        verdict = "matches_human"
    elif dropped:
        verdict = "drops_content"
    elif keys_match:
        verdict = "diverges_formatting"  # same definitions, different text
    else:
        verdict = "diverges_structure"

    return FileScore(
        rel, in_a, in_b, overlap, foch_emitted, foch_conflict, sim, keys_match, dropped, verdict
    )


# --------------------------------------------------------------------------- run
def write_playset(tmp: Path, mods: list[tuple[str, Path]]) -> Path:
    """Create dlc_load.json + mod/ugc_<id>.mod descriptors pointing at workshop dirs."""
    (tmp / "mod").mkdir(parents=True, exist_ok=True)
    enabled = []
    for steam_id, ws_dir in mods:
        rel = f"mod/ugc_{steam_id}.mod"
        enabled.append(rel)
        path_val = str(ws_dir).replace("\\", "/")
        (tmp / rel).write_text(
            f'name="{steam_id}"\npath="{path_val}"\nremote_file_id="{steam_id}"\n',
            encoding="utf-8",
        )
    (tmp / "dlc_load.json").write_text(
        json.dumps({"enabled_mods": enabled, "disabled_dlcs": []}), encoding="utf-8"
    )
    return tmp / "dlc_load.json"


def run_merge(foch_bin: Path, dlc_load: Path, out_dir: Path) -> dict:
    proc = subprocess.run(
        [str(foch_bin), "merge", str(dlc_load), "--out", str(out_dir), "--non-interactive"],
        capture_output=True,
        text=True,
        timeout=1200,
    )
    report_path = out_dir / ".foch" / "foch-merge-report.json"
    report = {}
    if report_path.is_file():
        try:
            report = json.loads(report_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            pass
    return {"exit_code": proc.returncode, "stderr_tail": proc.stderr[-2000:], "report": report}


def conflict_rel_paths(report: dict) -> set[str]:
    out = set()
    for c in report.get("conflict_resolutions", []) or []:
        if c.get("path"):
            out.add(c["path"])
    for w in report.get("warnings", []) or []:
        m = re.search(r"for ([\w./\-]+);", w)
        if m:
            out.add(m.group(1))
    return out


def score_case(case: Case, ws_root: Path, foch_bin: Path, keep: bool) -> dict:
    compatch_dir = ws_root / case.compatch_id
    mod_ids = case.patched  # description order: first = base mod, rest overlay
    mod_dirs = [ws_root / m for m in mod_ids]
    gt = ground_truth_files(compatch_dir)

    tmp = Path(tempfile.mkdtemp(prefix=f"foch_mq_{case.compatch_id}_"))
    out_dir = tmp / "out"
    try:
        dlc = write_playset(tmp, list(zip(mod_ids, mod_dirs)))
        merged = run_merge(foch_bin, dlc, out_dir)
        conflicts = conflict_rel_paths(merged["report"])
        mod_a = mod_dirs[0]
        mod_b = mod_dirs[1] if len(mod_dirs) > 1 else mod_dirs[0]
        files = [
            asdict(score_file(rel, mod_a, mod_b, compatch_dir, out_dir, conflicts))
            for rel in gt
        ]
        overlap_files = [f for f in files if f["overlap"]]
        verdicts: dict[str, int] = {}
        for f in overlap_files:
            verdicts[f["verdict"]] = verdicts.get(f["verdict"], 0) + 1
        return {
            "compatch_id": case.compatch_id,
            "title": case.title,
            "patched": mod_ids,
            "merge_exit_code": merged["exit_code"],
            "merge_status": merged["report"].get("status"),
            "validation": merged["report"].get("validation"),
            "ground_truth_files": len(gt),
            "overlap_files": len(overlap_files),
            "verdicts": verdicts,
            "files": files,
            "stderr_tail": merged["stderr_tail"] if merged["exit_code"] not in (0, 2, 3) else "",
        }
    finally:
        if not keep:
            import shutil

            shutil.rmtree(tmp, ignore_errors=True)


# ------------------------------------------------------------------ learn rules
def classify_resolution(rel: str, base: Path, overlay: Path, compatch: Path) -> dict | None:
    """How did the human compatch resolve {base, overlay} for this file?

    Uses unique-line sets: a file's distinctive content is the lines only it has.
    Measures how much of each contributor's *unique* content survived into the
    human's resolution -> kept base / kept overlay / unioned both / hand-edited.
    """
    h = read(compatch / rel)
    a = read(base / rel)
    b = read(overlay / rel)
    if h is None or a is None or b is None:
        return None
    hs, as_, bs = set(normalise(h)), set(normalise(a)), set(normalise(b))
    a_only = as_ - bs
    b_only = bs - as_
    # contributor relationship (order-independent): how redundant are A and B?
    inter, union_ab = as_ & bs, as_ | bs
    jaccard = (len(inter) / len(union_ab)) if union_ab else 1.0
    if not a_only or not b_only:
        relationship = "subset"  # one contributor's unique content ⊆ the other
    elif jaccard >= 0.5:
        relationship = "redundant"  # mechanics overlap heavily (e.g. renamed dupes)
    else:
        relationship = "disjoint"  # genuinely additive content
    fa = (len(hs & a_only) / len(a_only)) if a_only else None
    fb = (len(hs & b_only) / len(b_only)) if b_only else None
    T = 0.5
    keep_a = fa is None or fa >= T
    keep_b = fb is None or fb >= T
    if not a_only and not b_only:
        verdict = "identical"
    elif a_only and b_only:
        if keep_a and keep_b:
            verdict = "union"
        elif keep_a:
            verdict = "took_base"
        elif keep_b:
            verdict = "took_overlay"
        else:
            verdict = "hand_edit"
    elif a_only:  # overlay adds nothing unique
        verdict = "took_base" if keep_a else "hand_edit"
    else:  # base adds nothing unique
        verdict = "took_overlay" if keep_b else "hand_edit"
    return {
        "rel": rel,
        "frac_base_kept": round(fa, 2) if fa is not None else None,
        "frac_overlay_kept": round(fb, 2) if fb is not None else None,
        "ab_jaccard": round(jaccard, 2),
        "relationship": relationship,
        "verdict": verdict,
    }


RES_MEANING = {
    "union": "human kept BOTH contributors' unique content (do-both)",
    "took_base": "human kept the base (first) mod, dropped the overlay's unique content",
    "took_overlay": "human kept the overlay (later) mod = load-order / last-writer",
    "hand_edit": "human wrote something not derivable from either side",
    "identical": "both contributors identical here (no real conflict)",
}


def cmd_learn(args) -> int:
    """Classify how humans resolved each overlapping file -> look for a general rule."""
    res_path = args.results_dir / "results.json"
    if not res_path.is_file():
        print(f"ERROR: {res_path} missing - run `run` first.", file=sys.stderr)
        return 2
    results = json.loads(res_path.read_text(encoding="utf-8"))
    ws = args.workshop_dir

    rows = []  # (case_title, rel, foch_verdict, res)
    for r in results:
        patched = r["patched"]
        if len(patched) < 2:
            continue
        base, overlay, compatch = (
            ws / patched[0],
            ws / patched[1],
            ws / r["compatch_id"],
        )
        for f in r["files"]:
            if not f["overlap"]:
                continue
            res = classify_resolution(f["rel"], base, overlay, compatch)
            if res:
                rows.append((r["title"], f["rel"], f["verdict"], res))

    agg: dict[str, int] = {}
    agg_conflict: dict[str, int] = {}
    crosstab: dict[str, dict[str, int]] = {}  # relationship -> verdict -> count
    for _, _, foch_v, res in rows:
        agg[res["verdict"]] = agg.get(res["verdict"], 0) + 1
        rel_kind = res["relationship"]
        crosstab.setdefault(rel_kind, {})
        crosstab[rel_kind][res["verdict"]] = crosstab[rel_kind].get(res["verdict"], 0) + 1
        if foch_v == "conflict_withheld":
            agg_conflict[res["verdict"]] = agg_conflict.get(res["verdict"], 0) + 1

    out = ["# Human resolution rules (learned from compatches)", ""]
    out.append(f"Overlapping files analysed: **{len(rows)}**")
    out.append("")
    out.append("## Order-independent rule: contributor relationship -> human resolution")
    out.append("")
    out.append("The honest signal is the relationship between the two contributors (not which")
    out.append("side won, which depends on load order). `disjoint`=additive, `redundant`=heavily")
    out.append("overlapping mechanics (e.g. renamed dupes), `subset`=one contained in the other.")
    out.append("")
    out.append("| contributor relationship | human resolutions |")
    out.append("|---|---|")
    for rel_kind in sorted(crosstab, key=lambda k: -sum(crosstab[k].values())):
        dist = ", ".join(
            f"{v}:{n}" for v, n in sorted(crosstab[rel_kind].items(), key=lambda kv: -kv[1])
        )
        out.append(f"| `{rel_kind}` | {dist} |")
    out.append("")
    out.append("## How humans resolve overlaps (ALL overlapping files)")
    out.append("")
    out.append("| human resolution | count | meaning |")
    out.append("|---|---|---|")
    for v in sorted(agg, key=lambda k: -agg[k]):
        out.append(f"| `{v}` | {agg[v]} | {RES_MEANING.get(v, '')} |")
    out.append("")
    out.append("## How humans resolve the conflicts foch WITHHELD (the actionable set)")
    out.append("")
    if agg_conflict:
        out.append("| human resolution | count |")
        out.append("|---|---|")
        for v in sorted(agg_conflict, key=lambda k: -agg_conflict[k]):
            out.append(f"| `{v}` | {agg_conflict[v]} |")
    else:
        out.append("_(no conflict_withheld files in the corpus)_")
    out.append("")
    out.append("## Per-file detail")
    out.append("")
    out.append("| case | file | foch | relationship | human | AB_sim | base_kept | overlay_kept |")
    out.append("|---|---|---|---|---|---|---|---|")
    for title, rel, foch_v, res in rows:
        out.append(
            f"| {title} | `{rel}` | {foch_v} | {res['relationship']} | **{res['verdict']}** | "
            f"{res['ab_jaccard']} | {res['frac_base_kept']} | {res['frac_overlay_kept']} |"
        )
    report = "\n".join(out)
    (args.results_dir / "rules.md").write_text(report, encoding="utf-8")
    print(report)
    print(f"\nWrote {args.results_dir / 'rules.md'}")
    return 0


# ------------------------------------------------------------------------ report
VERDICT_MEANING = {
    "matches_human": "foch's merge \u2248 the hand-written compatch (same defs, >=0.92 similar)",
    "diverges_formatting": "same definitions, different text/formatting",
    "diverges_structure": "different set of top-level definitions vs the human",
    "drops_content": "foch lost a top-level def present in mod A or B (load-order failure mode)",
    "conflict_withheld": "foch surfaced a manual conflict; the human resolved it by hand",
    "not_emitted": "foch produced no file for this path",
}


def render_report(results: list[dict]) -> str:
    lines = ["# foch merge-quality report", ""]
    agg: dict[str, int] = {}
    total_overlap = 0
    for r in results:
        for v, n in r["verdicts"].items():
            agg[v] = agg.get(v, 0) + n
            total_overlap += n
    lines.append(
        f"Cases scored: **{len(results)}**  \u00b7  overlapping ground-truth files: **{total_overlap}**"
    )
    lines.append("")
    lines.append("## Corpus verdicts (overlapping files)")
    lines.append("")
    lines.append("| verdict | count | meaning |")
    lines.append("|---|---|---|")
    for v in sorted(agg, key=lambda k: -agg[k]):
        lines.append(f"| `{v}` | {agg[v]} | {VERDICT_MEANING.get(v, '')} |")
    lines.append("")
    lines.append("## Per-case")
    lines.append("")
    for r in results:
        lines.append(
            f"### {r['title']} (`{r['compatch_id']}`) \u2014 patches {' + '.join(r['patched'])}"
        )
        val = r.get("validation") or {}
        lines.append(
            f"- merge: exit={r['merge_exit_code']} status={r['merge_status']} "
            f"parse_errors={val.get('parse_errors')} "
            f"\u00b7 ground-truth files={r['ground_truth_files']} overlap={r['overlap_files']}"
        )
        lines.append(f"- verdicts: {r['verdicts']}")
        for f in r["files"]:
            if not f["overlap"]:
                continue
            extra = ""
            if f["similarity"] is not None:
                extra += f" sim={f['similarity']}"
            if f["dropped_keys"]:
                extra += f" dropped={f['dropped_keys'][:4]}"
            lines.append(f"  - `{f['rel']}` \u2192 **{f['verdict']}**{extra}")
        lines.append("")
    return "\n".join(lines)


# --------------------------------------------------------------------------- main
def cmd_discover(args) -> int:
    key = os.environ.get("STEAM_API_KEY")
    if not key:
        print("ERROR: STEAM_API_KEY not set (env or repo-root .env).", file=sys.stderr)
        return 2
    cases = build_corpus(key, args.max_items)
    out = {"generated_at": int(time.time()), "cases": [asdict(c) for c in cases]}
    args.corpus.write_text(json.dumps(out, indent=2), encoding="utf-8")
    paired = sum(1 for c in cases if len(c.patched) >= 2)
    print(f"discovered {len(cases)} compatches ({paired} with >=2 patched mods) -> {args.corpus}")
    return 0


def cmd_run(args) -> int:
    if not args.corpus.is_file():
        print(f"ERROR: {args.corpus} missing - run `discover` first.", file=sys.stderr)
        return 2
    if not args.foch_bin.is_file():
        print(f"ERROR: foch binary not found at {args.foch_bin}", file=sys.stderr)
        return 2
    corpus = json.loads(args.corpus.read_text(encoding="utf-8"))
    cases = [Case(**c) for c in corpus["cases"]]
    ws = args.workshop_dir

    local = []
    for c in cases:
        if len(c.patched) < 2:
            continue
        if not (ws / c.compatch_id).is_dir():
            continue
        if all((ws / m).is_dir() for m in c.patched):
            local.append(c)
    print(f"{len(local)} fully-testable local cases (of {len(cases)} discovered).")
    if args.limit:
        local = local[: args.limit]

    results = []
    for c in local:
        print(f"  merging {c.title} ({c.compatch_id}) ...", flush=True)
        results.append(score_case(c, ws, args.foch_bin, args.keep))

    args.results_dir.mkdir(parents=True, exist_ok=True)
    (args.results_dir / "results.json").write_text(
        json.dumps(results, indent=2), encoding="utf-8"
    )
    report = render_report(results)
    (args.results_dir / "report.md").write_text(report, encoding="utf-8")
    print("\n" + report)
    print(f"\nWrote {args.results_dir / 'report.md'} and results.json")
    return 0


def main() -> int:
    load_dotenv(REPO_ROOT)
    default_ws = Path(
        os.environ.get(
            "STEAM_WORKSHOP_DIR",
            r"G:\SteamLibrary\steamapps\workshop\content\236850",
        )
    )
    default_foch = Path(
        os.environ.get("FOCH_BIN", str(REPO_ROOT / "target" / "release" / "foch.exe"))
    )
    p = argparse.ArgumentParser(description="foch merge-quality harness")
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--corpus", type=Path, default=HERE / "corpus.json")
    common.add_argument("--workshop-dir", type=Path, default=default_ws)
    common.add_argument("--foch-bin", type=Path, default=default_foch)
    common.add_argument("--results-dir", type=Path, default=HERE / "results")
    common.add_argument("--max-items", type=int, default=300, help="discover: max compatches")
    common.add_argument("--limit", type=int, default=0, help="run: cap number of cases")
    common.add_argument("--keep", action="store_true", help="run: keep temp merge dirs")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("discover", parents=[common])
    sub.add_parser("run", parents=[common])
    sub.add_parser("all", parents=[common])
    sub.add_parser("learn", parents=[common])
    args = p.parse_args()

    if args.cmd == "learn":
        return cmd_learn(args)
    if args.cmd in ("discover", "all"):
        rc = cmd_discover(args)
        if rc or args.cmd == "discover":
            return rc
    return cmd_run(args)


if __name__ == "__main__":
    raise SystemExit(main())
