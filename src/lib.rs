//! Core domain types and the in-memory matching engine.

use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, VecDeque};

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

/// One aggregated price level for snapshots (sum of resting qty at that price).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: u64,
    pub qty: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    /// Best bids first (highest price).
    pub bids: Vec<PriceLevel>,
    /// Best asks first (lowest price).
    pub asks: Vec<PriceLevel>,
}

/// Price-time matching: bids in `BTreeMap<Reverse<price>>` (best bid = first iteration);
/// asks in `BTreeMap<price>` ascending (best ask = first key). FIFO within each level via `VecDeque`.
#[derive(Debug, Default)]
pub struct OrderBook {
    bids: BTreeMap<Reverse<u64>, VecDeque<Order>>,
    asks: BTreeMap<u64, VecDeque<Order>>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Submit an order: match against the opposite side, then rest any remainder.
    /// Incoming order is the taker when trades occur; resting book orders are makers.
    /// Fills execute at the **maker's** price.
    pub fn submit_order(&mut self, mut order: Order) -> Vec<Fill> {
        let mut fills = Vec::new();
        match order.side {
            Side::Buy => self.match_buy(&mut order, &mut fills),
            Side::Sell => self.match_sell(&mut order, &mut fills),
        }
        if order.qty > 0 {
            self.rest(order);
        }
        fills
    }

    pub fn snapshot(&self) -> OrderBookSnapshot {
        let bids = self
            .bids
            .iter()
            .map(|(k, dq)| PriceLevel {
                price: k.0,
                qty: dq.iter().map(|o| o.qty).sum(),
            })
            .collect();
        let asks = self
            .asks
            .iter()
            .map(|(&price, dq)| PriceLevel {
                price,
                qty: dq.iter().map(|o| o.qty).sum(),
            })
            .collect();
        OrderBookSnapshot { bids, asks }
    }

    fn match_buy(&mut self, order: &mut Order, fills: &mut Vec<Fill>) {
        while order.qty > 0 {
            let ask_price = match self.asks.first_key_value() {
                Some((&p, _)) => p,
                None => break,
            };
            if ask_price > order.price {
                break;
            }
            let level = self.asks.get_mut(&ask_price).unwrap();
            let maker = level.front_mut().unwrap();
            let trade_qty = order.qty.min(maker.qty);
            let maker_id = maker.id;
            let maker_price = maker.price;
            maker.qty -= trade_qty;
            fills.push(Fill {
                maker_order_id: maker_id,
                taker_order_id: order.id,
                price: maker_price,
                qty: trade_qty,
            });
            order.qty -= trade_qty;
            if maker.qty == 0 {
                level.pop_front();
                if level.is_empty() {
                    self.asks.remove(&ask_price);
                }
            }
        }
    }

    fn match_sell(&mut self, order: &mut Order, fills: &mut Vec<Fill>) {
        while order.qty > 0 {
            let bid_key = match self.bids.first_key_value() {
                Some((k, _)) => *k,
                None => break,
            };
            let bid_price = bid_key.0;
            if bid_price < order.price {
                break;
            }
            let level = self.bids.get_mut(&bid_key).unwrap();
            let maker = level.front_mut().unwrap();
            let trade_qty = order.qty.min(maker.qty);
            let maker_id = maker.id;
            let maker_price = maker.price;
            maker.qty -= trade_qty;
            fills.push(Fill {
                maker_order_id: maker_id,
                taker_order_id: order.id,
                price: maker_price,
                qty: trade_qty,
            });
            order.qty -= trade_qty;
            if maker.qty == 0 {
                level.pop_front();
                if level.is_empty() {
                    self.bids.remove(&bid_key);
                }
            }
        }
    }

    fn rest(&mut self, order: Order) {
        debug_assert!(order.qty > 0);
        match order.side {
            Side::Buy => {
                self.bids
                    .entry(Reverse(order.price))
                    .or_default()
                    .push_back(order);
            }
            Side::Sell => {
                self.asks.entry(order.price).or_default().push_back(order);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn o(id: u64, side: Side, price: u64, qty: u64) -> Order {
        Order {
            id,
            side,
            price,
            qty,
        }
    }

    #[test]
    fn buy_matches_lowest_ask_full_fill() {
        let mut book = OrderBook::new();
        book.submit_order(o(1, Side::Sell, 100, 5));
        let fills = book.submit_order(o(2, Side::Buy, 100, 5));
        assert_eq!(fills.len(), 1);
        assert_eq!(
            fills[0],
            Fill {
                maker_order_id: 1,
                taker_order_id: 2,
                price: 100,
                qty: 5,
            }
        );
        assert!(book.snapshot().asks.is_empty());
        assert!(book.snapshot().bids.is_empty());
    }

    #[test]
    fn partial_fill_then_rest() {
        let mut book = OrderBook::new();
        book.submit_order(o(1, Side::Sell, 100, 5));
        let fills = book.submit_order(o(2, Side::Buy, 100, 10));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].qty, 5);
        let snap = book.snapshot();
        assert_eq!(snap.asks.len(), 0);
        assert_eq!(snap.bids.len(), 1);
        assert_eq!(snap.bids[0].price, 100);
        assert_eq!(snap.bids[0].qty, 5);
    }

    #[test]
    fn fifo_at_same_price_level() {
        let mut book = OrderBook::new();
        book.submit_order(o(1, Side::Sell, 100, 2));
        book.submit_order(o(2, Side::Sell, 100, 2));
        let fills = book.submit_order(o(3, Side::Buy, 100, 3));
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].maker_order_id, 1);
        assert_eq!(fills[0].qty, 2);
        assert_eq!(fills[1].maker_order_id, 2);
        assert_eq!(fills[1].qty, 1);
        let snap = book.snapshot();
        assert_eq!(snap.asks[0].qty, 1);
    }

    #[test]
    fn sell_matches_highest_bid() {
        let mut book = OrderBook::new();
        book.submit_order(o(1, Side::Buy, 100, 3));
        let fills = book.submit_order(o(2, Side::Sell, 100, 3));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].maker_order_id, 1);
        assert_eq!(fills[0].qty, 3);
    }

    #[test]
    fn no_trade_when_limit_not_marketable() {
        let mut book = OrderBook::new();
        book.submit_order(o(1, Side::Sell, 101, 1));
        book.submit_order(o(2, Side::Buy, 100, 1));
        let snap = book.snapshot();
        assert_eq!(snap.bids.len(), 1);
        assert_eq!(snap.asks.len(), 1);
    }

    #[test]
    fn concurrent_submits_under_mutex() {
        let book = Arc::new(Mutex::new(OrderBook::new()));
        let mut handles = vec![];
        for i in 0_u64..64 {
            let b = Arc::clone(&book);
            handles.push(thread::spawn(move || {
                let mut book = b.lock().unwrap();
                let id = 100 + i;
                book.submit_order(o(id, Side::Buy, 50, 1));
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let snap = book.lock().unwrap().snapshot();
        let total: u64 = snap.bids.iter().map(|l| l.qty).sum();
        assert_eq!(total, 64);
    }
}
