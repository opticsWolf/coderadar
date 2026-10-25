"""Reference documents bundled with CodeRadar.

Like the agent skills, these files live under `python-source` so maturin
copies them into the wheel. `coderadar docs <name>` prints one and the MCP
server exposes the same text as a `docs://` resource, so installed users do
not need a repo checkout to read the reference.
"""

from __future__ import annotations

from importlib.resources import files

#: Bundled references, name -> one-line description.
DOCS = {
    "query-language": "Query language and field reference (generated from the grammar).",
}

DOC_NAMES = tuple(DOCS)


class DocNotFound(KeyError):
    """Raised when a name does not identify a bundled document."""


def doc_names() -> tuple[str, ...]:
    """Names of the reference documents this distribution ships."""
    return DOC_NAMES


def doc_description(name: str) -> str:
    """One-line description for `name`."""
    try:
        return DOCS[name]
    except KeyError as exc:
        raise DocNotFound(
            f"unknown document {name!r}; available: {', '.join(DOC_NAMES)}"
        ) from exc


def doc_text(name: str) -> str:
    """Return a bundled reference document verbatim."""
    if name not in DOCS:
        raise DocNotFound(
            f"unknown document {name!r}; available: {', '.join(DOC_NAMES)}"
        )
    return (files(__name__) / f"{name}.md").read_text(encoding="utf-8")


__all__ = [
    "DOCS",
    "DOC_NAMES",
    "DocNotFound",
    "doc_description",
    "doc_names",
    "doc_text",
]
