"""Scaffold fixture: markers, a stub, and (opt-in) one secret."""

# Phase 1: wire the client
# TODO: add retries
API_KEY = "AKIAABCDEFGHIJKLMNOP"


def handler():
    pass


def real_logic(items):
    return sum(i.value for i in items if i.enabled)
