"""Target definitions for the call-shape fixtures."""

LIMIT = 3


class Serializer:
    def serialize(self, obj):
        return str(obj)

    @staticmethod
    def make():
        return Serializer()

    @classmethod
    def build(cls):
        return cls()


class Manager:
    def __init__(self):
        self.ser = Serializer()
        self.items = []

    def add(self, item):
        self.items.append(item)  # -> external

    def dump(self):
        return self.ser.serialize(self.items)  # -> Serializer.serialize

    def helper(self):
        return self.add(1)  # -> Manager.add


class Child(Manager):
    def run(self):
        return self.add(2)  # -> Manager.add


def make_manager():
    return Manager()  # -> Manager


def util(x):
    return x
