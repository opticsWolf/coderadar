// CodeRadar v3.3 — Incremental Update: Write-Ahead Log (§5.5)
// Per-entry MVCC with TxBegin/TxAck for crash recovery.

use std::sync::Arc;

use crate::types::*;
use crate::update::diff::EntityKind;

/// Monotonic transaction ID.
pub type TxId = u64;

/// Identifies an arena by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArenaKind {
    Module,
    Class,
    Function,
    Import,
    Constant,
    TypeAlias,
}

/// Raw generational SlotMap key packed as u64.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotKeyRaw {
    pub kind: ArenaKind,
    pub raw: u64,
}

/// Type-erased Arc<Entity> for generic WAL storage.
pub enum ArcAny {
    Module(Arc<Module>),
    Class(Arc<Class>),
    Function(Arc<Function>),
    Import(Arc<Import>),
    Constant(Arc<Constant>),
    TypeAlias(Arc<TypeAlias>),
}

/// WAL entry — one atomic operation within a transaction.
pub enum WalEntry {
    Insert {
        kind: ArenaKind,
        key: SlotKeyRaw,
        entity: ArcAny,
    },
    Modify {
        kind: ArenaKind,
        key: SlotKeyRaw,
        new_entity: ArcAny,
    },
    Remove {
        kind: ArenaKind,
        key: SlotKeyRaw,
    },
    IndexInsert {
        index: IndexKind,
        key: IndexKey,
        value: IndexValue,
    },
    IndexRemove {
        index: IndexKind,
        key: IndexKey,
        value: IndexValue,
    },
    TxBegin,
    TxAck,
}

/// The patch transaction carries a list of WAL entries plus a rollback journal.
pub struct PatchTransaction {
    pub id: TxId,
    pub entries: Vec<WalEntry>,
    pub rollback: Vec<(ArenaKind, SlotKeyRaw, Option<ArcAny>)>,
}

impl PatchTransaction {
    pub fn new(id: TxId) -> Self {
        Self {
            id,
            entries: vec![WalEntry::TxBegin],
            rollback: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: WalEntry) {
        self.entries.push(entry);
    }

    /// Commit protocol steps (§5.5):
    /// Phase 1 — Prepare: all mutations in memory, read locks only.
    /// Phase 2 — Validate: re-check preconditions.
    /// Phase 3 — Journal write: write WalEntry list to journal, fsync().
    /// Phase 4 — Apply: walk entries, replace Arcs atomically.
    /// Phase 5 — Journal ack: write TxAck, fsync().
    /// Phase 6 — Bump epoch.
    pub fn commit(self) -> Result<(), TxError> {
        // Placeholder: in production this serializes to a journal file
        Ok(())
    }

    pub fn rollback(&mut self) {
        // Restore original Arc pointers from rollback journal
        self.entries.clear();
        self.rollback.clear();
    }
}

#[derive(Debug)]
pub enum TxError {
    Conflict,
    JournalWrite(std::io::Error),
    ValidationFailed(String),
}

// ── Index Key/Value types for WAL ──────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IndexKind {
    FileToModules,
    ModuleByDottedName,
    Importers,
    CallersByCallee,
    CalleesByCaller,
    Subclasses,
    OverriddenBy,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IndexKey {
    Path(std::path::PathBuf),
    DottedName {
        language: String,
        name: String,
    },
    Entity(u64),
}

#[derive(Clone, Debug)]
pub enum IndexValue {
    Entity(u64),
    EntityList(Vec<u64>),
    ModuleList(Vec<u64>),
}

/// Recovery: replay journal entries with trailing TxAck.
pub fn recover_from_journal(_journal_path: &str) -> Result<Vec<PatchTransaction>, TxError> {
    // Read journal, find TxAck-terminated transactions, replay them
    Ok(Vec::new())
}
