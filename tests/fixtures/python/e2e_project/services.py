# e2e_project/services.py — business logic with cross-module calls

from typing import List
from .models import User, AdminUser, find_user_by_id


def format_username(name: str) -> str:
    """Format a username for display — uses str builtins."""
    return name.strip().title()


def create_user(user_id: int, name: str, email: str) -> User:
    """Create a new User instance."""
    formatted = format_username(name)
    return User(id=user_id, name=formatted, email=email)


class UserService:
    """Service class for user management."""

    def __init__(self):
        self._users: List[User] = []

    def register(self, user_id: int, name: str, email: str) -> User:
        """Register a new user — calls create_user."""
        user = create_user(user_id, name, email)
        self._users.append(user)
        return user

    def find_by_id(self, user_id: int) -> User | None:
        """Find user by ID — calls find_user_by_id from models."""
        return find_user_by_id(self._users, user_id)

    def promote_to_admin(self, user: User, level: int = 1) -> AdminUser:
        """Promote a User to AdminUser."""
        admin = AdminUser(
            id=user.id,
            name=user.name,
            email=user.email,
            permissions=[],
            level=level,
        )
        return admin

    @property
    def user_count(self) -> int:
        """Number of registered users."""
        return len(self._users)
