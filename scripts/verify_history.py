#!/usr/bin/env python
"""Prototype of `verify_history` (docs/temporal-code-reconstruction.md §6).

Answers one question per recorded generation: **what fraction of the files
the ledger describes at T can actually have their bytes reconstructed?**

Read-only. It never writes to the store, never archives, never indexes.

## Why this replays the log instead of calling the engine per file

`entity_source_ref_at(id, T)` runs a **full `reconstruct(T)` per call**
(`core_indexer/src/storage.rs:957`). Measured on this repo's store (40k log
entries, 490 modules): ~0.36 s per call, so one 490-module generation costs
~3 minutes and a 12-generation sweep ~35 minutes. That API is shaped for one
read, not for a sweep.

So this prototype replays `transaction_log` payloads directly: one windowed
query per generation gives the latest `concepts` payload per id at that T,
and the payload already carries `extra.coderadar.source_blob`. Blob reads
still go through the engine (`_core.blob_get`), which is where the hot→cold
fallback lives.

That means this tool *does* re-implement a fold, which the philosophy doc
rightly forbids for shipped code (doctrine I). Two consequences, both
deliberate:

1. `--crosscheck N` samples N (id, generation) pairs and compares the replay
   against `entity_source_ref_at`. Any disagreement is printed and counted.
   Run it before trusting a number from this script.
2. **The shipped op must not do this.** It needs one batched primitive —
   `source_refs_at(ids, T)`: one `reconstruct`, N refs — and then the Python
   side is a loop over generations with no fold of its own. That primitive is
   the real deliverable of step 1; this script exists to size the problem
   first.

## Outcomes, per (generation, module)

| outcome | meaning |
|---|---|
| `ok` | digest recorded, bytes resolve, SHA-256 matches the address |
| `no_digest` | described at T, no `source_blob` — skipped, excluded, oversize, disabled, or a pre-blob generation |
| `unresolvable` | digest recorded, `blob_get` returns None — bytes gone (hot-only backup, lost cold file) |
| `digest_mismatch` | bytes resolve but re-hash to a different address — corruption |
| `digest_malformed` | not 64 lowercase hex: invisible to the archive's reference scan, so the bytes were reclaimable |

Each distinct digest is fetched and re-hashed once, not once per occurrence:
the address is the identity.

## Usage

    python scripts/verify_history.py                        # 25 sampled generations
    python scripts/verify_history.py --all                  # every generation
    python scripts/verify_history.py --from 2026-10-01T00:00:00Z
    python scripts/verify_history.py --crosscheck 40        # validate the replay
    python scripts/verify_history.py --json

Exit status is 0 whatever the coverage: this reports, it does not gate.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import random
import sqlite3
import sys
from pathlib import Path

DEFAULT_STORE = Path(".coderadar/store/coderadar.db")
MODULE_SUFFIX = "::module"

# Latest `concepts` payload per entity at a recorded instant, which is the
# fold this script owes the cross-check. `seq_id DESC` breaks ties within one
# stamp: the log is append-only and AUTOINCREMENT, so later seq_id is later
# belief — the same order `temporal/replay.rs` folds in.
FOLD_SQL = """
SELECT entity_id, payload FROM (
    SELECT entity_id, payload,
           ROW_NUMBER() OVER (PARTITION BY entity_id
                              ORDER BY recorded_at DESC, seq_id DESC) AS rn
    FROM transaction_log
    WHERE table_name = 'concepts'
      AND branch_id = :branch
      AND recorded_at <= :ts
      AND entity_id LIKE :suffix
) WHERE rn = 1
"""


def connect(db: Path) -> sqlite3.Connection:
    """Read-only URI handle: a bad path cannot create a second store file."""
    return sqlite3.connect(f"file:{db.as_posix()}?mode=ro", uri=True)


# ── Enumeration ─────────────────────────────────────────────────────────────

def generations(conn: sqlite3.Connection, branch: str, lo: str | None, hi: str | None) -> list[str]:
    """Distinct `recorded_at` stamps in the window, oldest first.

    One stamp is one act the ledger was told about.
    """
    sql = "SELECT DISTINCT recorded_at FROM transaction_log WHERE branch_id = ?"
    params: list[str] = [branch]
    if lo:
        sql += " AND recorded_at >= ?"
        params.append(lo)
    if hi:
        sql += " AND recorded_at <= ?"
        params.append(hi)
    return [r[0] for r in conn.execute(sql + " ORDER BY recorded_at", params)]


def sample(items: list[str], n: int) -> list[str]:
    """`n` stamps spread evenly across `items`, both ends always included."""
    if n <= 0 or len(items) <= n:
        return items
    if n == 1:
        return [items[-1]]
    step = (len(items) - 1) / (n - 1)
    picked = {round(i * step) for i in range(n)} | {0, len(items) - 1}
    return [items[i] for i in sorted(picked)]


# ── The check ───────────────────────────────────────────────────────────────

class Verifier:
    """One pass over sampled generations, with a digest cache across them."""

    def __init__(self, conn: sqlite3.Connection, core, branch: str) -> None:
        self._conn = conn
        self._core = core
        self._branch = branch
        self._verdicts: dict[str, str] = {}

    def digests_at(self, ts: str) -> dict[str, str | None]:
        """`{module_id: digest | None}` for the modules described at `ts`.

        Retired-at-T modules are absent from the mapping, not `None`: "not in
        the graph" and "in the graph without bytes" are different answers and
        only the second is a gap.
        """
        rows = self._conn.execute(
            FOLD_SQL,
            {"branch": self._branch, "ts": ts, "suffix": f"%{MODULE_SUFFIX}"},
        )
        out: dict[str, str | None] = {}
        for entity_id, payload in rows:
            try:
                p = json.loads(payload)
            except json.JSONDecodeError:
                continue
            if p.get("retired"):
                continue
            extra = p.get("extra") or {}
            if isinstance(extra, str):  # defensive: payloads have carried both
                try:
                    extra = json.loads(extra)
                except json.JSONDecodeError:
                    extra = {}
            cr = extra.get("coderadar") or {}
            digest = cr.get("source_blob")
            out[entity_id] = digest if isinstance(digest, str) else None
        return out

    def check_digest(self, digest: str) -> str:
        cached = self._verdicts.get(digest)
        if cached is not None:
            return cached
        if len(digest) != 64 or any(ch not in "0123456789abcdef" for ch in digest):
            verdict = "digest_malformed"
        else:
            try:
                raw = self._core.blob_get(digest)
            except ValueError:
                verdict = "digest_malformed"
            else:
                if raw is None:
                    verdict = "unresolvable"
                elif hashlib.sha256(bytes(raw)).hexdigest() != digest:
                    verdict = "digest_mismatch"
                else:
                    verdict = "ok"
        self._verdicts[digest] = verdict
        return verdict

    def generation(self, ts: str) -> dict:
        described = self.digests_at(ts)
        counts: dict[str, int] = {}
        offenders: dict[str, list[str]] = {}
        for entity_id, digest in described.items():
            outcome = "no_digest" if not digest else self.check_digest(digest)
            counts[outcome] = counts.get(outcome, 0) + 1
            if outcome != "ok":
                offenders.setdefault(outcome, []).append(entity_id)
        return {
            "timestamp": ts,
            "modules": len(described),
            "counts": counts,
            "offenders": offenders,
        }


def crosscheck(verifier: Verifier, core, stamps: list[str], n: int) -> dict:
    """Compare the replay's digest against the engine's for `n` random pairs.

    The engine is the authority; a disagreement means this script's numbers
    are wrong and must not be quoted.
    """
    rng = random.Random(0)
    pairs: list[tuple[str, str]] = []
    for ts in stamps:
        described = verifier.digests_at(ts)
        if described:
            pairs += [(i, ts) for i in described]
    rng.shuffle(pairs)
    checked, agreed, disagreements = 0, 0, []
    for entity_id, ts in pairs[:n]:
        replay = verifier.digests_at(ts).get(entity_id)
        ref = core.entity_source_ref_at(entity_id, ts)
        engine = ref.get("digest") if ref else None
        checked += 1
        if replay == engine:
            agreed += 1
        else:
            disagreements.append(
                {"id": entity_id, "ts": ts, "replay": replay, "engine": engine}
            )
    return {"checked": checked, "agreed": agreed, "disagreements": disagreements}


# ── Reporting ───────────────────────────────────────────────────────────────

def coverage(counts: dict[str, int]) -> float:
    total = sum(counts.values())
    return (counts.get("ok", 0) / total * 100.0) if total else 0.0


def render(report: dict, show_offenders: int) -> str:
    out = [
        f"store            {report['store']}  (branch {report['branch']})",
        f"window           {report['window'][0]} .. {report['window'][1]}",
        f"generations      {report['sampled']} sampled of {report['total_generations']}",
        "",
        (
            f"{'generation':28} {'files':>6} {'ok':>6} {'no_digest':>10} "
            f"{'unresolv':>9} {'mismatch':>9} {'cov%':>7}"
        ),
    ]
    for gen in report["generations"]:
        c = gen["counts"]
        out.append(
            f"{gen['timestamp'][:28]:28} {gen['modules']:>6} {c.get('ok', 0):>6} "
            f"{c.get('no_digest', 0):>10} {c.get('unresolvable', 0):>9} "
            f"{c.get('digest_mismatch', 0):>9} {coverage(c):>6.1f}%"
        )

    t = report["totals"]
    out += [
        "",
        (
            f"totals (file-generations)  ok={t.get('ok', 0)}  no_digest={t.get('no_digest', 0)}  "
            f"unresolvable={t.get('unresolvable', 0)}  mismatch={t.get('digest_mismatch', 0)}  "
            f"malformed={t.get('digest_malformed', 0)}"
        ),
        f"byte coverage              {report['byte_coverage_pct']:.1f}%",
        f"distinct digests checked   {report['distinct_digests']}",
    ]

    cc = report.get("crosscheck")
    if cc:
        out.append(
            f"crosscheck vs engine       {cc['agreed']}/{cc['checked']} agree"
            + ("" if not cc["disagreements"] else "  ** REPLAY IS WRONG **")
        )
        for d in cc["disagreements"][:5]:
            out.append(f"  {d['ts']} {d['id']}: replay={d['replay']} engine={d['engine']}")

    if show_offenders:
        for outcome, paths in sorted(report["offender_sample"].items()):
            total = report["offender_distinct"][outcome]
            shown = paths[:show_offenders]
            out.append(f"\n{outcome} ({total} distinct files):")
            out += [f"  {p}" for p in shown]
            if total > len(shown):
                out.append(f"  ... and {total - len(shown)} more")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Per-generation byte coverage of a CodeRadar ledger.")
    ap.add_argument("--store", type=Path, default=DEFAULT_STORE)
    ap.add_argument("--root", default=".", help="indexed root, for coderadar.load")
    ap.add_argument("--branch", default="main")
    ap.add_argument("--from", dest="lo", default=None, help="window start (canonical UTC)")
    ap.add_argument("--to", dest="hi", default=None, help="window end (canonical UTC)")
    ap.add_argument("--generations", type=int, default=25, help="how many to sample (default 25)")
    ap.add_argument("--all", action="store_true", help="every generation in the window")
    ap.add_argument("--crosscheck", type=int, default=0, metavar="N",
                    help="validate the replay against the engine on N random pairs (slow)")
    ap.add_argument("--offenders", type=int, default=10, help="offending ids to list per outcome")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    if not args.store.exists():
        print(f"no store at {args.store}", file=sys.stderr)
        return 2

    import coderadar
    from coderadar import _core

    lo = _core.normalize_timestamp(args.lo) if args.lo else None
    hi = _core.normalize_timestamp(args.hi) if args.hi else None

    conn = connect(args.store)
    stamps = generations(conn, args.branch, lo, hi)
    if not stamps:
        print("no recorded generations in that window", file=sys.stderr)
        return 2
    picked = stamps if args.all else sample(stamps, args.generations)

    # `blob_get` needs a loaded store; the fold above does not.
    coderadar.load(str(args.store), args.root)

    verifier = Verifier(conn, _core, args.branch)
    gens = [verifier.generation(ts) for ts in picked]

    totals: dict[str, int] = {}
    offender_sample: dict[str, set[str]] = {}
    for gen in gens:
        for outcome, n in gen["counts"].items():
            totals[outcome] = totals.get(outcome, 0) + n
        for outcome, ids in gen["offenders"].items():
            offender_sample.setdefault(outcome, set()).update(ids)

    # `--offenders` caps both renderings, not just the human one: an
    # uncapped JSON report on this store is ~570 KB of entity ids, which
    # is useless as a committed fixture and drowns the counts that matter.
    # The counts above are computed from the full lists first, so capping
    # here never changes a number — only how many examples travel with it.
    cap = args.offenders
    for gen in gens:
        gen["offenders"] = {k: v[:cap] for k, v in gen["offenders"].items()}

    report = {
        "store": str(args.store),
        "root": args.root,
        "branch": args.branch,
        "window": (stamps[0], stamps[-1]),
        "total_generations": len(stamps),
        "sampled": len(picked),
        "offenders_per_outcome": cap,
        "generations": gens,
        "totals": totals,
        "byte_coverage_pct": round(coverage(totals), 2),
        "distinct_digests": len(verifier._verdicts),
        # The sample is capped; the distinct count beside it is not, so the
        # report never understates how many files an outcome covers.
        "offender_sample": {k: sorted(v)[:cap] for k, v in offender_sample.items()},
        "offender_distinct": {k: len(v) for k, v in offender_sample.items()},
    }
    if args.crosscheck:
        report["crosscheck"] = crosscheck(verifier, _core, picked, args.crosscheck)

    print(json.dumps(report, indent=2) if args.json else render(report, args.offenders))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
