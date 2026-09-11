"""Round-2 dogfood fixture: tiny multi-language project with a re-export
chain, a star-export package, a clone pair, and a smelly function."""
from app import combine


def run(items):
    total = combine(items)
    return total


def main():
    return run([1, 2, 3])


def helper(x):
    return x + 1
