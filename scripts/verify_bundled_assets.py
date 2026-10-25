"""Release gate: built artifacts must contain the bundled agent assets.

`tests/test_skill_drift.py` proves the source tree is honest; this proves
what `pip install coderadar-rs` actually delivers. Run it from the release
workflow against `dist/` (or pass explicit wheel/sdist paths).
"""

from __future__ import annotations

import sys
import tarfile
import zipfile
from collections.abc import Iterator
from pathlib import Path

#: Bundled path -> a byte string that must appear in that file.
REQUIRED = {
    "coderadar/skills/coderadar-cli/SKILL.md": b"description:",
    "coderadar/skills/coderadar-mcp/SKILL.md": b"description:",
    "coderadar/docs/query-language.md": b"## Query shape",
}


def _required_name(archive_name: str) -> str | None:
    for required in REQUIRED:
        if archive_name.endswith(required):
            return required
    return None


def _payloads(artifact: Path) -> Iterator[tuple[str, bytes]]:
    if artifact.suffix == ".whl":
        with zipfile.ZipFile(artifact) as archive:
            for name in archive.namelist():
                required = _required_name(name)
                if required:
                    yield required, archive.read(name)
    elif artifact.name.endswith(".tar.gz"):
        with tarfile.open(artifact, "r:gz") as archive:
            for member in archive.getmembers():
                if not member.isfile():
                    continue
                required = _required_name(member.name)
                if required:
                    handle = archive.extractfile(member)
                    if handle is not None:
                        yield required, handle.read()
    else:
        raise ValueError(f"unsupported artifact: {artifact}")


def check(artifact: Path) -> list[str]:
    found = dict(_payloads(artifact))
    problems = [f"missing {name}" for name in REQUIRED if name not in found]
    for name, marker in REQUIRED.items():
        payload = found.get(name)
        if payload is not None and marker not in payload:
            problems.append(f"{name} is missing expected content {marker!r}")
    return problems


def main(argv: list[str]) -> int:
    targets = argv[1:] or ["dist"]
    artifacts: list[Path] = []
    for target in targets:
        path = Path(target)
        if path.is_dir():
            artifacts.extend(sorted(path.glob("*.whl")))
            artifacts.extend(sorted(path.glob("*.tar.gz")))
        else:
            artifacts.append(path)
    if not artifacts:
        print("no wheel or sdist artifacts found", file=sys.stderr)
        return 1

    failed = False
    for artifact in artifacts:
        problems = check(artifact)
        if problems:
            failed = True
            print(f"FAIL {artifact.name}: {'; '.join(problems)}", file=sys.stderr)
        else:
            print(f"OK   {artifact.name}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
