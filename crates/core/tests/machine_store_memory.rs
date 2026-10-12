//! The machine store's contract suite, against `MemoryMachine` (task 041).
//!
//! The runner crate invokes the same macro over a real `runner.db`
//! (`crates/runner/tests/machine_store_sqlite.rs`); a case that holds here
//! and not there is the bug the suite exists to find.

use std::sync::Arc;

use rimaia_core::machine::MachineStore;
use rimaia_core::testing::machine::MemoryMachine;
use rimaia_core::testing::machine_contract::Harness;

struct Memory {
    store: Arc<MemoryMachine>,
}

impl Harness for Memory {
    async fn start() -> Self {
        Self {
            store: Arc::new(MemoryMachine::new()),
        }
    }

    fn store(&self) -> Arc<dyn MachineStore> {
        self.store.clone()
    }
}

rimaia_core::machine_store_contract!(Memory);
