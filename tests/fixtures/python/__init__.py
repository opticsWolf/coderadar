"""CodeRadar v3.3 — Test Fixture: Sample Python Project

A minimal project used for integration testing the ingestion pipeline,
resolution cascade, and query execution.
"""

# test_project/main.py
MAIN_PY = '''
"""Main application entry point."""

from app.services import UserService
from app.models import User


def main() -> None:
    """Run the application."""
    service = UserService()
    user = service.create_user("alice@example.com", "Alice")
    print(f"Created user: {user.name}")


if __name__ == "__main__":
    main()
'''

# test_project/app/__init__.py
APP_INIT = '''
"""Application package."""
'''

# test_project/app/models.py
MODELS_PY = '''
"""Data models."""

from dataclasses import dataclass
from typing import Optional


@dataclass
class User:
    """A user entity."""
    email: str
    name: str
    id: Optional[int] = None


class BaseRepository:
    """Base class for data access."""

    def find_by_id(self, id: int):
        raise NotImplementedError


class UserRepository(BaseRepository):
    """User-specific data access."""

    def find_by_email(self, email: str) -> Optional[User]:
        return None
'''

# test_project/app/services.py
SERVICES_PY = '''
"""Business logic services."""

from .models import User, UserRepository


class UserService:
    """Service for user operations."""

    def __init__(self):
        self._repo = UserRepository()

    def create_user(self, email: str, name: str) -> User:
        """Create a new user after validation."""
        user = User(email=email, name=name)
        self._validate_user(user)
        return user

    def _validate_user(self, user: User) -> None:
        """Validate user data."""
        if not user.email or "@" not in user.email:
            raise ValueError("Invalid email")
'''

# test_project/app/utils.py
UTILS_PY = '''
"""Utility functions."""

from typing import Any, Callable


def retry(func: Callable, max_attempts: int = 3) -> Any:
    """Retry a function on failure."""
    for attempt in range(max_attempts):
        try:
            return func()
        except Exception:
            if attempt == max_attempts - 1:
                raise
    return None


def cached_property(func: Callable) -> property:
    """Simple cached property decorator."""
    return property(func)
'''
