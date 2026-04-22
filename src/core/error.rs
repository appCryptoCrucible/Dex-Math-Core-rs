//! Error types - ZERO PANIC architecture
//! All errors use thiserror for structured handling


use ethers_core::types::{Address, U256};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, PartialOrd)]
pub enum MathError {
    #[error("Overflow in {operation} with inputs: {inputs:?}")]
    Overflow {
        operation: String,
        inputs: Vec<U256>,
        context: String,
    },

    #[error("Underflow in {operation} with inputs: {inputs:?}")]
    Underflow {
        operation: String,
        inputs: Vec<U256>,
        context: String,
    },

    #[error("Division by zero in {operation}")]
    DivisionByZero { operation: String, context: String },

    #[error("Invalid input for {operation}: {reason}")]
    InvalidInput {
        operation: String,
        reason: String,
        context: String,
    },

    #[error("Precision loss in {operation}")]
    PrecisionLoss {
        operation: String,
        expected: U256,
        actual: U256,
        context: String,
    },
}

#[derive(Debug, Error, PartialEq, PartialOrd)]
pub enum DexError {
    #[error(
        "Insufficient liquidity: pool {pool_address}, required {required}, available {available}"
    )]
    InsufficientLiquidity {
        pool_address: Address,
        required: U256,
        available: U256,
    },

    #[error("Invalid pool: {reason}")]
    InvalidPool { reason: String },

    #[error("Unsupported DEX: {dex_name}")]
    UnsupportedDex { dex_name: String },

    #[error("Math error: {0}")]
    MathError(#[from] MathError),
}

#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("Insufficient data for {strategy}: {reason}")]
    InsufficientData { strategy: String, reason: String },

    #[error("Slippage exceeds limit: {actual_bps} bps > {limit_bps} bps")]
    ExceedsSlippageLimit { actual_bps: u32, limit_bps: u32 },

    #[error("Profit below threshold: {actual} < {threshold}")]
    BelowProfitThreshold { actual: U256, threshold: U256 },

    #[error("DEX error: {0}")]
    DexError(#[from] DexError),
}

#[derive(Debug, Error)]
pub enum BlockchainError {
    #[error("RPC error in {operation}: {error}")]
    RpcError { operation: String, error: String },

    #[error("WebSocket error: {error}")]
    WebSocketError { error: String },

    #[error("Event decoding error: {error}")]
    EventDecodingError { error: String },

    #[error("Block {block_number} not found")]
    BlockNotFound { block_number: u64 },
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Invalid config value for {field}: {value}, reason: {reason}")]
    InvalidValue {
        field: String,
        value: String,
        reason: String,
    },

    #[error("Missing required field: {field}")]
    MissingField { field: String },

    #[error("Parse error: {error}")]
    ParseError { error: String },
}

#[derive(Debug, Error)]
pub enum AccountingError {
    #[error("Database error: {error}")]
    DatabaseError { error: String },

    #[error("Invalid transaction: {reason}")]
    InvalidTransaction { reason: String },

    #[error("Settlement error: {reason}")]
    SettlementError { reason: String },
}

impl From<DexError> for String {
    fn from(error: DexError) -> Self {
        error.to_string()
    }
}

impl From<MathError> for String {
    fn from(error: MathError) -> Self {
        error.to_string()
    }
}
