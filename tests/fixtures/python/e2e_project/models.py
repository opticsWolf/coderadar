# e2e_project/models.py — data models with inheritance

from dataclasses import dataclass
from typing import Optional, List


@dataclass
class User:
    """Base user model."""
    id: int
    name: str
    email: str

    def display_name(self) -> str:
        return f"{self.name} <{self.email}>"

    def is_valid(self) -> bool:
        return "@" in self.email and len(self.name) > 0


class AdminUser(User):
    """Admin user with elevated privileges."""
    permissions: List[str]
    level: int = 1

    def display_name(self) -> str:
        return f"[ADMIN] {self.name}"

    def grant_permission(self, perm: str) -> None:
        self.permissions.append(perm)

    def can_manage_users(self) -> bool:
        return "manage_users" in self.permissions


def find_user_by_id(users: List[User], user_id: int) -> Optional[User]:
    """Find a user in a list by ID."""
    for user in users:
        if user.id == user_id:
            return user
    return None
