"""`coderadar.ops` over the Rust engine.

Every operation is a native export; the error taxonomy is re-exported from
the extension so `except coderadar.ops.NoIndex` keeps working.
"""

from . import *  # noqa: F401,F403
from . import __all__ as _ALL

__all__ = sorted(_ALL)
