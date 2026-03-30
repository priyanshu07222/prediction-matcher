//! Single matcher process: consumes orders from Redis, owns the [`OrderBook`], publishes fills.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use prediction_matcher::protocol::{reply_list_key, QueuedOrder, FILLS_CHANNEL, ORDERS_QUEUE};
use prediction_matcher::{Order, OrderBook, OrderBookSnapshot};
use redis::AsyncCommands;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
struct AppState {
    book: Arc<Mutex<OrderBook>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let http_addr =
        std::env::var("MATCHER_HTTP_ADDR").unwrap_or_else(|_| "0.0.0.0:4001".to_string());

    let client = redis::Client::open(redis_url)?;
    let redis = redis::aio::ConnectionManager::new(client).await?;

    let book = Arc::new(Mutex::new(OrderBook::new()));
    let next_id = Arc::new(AtomicU64::new(1));

    let redis_consumer = redis.clone();
    tokio::spawn(consumer_loop(Arc::clone(&book), next_id, redis_consumer));

    let app = Router::new()
        .route("/orderbook", get(get_orderbook))
        .with_state(AppState { book });

    let listener = tokio::net::TcpListener::bind(&http_addr).await?;
    eprintln!("matcher HTTP listening on http://{http_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn get_orderbook(State(state): State<AppState>) -> Json<OrderBookSnapshot> {
    Json(state.book.lock().await.snapshot())
}

async fn consumer_loop(
    book: Arc<Mutex<OrderBook>>,
    next_id: Arc<AtomicU64>,
    mut redis: redis::aio::ConnectionManager,
) {
    loop {
        let payload: String = match brpop_payload(&mut redis).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("BRPOP error: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
        };

        let msg: QueuedOrder = match serde_json::from_str(&payload) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("invalid order JSON ({e}): {payload}");
                continue;
            }
        };

        if msg.qty == 0 {
            continue;
        }

        let id = next_id.fetch_add(1, Ordering::SeqCst);
        let order = Order {
            id,
            side: msg.side,
            price: msg.price,
            qty: msg.qty,
        };

        let fills = {
            let mut book = book.lock().await;
            book.submit_order(order)
        };

        for fill in &fills {
            let json = match serde_json::to_string(fill) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("serialize fill: {e}");
                    continue;
                }
            };
            if let Err(e) = redis.publish::<_, _, ()>(FILLS_CHANNEL, json).await {
                eprintln!("PUBLISH fill: {e}");
            }
        }

        let key = reply_list_key(&msg.reply_key);
        if let Err(e) = redis.lpush::<_, _, ()>(&key, id.to_string()).await {
            eprintln!("LPUSH reply {key}: {e}");
        }
        let _: redis::RedisResult<()> = redis.expire(&key, 300).await;
    }
}

async fn brpop_payload(redis: &mut redis::aio::ConnectionManager) -> redis::RedisResult<String> {
    loop {
        let out: Option<(String, String)> = redis.brpop(ORDERS_QUEUE, 0.0).await?;
        if let Some((_, payload)) = out {
            return Ok(payload);
        }
    }
}
