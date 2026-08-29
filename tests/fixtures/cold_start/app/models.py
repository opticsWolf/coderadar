"""Domain models.

Exercises the star-export path (`__all__`) plus a class hierarchy with an
override, so a cold load must restore EXTENDS and OVERRIDES structure.
"""

__all__ = ["Base", "Derived", "STATUS"]

STATUS = "active"


class Base:
    def describe(self):
        return "base"


class Derived(Base):
    kind = "derived"

    def describe(self):
        return "derived"

    def clone(self):
        other = Derived()
        return other.describe()
