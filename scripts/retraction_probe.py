"""The retraction case, measured: "this lineage absent from T on".

See docs/temporal-retraction-case.md.  Builds the ledger state the Macrame
v0.23.0 plan's Q5 names -- a lineage holds its own row starting at T5, and a
caller wants absence from T3 -- and prints what today's acts do to it.

Run with the Macrame venv (this imports `macrame`, the published wheel):

    D:/User/Documents/Python/Macrame/.venv/Scripts/python.exe \
        scripts/retraction_probe.py

Read-only against a temporary store; nothing here is a CodeRadar test.
"""
import os
import tempfile

from macrame import ConceptUpsert, Database

ID = "x.py::module"
T2 = "2026-02-01T00:00:00.000000Z"
T3 = "2026-03-01T00:00:00.000000Z"
T4 = "2026-04-01T00:00:00.000000Z"
T5 = "2026-05-01T00:00:00.000000Z"
T6 = "2026-06-01T00:00:00.000000Z"


def line(db, label):
    out = []
    for ts in (T2, T4, T6):
        rows = db.concepts_by_id([ID], branch="feat", as_of_valid=ts)
        out.append(f"{ts[5:7]}:{rows[0].content if rows else '-'}")
    print(f"  {label:<42} {'  '.join(out)}")


def store():
    """feat forked at T3, its own belief [T2, T5) 'a' then [T5, open) 'b'."""
    path = os.path.join(tempfile.mkdtemp(), "retraction.db")
    db = Database.open(path)
    db.fork("feat", "main", at=T3)
    db.upsert_concept(ConceptUpsert(ID, "x", valid_from=T2, branch="feat", content="a"))
    db.upsert_concept(
        ConceptUpsert(ID, "x", valid_from=T5, branch="feat", content="b"),
        supersede=True,
    )
    return db


print("state: own row [T2,T5) 'a', own row [T5,open) 'b'")
line(store(), "before")

# The request, attempted against the existing later row.
db = store()
try:
    db.upsert_concept(
        ConceptUpsert(ID, "x", valid_from=T3, branch="feat", retired=True),
        supersede=True,
    )
    line(db, "retire@T3 (single write)")
except Exception as e:  # noqa: BLE001 - the probe reports the refusal
    print(f"  {'retire@T3 (single write)':<42} refused: {type(e).__name__}")

db = store()
try:
    db.write_concepts([ConceptUpsert(ID, "x", valid_from=T3, branch="feat", retired=True)])
    line(db, "retire@T3 (engine bulk)")
except Exception as e:  # noqa: BLE001 - the probe reports the refusal
    print(f"  {'retire@T3 (engine bulk)':<42} refused: {type(e).__name__}")

# The other order: the act first, the later belief arrives afterwards.  The
# trunk's retirement shape is zero-width; the branch shadow's is open-ended.
for label, kwargs in (
    ("zero-width tombstone@T3, then a later T5 write", {"valid_to": T3}),
    ("open-ended tombstone@T3, then a later T5 supersede", {}),
):
    print(label + ":")
    db = Database.open(os.path.join(tempfile.mkdtemp(), "retraction.db"))
    db.fork("feat", "main", at=T3)
    db.upsert_concept(ConceptUpsert(ID, "x", valid_from=T2, branch="feat", content="a"))
    db.upsert_concept(
        ConceptUpsert(ID, "x", valid_from=T3, branch="feat", retired=True, **kwargs),
        supersede=True,
    )
    line(db, "after the tombstone")
    db.upsert_concept(
        ConceptUpsert(ID, "x", valid_from=T5, branch="feat", content="b"),
        supersede=True,
    )
    line(db, "after the later T5 belief")
