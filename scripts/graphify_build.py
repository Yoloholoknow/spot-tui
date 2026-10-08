#!/usr/bin/env python3
"""Rebuild graphify-out/ for this repo in one process.

Why not plain `/graphify`: inline `#[cfg(test)]` modules otherwise form their own
communities (test names are sentences), and community numbers are not stable
between runs, so labels must be derived from each community's members in the
same process that writes the report, never assigned by number afterwards.

    python3 scripts/graphify_build.py

Scope comes from .graphifyignore (vendor/, graphify-out*/). Docs come from
graphify's semantic cache; uncached docs are skipped with a warning (run
`/graphify .` to add them).
"""
import json
import re
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path

from graphify.analyze import god_nodes, suggest_questions, surprising_connections
from graphify.build import build_from_json
from graphify.cache import check_semantic_cache
from graphify.cluster import cluster, score_all
from graphify.detect import detect, save_manifest
from graphify.export import to_html, to_json
from graphify.extract import collect_files, extract
from graphify.report import generate

OUT = Path("graphify-out")


def test_ranges(path):
    """1-based (start, end) line ranges of #[cfg(test)] items. The code is rustfmt'd,
    so an item closes at the first line that is exactly its indent plus `}` (brace
    counting breaks on braces inside raw JSON strings)."""
    lines = Path(path).read_text(errors="ignore").split("\n")
    ranges, i = [], 0
    while i < len(lines):
        if lines[i].strip().startswith("#[cfg(test)]"):
            indent = len(lines[i]) - len(lines[i].lstrip())
            close = " " * indent + "}"
            j = i + 1
            while j < len(lines) and lines[j].rstrip() != close:
                j += 1
            ranges.append((i + 1, min(j + 1, len(lines))))
            i = j
        i += 1
    return ranges


def prune_tests(extraction):
    cache, drop = {}, set()
    for n in extraction["nodes"]:
        f, loc = n.get("source_file"), n.get("source_location")
        if not f or not f.endswith(".rs") or not loc or not re.fullmatch(r"L\d+", loc):
            continue
        if f not in cache:
            cache[f] = test_ranges(f) if Path(f).exists() else []
        ln = int(loc[1:])
        if any(a <= ln <= b for a, b in cache[f]):
            drop.add(n["id"])
    extraction["nodes"] = [n for n in extraction["nodes"] if n["id"] not in drop]
    extraction["edges"] = [
        e for e in extraction["edges"] if e["source"] not in drop and e["target"] not in drop
    ]
    extraction["hyperedges"] = [
        h for h in extraction.get("hyperedges", []) if not set(h.get("nodes", [])) & drop
    ]
    return len(drop)


def label_communities(G, communities):
    """Name each community after its dominant source file, so the label always
    matches the members. Duplicates get a numeric suffix."""
    names, used = {}, Counter()
    for cid, members in communities.items():
        files = Counter(
            G.nodes[m].get("source_file") for m in members if G.nodes[m].get("source_file")
        )
        if not files:
            base = "Misc"
        else:
            top, _ = files.most_common(1)[0]
            p = Path(top)
            if p.suffix == ".md" or p.suffix == ".html":
                base = "Docs"
            else:
                stem = p.parent.name if p.stem == "mod" else p.stem
                parent = p.parent.name
                base = stem if parent in ("src", "", stem) else f"{parent}/{stem}"
        used[base] += 1
        names[cid] = base if used[base] == 1 else f"{base} #{used[base]}"
    return names


def main():
    detection = detect(Path("."))
    code_files = []
    for f in detection["files"]["code"]:
        code_files.extend(collect_files(Path(f)) if Path(f).is_dir() else [Path(f)])
    ast = extract(code_files, cache_root=Path("."))

    all_files = [f for fs in detection["files"].values() for f in fs]
    cn, ce, ch, uncached = check_semantic_cache(all_files)
    docs = set(detection["files"].get("document", []))
    missing = [u for u in uncached if u in docs]
    if missing:
        # Not fatal: the code graph is rebuilt from source regardless, and the docs
        # that are cached still contribute. Run `/graphify .` to add the rest.
        print(
            f"warning: {len(missing)} doc(s) not in the semantic cache, left out "
            f"(run `/graphify .` to add them): {[Path(m).name for m in missing[:5]]}",
            file=sys.stderr,
        )

    seen = {n["id"] for n in ast["nodes"]}
    nodes = list(ast["nodes"]) + [n for n in cn if n["id"] not in seen]
    extraction = {"nodes": nodes, "edges": ast["edges"] + ce, "hyperedges": ch}
    dropped = prune_tests(extraction)

    G = build_from_json(extraction)
    communities = cluster(G)
    cohesion = score_all(G, communities)
    labels = label_communities(G, communities)
    questions = suggest_questions(G, communities, labels)
    report = generate(
        G, communities, cohesion, labels, god_nodes(G), surprising_connections(G, communities),
        detection, {"input": 0, "output": 0}, ".", suggested_questions=questions,
    )

    OUT.mkdir(exist_ok=True)
    (OUT / "GRAPH_REPORT.md").write_text(report)
    to_json(G, communities, str(OUT / "graph.json"), force=True)
    to_html(G, communities, str(OUT / "graph.html"), community_labels=labels)
    save_manifest(detection["files"])
    cost = {"runs": [{"date": datetime.now(timezone.utc).isoformat(), "input_tokens": 0,
                      "output_tokens": 0, "files": detection["total_files"]}],
            "total_input_tokens": 0, "total_output_tokens": 0}
    (OUT / "cost.json").write_text(json.dumps(cost, indent=2))
    print(f"pruned {dropped} test nodes; graph: {G.number_of_nodes()} nodes, "
          f"{G.number_of_edges()} edges, {len(communities)} communities")


if __name__ == "__main__":
    main()
