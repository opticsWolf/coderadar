# Fixture: module_b.py — imports from module_a and calls its functions.

from module_a import helper_format, helper_parse
import module_a

def process(value: str) -> str:
    """Calls helper_format from module_a."""
    result = helper_format(value)
    return result

def process_all(values: list) -> list:
    """Calls helper_format for each value."""
    return [helper_format(v) for v in values]

def convert(value: str) -> int:
    """Calls helper_parse."""
    return helper_parse(value)

def store_data(value: str) -> None:
    """Uses module_a.DataStore."""
    store = module_a.DataStore()
    store.add(value)
