//! Core domain types for the prediction market matcher.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub id: u64,
    pub side: Side,
    pub price: u64,
    pub qty: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    pub maker_order_id: u64,
    pub taker_order_id: u64,
    pub price: u64,
    pub qty: u64,
}
