# Prediction market matcher (toy)

A small **prediction-market order matcher**: HTTP API for orders, **price–time** matching, **WebSocket** fill feed, and **multiple API instances** coordinated through Redis and a single matcher process.

## Video walkthrough (required)

**Add your public 1–2 minute YouTube URL here after you record:**

`https://www.youtube.com/watch?v=REPLACE_ME`

## Architecture (short)

- **One matcher process** owns the in-memory [`OrderBook`](src/lib.rs), consumes orders from a **Redis list** (`orders:incoming`), assigns order IDs, runs matching, **PUBLISH**es each [`Fill`](src/lib.rs) to **`fills:events`**, and serves **`GET /orderbook`** on `MATCHER_HTTP_ADDR` (default `4001`).
- **Any number of API processes** are stateless: **`POST /orders`** enqueues a job and waits on a per-request **reply list** in Redis for the assigned id; **`GET /orderbook`** proxies to the matcher; **`GET /ws`** subscribes to **`fills:events`** and pushes fill JSON to connected clients.

Only the matcher mutates the book, so **orders are not double-matched** across API replicas.

## Run locally

Prerequisites: **Rust**, **Docker** (for Redis).

1. **Redis**

   ```bash
   docker compose up -d
   ```

2. **Matcher** (terminal 1)

   ```bash
   REDIS_URL=redis://127.0.0.1:6379 cargo run --bin matcher
   ```

3. **API** (terminal 2; default binary is the API)

   ```bash
   REDIS_URL=redis://127.0.0.1:6379 MATCHER_HTTP_URL=http://127.0.0.1:4001 cargo run
   ```

4. **Smoke test**

   ```bash
   curl -s http://127.0.0.1:3000/orderbook
   curl -s -X POST http://127.0.0.1:3000/orders \
     -H 'Content-Type: application/json' \
     -d '{"side":"buy","price":100,"qty":1}'
   ```

Environment variables are documented in [`.env.example`](.env.example).

### Multiple API instances

Run two APIs on different ports; both talk to the same Redis and matcher:

```bash
REDIS_URL=redis://127.0.0.1:6379 MATCHER_HTTP_URL=http://127.0.0.1:4001 API_ADDR=0.0.0.0:3000 cargo run
```

```bash
REDIS_URL=redis://127.0.0.1:6379 MATCHER_HTTP_URL=http://127.0.0.1:4001 API_ADDR=0.0.0.0:3001 cargo run
```

Use `curl` / WebSocket clients against `:3000` and `:3001` as needed.

## HTTP API

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/orders` | Body: `{ "side": "buy" \| "sell", "price": number, "qty": number }` → `{ "id": number }` |
| `GET` | `/orderbook` | JSON snapshot: `{ "bids": [...], "asks": [...] }` (aggregated per price) |
| `GET` | `/ws` | WebSocket text messages: each **fill** as JSON |

## Design questions (assignment)

### 1. How does the system handle multiple API server instances without double-matching an order?

**Matching happens only in the matcher process.** API servers never hold the authoritative book; they only **RPUSH** orders to Redis and **BRPOP** a reply list for the assigned id. A **single consumer** (the matcher) **BRPOP**s the queue, so each order is processed once. Fills are broadcast via **Redis pub/sub** so every API instance can forward events to its own WebSocket clients without performing matching.

### 2. What data structure did you use for the order book and why?

**Bids:** `BTreeMap<Reverse<price>, VecDeque<Order>>` — iterate best bid first; **FIFO** at each price level.  
**Asks:** `BTreeMap<price, VecDeque<Order>>` — best ask first; **FIFO** per level.

`BTreeMap` keeps price levels sorted for matching; `VecDeque` gives **time priority** within a level in **O(1)** at the front of the queue.

### 3. What breaks first if this were under real production load?

Roughly in order: **single matcher throughput** (one thread of matching + Redis I/O), **Redis** as queue + pub/sub bottleneck, **no persistence** (restart loses book), **head-of-line blocking** on `BRPOP` / client timeouts, and **WebSocket fanout** (broadcast channel + many slow clients). No autoscaling or backpressure story.

### 4. What would you build next if you had another 4 hours?

**Append-only log + snapshot** for recovery, **metrics** (latency, queue depth, match rate), **stricter API validation** and request IDs, **integration tests** against Redis, and a **clearer backpressure** story (rate limits or drop policy for pub/sub).

## Development

```bash
cargo test
cargo clippy --all-targets
```
