//! What model, what effort, and who decided — ADR-0016, seam-contract D17.
//!
//! Three concerns, one per file, and the split is the point:
//!
//! - [`catalogue`] — *which* models and effort levels exist, and the planner's
//!   own budget. Configuration, not constants, because ADR-0016 is explicit
//!   that "a new model does not require a release".
//! - [`settings`] — the global and per-repository defaults, and the approval
//!   flag. Four `settings` keys, in the shape D3 fixed and D16.2 repeated.
//! - [`resolve`] — the pure precedence chain that turns a task plus two
//!   defaults into the two flags a run actually spawns with. No I/O, no
//!   [`ServiceContext`](crate::ServiceContext), no database.
//!
//! Nothing here spawns a planner or writes to `tasks`. The strategy *run* is
//! [`crate::runner`]'s, and writing a proposal back onto a card is
//! [`crate::tasks`]', so that both the board and the MCP server reach the same
//! rules through the same functions (ADR-0006).

pub mod catalogue;
pub mod resolve;
pub mod settings;

pub use catalogue::{Catalogue, CatalogueEntry, PlannerBudget, DEFAULT_CATALOGUE_JSON};
pub use resolve::{effective_strategy, EffectiveStrategy, StrategyOrigin};
pub use settings::{StrategyApproval, StrategyDefaults};
