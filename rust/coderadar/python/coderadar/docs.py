"""Reference documents bundled with the binary."""

from . import doc_description, doc_names, doc_text


class DocNotFound(LookupError):
    """A document name outside `doc_names()`."""


__all__ = ["DocNotFound", "doc_description", "doc_names", "doc_text"]
