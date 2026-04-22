pub mod curve_registry;
pub mod kyber_pool_state;
pub mod pool_state;

pub use pool_state::PoolState;

/// Thin abstraction for external state containers used by adapters.
///
/// This crate intentionally avoids bundling a concrete runtime state manager.
/// Downstream projects can implement this trait for their own storage/indexer
/// layer and wire adapters without pulling non-math infrastructure.
pub trait PoolStateProvider {}

impl PoolStateProvider for () {}
