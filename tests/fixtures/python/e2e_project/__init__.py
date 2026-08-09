# e2e_project/__init__.py — root package
"""End-to-end test fixture: multi-file Python project with imports, calls,
classes, inheritance, and builtins."""

from .models import User, AdminUser
from .services import UserService, format_username, create_user

__all__ = ["User", "AdminUser", "UserService", "format_username", "create_user"]
