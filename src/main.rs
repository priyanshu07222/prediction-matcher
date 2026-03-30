//! Stateless API: enqueue orders via Redis, proxy orderbook from matcher, fan out fills over WebSocket.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt as _;
use prediction_matcher::protocol::{reply_list_key, QueuedOrder, FILLS_CHANNEL, ORDERS_QUEUE};
use prediction_matcher::{OrderBookSnapshot, Side};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

static REPLY_KEY_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct AppState {
    redis: redis::aio::ConnectionManager,
    matcher_base: Arc<String>,
    fill_tx: broadcast::Sender<String>,
}

#[derive(Deserialize)]
struct NewOrderBody {
    side: Side,
    price: u64,
    qty: u64,
}

#[derive(Serialize)]
struct OrderAccepted {
    id: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    dotenvy::dotenv().ok();
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let matcher_base =
        std::env::var("MATCHER_HTTP_URL").unwrap_or_else(|_| "http://127.0.0.1:4001".to_string());
    let api_addr = std::env::var("API_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    let client = redis::Client::open(redis_url.as_str())?;
    let redis = redis::aio::ConnectionManager::new(client).await?;

    let (fill_tx, _) = broadcast::channel::<String>(1024);
    let fill_tx_bg = fill_tx.clone();
    let redis_url_bg = redis_url.clone();
    tokio::spawn(async move {
        fill_subscriber_loop(redis_url_bg, fill_tx_bg).await;
    });

    let state = AppState {
        redis,
        matcher_base: Arc::new(matcher_base),
        fill_tx,
    };

    let app = Router::new()
        .route("/orders", post(post_order))
        .route("/orderbook", get(get_orderbook))
        .route("/ws", get(ws_upgrade))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&api_addr).await?;
    eprintln!("API listening on http://{api_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn post_order(
    State(state): State<AppState>,
    Json(body): Json<NewOrderBody>,
) -> Result<Json<OrderAccepted>, (StatusCode, String)> {
    if body.qty == 0 {
        return Err((StatusCode::BAD_REQUEST, "qty must be positive".into()));
    }

    let reply_key = format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        REPLY_KEY_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let queued = QueuedOrder {
        side: body.side,
        price: body.price,
        qty: body.qty,
        reply_key: reply_key.clone(),
    };
    let payload = serde_json::to_string(&queued)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut conn = state.redis.clone();
    redis::cmd("RPUSH")
        .arg(ORDERS_QUEUE)
        .arg(&payload)
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let list_key = reply_list_key(&reply_key);
    let popped = redis::cmd("BRPOP")
        .arg(&list_key)
        .arg(10_i64)
        .query_async::<Option<(String, String)>>(&mut conn)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let id_str = popped.map(|(_, v)| v).ok_or_else(|| {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "matcher did not assign an order id in time".into(),
        )
    })?;

    let id = id_str
        .parse::<u64>()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "invalid order id".into()))?;

    Ok(Json(OrderAccepted { id }))
}

async fn get_orderbook(
    State(state): State<AppState>,
) -> Result<Json<OrderBookSnapshot>, (StatusCode, String)> {
    let url = format!("{}/orderbook", state.matcher_base.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    if !resp.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("matcher returned {}", resp.status()),
        ));
    }
    let snap = resp
        .json::<OrderBookSnapshot>()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(snap))
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_handle(socket, state))
}

async fn ws_handle(mut socket: WebSocket, state: AppState) {
    let mut rx = state.fill_tx.subscribe();
    loop {
        match rx.recv().await {
            Ok(text) => {
                if socket.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn fill_subscriber_loop(redis_url: String, fill_tx: broadcast::Sender<String>) {
    let client = match redis::Client::open(redis_url.as_str()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fill subscriber: redis client: {e}");
            return;
        }
    };
    let mut pubsub = match client.get_async_pubsub().await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fill subscriber: pubsub: {e}");
            return;
        }
    };
    if let Err(e) = pubsub.subscribe(FILLS_CHANNEL).await {
        eprintln!("fill subscriber: subscribe: {e}");
        return;
    }
    let mut stream = pubsub.on_message();
    while let Some(msg) = stream.next().await {
        let payload: String = match msg.get_payload() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("fill subscriber: bad payload: {e}");
                continue;
            }
        };
        let _ = fill_tx.send(payload);
    }
}
