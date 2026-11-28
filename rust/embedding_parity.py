"""Record the embedding reference vectors used by the E2 drift contract.

The reference runtime is the *Python* fastembed stack that CodeRadar shipped
before the port. It is not installed in the development venv (the port repo has
no Python ML dependencies), so point `--python` at any interpreter that has
`fastembed` + `onnxruntime`; the model cache is shared with the Rust build
(`%TEMP%/fastembed_cache` on Windows, `~/.cache/fastembed` elsewhere), so the
recording works offline once either side has downloaded it once.

    python rust/embedding_parity.py --python <ref-python>

Writes `rust/golden/embedding_parity.json`; the Rust side of the contract lives
in `core_indexer/src/graph/tests/embedding_parity_tests.rs`.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

MODEL = "BAAI/bge-small-en-v1.5"
DIMENSION = 384

#: Max absolute per-component drift accepted between the reference runtime and
#: the ORT build the binary links. The same model file and tokenizer are used on
#: both sides; measured drift is 4.3e-4 worst-case (mean 2.5e-5, a handful of
#: int8 quantization steps), so the ceiling is 5e-4 — tight enough that a
#: different model or preprocessing could not pass.
TOLERANCE = 5e-4

CORPUS = [
    "def save(self, path: str) -> None: Persist the graph to disk.",
    "class CodeGraph: An indexed, queryable view of one repository.",
    "func (s *Server) Handle(w http.ResponseWriter, r *http.Request) {}",
    "SELECT id, name FROM entities WHERE kind = 'function'",
    "Callers of a function are resolved through the reverse call index.",
    "Embeddings deduplicate work: the cache key is model + body hash.",
    "A temporal query reconstructs the graph as of a commit instant.",
    "resolve_calls walks imports first, then heuristics, then LSP.",
    "The MCP server answers tool calls from the projected graph.",
    "fn main() { let args = std::env::args().collect::<Vec<_>>(); }",
    "Mutation plans are dry runs unless the caller asks to apply them.",
    "评分是余弦相似度，向量按模型归一化。",
]

RECORD = """
import json, sys
from fastembed import TextEmbedding
import fastembed, onnxruntime

model_name = sys.argv[1]
payload = json.loads(sys.stdin.read())
texts = payload["texts"]
model = TextEmbedding(model_name)
vectors = [list(map(float, vector)) for vector in model.embed(texts)]
print(json.dumps({
    "model": model_name,
    "dimension": len(vectors[0]),
    "vectors": vectors,
    "reference": {
        "implementation": "python-fastembed",
        "fastembed": fastembed.__version__,
        "onnxruntime": onnxruntime.__version__,
        "python": sys.version.split()[0],
    },
}))
"""


def record(python: Path, out: Path) -> dict:
    proc = subprocess.run(
        [str(python), "-c", RECORD, MODEL],
        input=json.dumps({"texts": CORPUS}),
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise SystemExit(
            "reference embedding failed:\n"
            + (proc.stderr or proc.stdout or "").strip()
        )
    recorded = json.loads(proc.stdout.strip().splitlines()[-1])
    for vector in recorded["vectors"]:
        if len(vector) != recorded["dimension"]:
            raise SystemExit("reference returned ragged vectors")
    report = {
        "model": recorded["model"],
        "dimension": recorded["dimension"],
        "tolerance": TOLERANCE,
        "texts": CORPUS,
        "vectors": recorded["vectors"],
        "reference": recorded["reference"],
        "note": (
            "Vectors are the reference runtime's output, not a golden by "
            "fiat: the Rust side must stay within `tolerance` and produce the "
            "same ranking. Measured against the port with the same model file "
            "and tokenizer: 4.328e-4 max component drift, 2.46e-5 mean, "
            "1.254e-4 max cosine drift, identical top-3. Bump `tolerance` "
            "only with evidence."
        ),
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--python",
        default=sys.executable,
        help="interpreter with fastembed + onnxruntime (the reference runtime)",
    )
    parser.add_argument(
        "--out",
        default=str(Path(__file__).with_name("golden") / "embedding_parity.json"),
    )
    args = parser.parse_args(argv)
    python = Path(args.python)
    if not python.exists():
        raise SystemExit(f"no such interpreter: {python}")
    report = record(python, Path(args.out))
    ref = report["reference"]
    print(
        f"recorded {len(report['texts'])} texts at {report['dimension']} dims "
        f"with fastembed {ref['fastembed']} / ort {ref['onnxruntime']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
