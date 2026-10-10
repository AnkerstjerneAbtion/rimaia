//! The machine store's contract suite, against `runner.db` (task 041).
//!
//! The same cases core runs against `MemoryMachine`
//! (`crates/core/tests/machine_store_memory.rs`), here over a real file in a
//! `TempDir`, migrated as the shell migrates it.

use std::sync::Arc;

use rimaia_core::machine::MachineStore;
use rimaia_core::testing::machine_contract::Harness;
use rimaia_runner::RunnerStore;
use tempfile::TempDir;

struct Sqlite {
    store: Arc<RunnerStore>,
    /// Held for its `Drop`: the store's file is inside it.
    _dir: TempDir,
}

impl Harness for Sqlite {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = RunnerStore::open(&dir.path().join("runner.db"))
            .await
            .expect("open the store");
        Self {
            store: Arc::new(store),
            _dir: dir,
        }
    }

    fn store(&self) -> Arc<dyn MachineStore> {
        self.store.clone()
    }
}

rimaia_core::machine_store_contract!(Sqlite);
