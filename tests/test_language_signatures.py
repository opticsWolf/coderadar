"""Signatures are extracted for every Tier-1 language, not just Python.

`extract_parameters` only knew the Python grammar's node kinds
(`identifier`, `typed_parameter`, `default_parameter`), so every parameter
of every PHP, Kotlin, C, C++, Go, Java, Rust and Ruby function was silently
dropped — a PHP method taking `$name` came back as `hello()`. C and C++ hang
the parameter list off the declarator chain rather than off the function
node, so they found nothing even by kind.

The rendered keyword was hardcoded `def` for all of them, so an agent
reading a signature before calling `update_signature` was told a PHP method
was spelled `def hello()`.

The README's Tier-1 table is the claim these tests hold to.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

try:
    from coderadar._core import analyze as _analyze, search_entities
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


# (filename, source, function name, expected parameter name, expected keyword)
CASES = [
    ("s.py", "def hello(name):\n    return name\n",
     "hello", "name", "def "),
    ("s.php", "<?php\nfunction hello($name) {\n    return $name;\n}\n",
     "hello", "$name", "function "),
    ("s.js", "function hello(name) {\n  return name;\n}\n",
     "hello", "name", "function "),
    ("s.ts", "function hello(name: string): string {\n  return name;\n}\n",
     "hello", "name", "function "),
    ("s.kt", "fun hello(name: String): String {\n    return name\n}\n",
     "hello", "name", "fun "),
    ("s.go", "package m\n\nfunc hello(name string) string {\n\treturn name\n}\n",
     "hello", "name", "func "),
    ("s.rs", "fn hello(name: String) -> String {\n    name\n}\n",
     "hello", "name", "fn "),
    ("s.rb", "def hello(name)\n  name\nend\n",
     "hello", "name", "def "),
    ("s.java",
     "class C {\n  String hello(String name) {\n    return name;\n  }\n}\n",
     "hello", "name", ""),
    ("s.cpp",
     '#include <string>\n\n'
     'std::string hello(const std::string& name) {\n'
     '    return name;\n}\n',
     "hello", "name", ""),
]

IDS = [c[0] for c in CASES]


@pytest.fixture(scope="module")
def indexed(tmp_path_factory):
    """One project holding a file per language, indexed once."""
    root = tmp_path_factory.mktemp("langs")
    for filename, source, _, _, _ in CASES:
        (root / filename).write_text(source, encoding="utf-8")
    previous = Path(os.getcwd())
    os.chdir(root)
    try:
        _analyze(".")
        yield root
    finally:
        os.chdir(previous)


def _signature(filename: str, function: str) -> str:
    stem = filename.replace(".", "_")
    for hit in search_entities("", 500, "function"):
        if hit.get("name") == function and filename in hit.get("id", ""):
            return hit.get("signature", "")
    raise AssertionError(f"{function} from {filename} ({stem}) is not indexed")


@pytest.mark.parametrize(
    "filename,source,function,parameter,keyword", CASES, ids=IDS)
class TestEveryTier1Language:
    def test_the_function_is_indexed(
            self, indexed, filename, source, function, parameter, keyword):
        assert _signature(filename, function)

    def test_its_parameters_survive(
            self, indexed, filename, source, function, parameter, keyword):
        signature = _signature(filename, function)
        assert parameter in signature, signature
        # The empty-parens rendering is exactly what the dropped parameters
        # produced, for eight of these ten languages.
        assert f"{function}()" not in signature

    def test_it_is_not_spelled_like_python(
            self, indexed, filename, source, function, parameter, keyword):
        signature = _signature(filename, function)
        if keyword:
            assert signature.startswith(keyword), signature
        else:
            # C++/Java write a return type, not a keyword.
            assert signature.startswith(function), signature
        if filename != "s.py" and filename != "s.rb":
            assert not signature.startswith("def "), signature
