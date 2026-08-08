# Fixture: module_a.py — defines functions that module_b imports and calls.

def helper_format(value: str) -> str:
    return f"formatted: {value}"

def helper_parse(value: str) -> int:
    return int(value)

class DataStore:
    def __init__(self):
        self.items = []

    def add(self, item: str) -> None:
        self.items.append(item)
