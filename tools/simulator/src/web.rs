use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use tokio::sync::broadcast;

/// Shared state for the web server
#[derive(Clone)]
pub struct WebState {
    pub tx: broadcast::Sender<String>,
}

/// Start the web server on the given port
pub async fn run_web_server(port: u16, state: WebState) {
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        // Prometheus scrape endpoint for the simulator's own counters
        // (recommendation #7). Pair with the engine's `/metrics/all` in
        // Grafana to compare attempted-vs-accepted on the same panel —
        // e.g. spammers attempt 50 RPS, engine accepts 30 RPS,
        // dashboard shows the 20 RPS gap as `pms_simulator_tx_failed_total`.
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Web dashboard at http://localhost:{}", port);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind web server");
    axum::serve(listener, app).await.ok();
}

async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn metrics_handler() -> impl axum::response::IntoResponse {
    let body = crate::sim_metrics::render();
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<WebState>,
) -> axum::response::Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: WebState) {
    let mut rx = state.tx.subscribe();
    let (mut sender, mut _receiver) = socket.split();

    loop {
        match rx.recv().await {
            Ok(msg) => {
                if sender.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("WebSocket client lagged, skipped {} messages", n);
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="fr">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>pms-simulator — Agent Messages</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    background: #0a0a0a;
    color: #e0e0e0;
    font-family: 'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace;
    font-size: 14px;
    line-height: 1.5;
    padding: 20px;
  }
  #header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 16px;
    margin-bottom: 16px;
    border-bottom: 1px solid #222;
  }
  #header h1 {
    font-size: 16px;
    font-weight: 600;
    color: #fff;
  }
  #status {
    font-size: 12px;
    padding: 2px 8px;
    border-radius: 4px;
    background: #1a1a1a;
  }
  #status.connected { color: #4ade80; border: 1px solid #166534; }
  #status.disconnected { color: #f87171; border: 1px solid #7f1d1d; }
  #counter {
    font-size: 12px;
    color: #666;
    margin-left: auto;
  }
  #messages {
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-y: auto;
    max-height: calc(100vh - 100px);
    padding-bottom: 20px;
  }
  .msg {
    padding: 4px 12px;
    border-radius: 3px;
    animation: fadeIn 0.3s ease;
  }
  .msg:hover { background: #1a1a1a; }
  .msg .time {
    color: #555;
    font-size: 12px;
    margin-right: 8px;
  }
  .msg .agent {
    font-weight: 600;
    margin-right: 4px;
  }
  .msg.text .agent { color: #60a5fa; }
  .msg.tx_notification .agent { color: #4ade80; }
  .msg.tx_notification .content { color: #86efac; }
  .msg.info .agent { color: #a78bfa; }
  .msg.error {
    background: rgba(127, 29, 29, 0.2);
    border-left: 3px solid #ef4444;
  }
  .msg.error .agent { color: #f87171; }
  .msg.error .content { color: #fca5a5; }
  .msg.error .explanation {
    display: block;
    margin-top: 4px;
    padding: 6px 10px;
    background: rgba(127, 29, 29, 0.15);
    border-radius: 3px;
    color: #fca5a5;
    font-size: 13px;
    font-style: italic;
  }
  @keyframes fadeIn {
    from { opacity: 0; transform: translateY(4px); }
    to { opacity: 1; transform: translateY(0); }
  }
</style>
</head>
<body>
  <div id="header">
    <h1>pms-simulator</h1>
    <span id="status" class="disconnected">disconnected</span>
    <span id="counter">0 messages</span>
  </div>
  <div id="messages"></div>

<script>
const messagesDiv = document.getElementById('messages');
const statusEl = document.getElementById('status');
const counterEl = document.getElementById('counter');
let msgCount = 0;
let ws;

function connect() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${proto}://${location.host}/ws`);

  ws.onopen = () => {
    statusEl.textContent = 'connected';
    statusEl.className = 'connected';
  };

  ws.onclose = () => {
    statusEl.textContent = 'disconnected';
    statusEl.className = 'disconnected';
    setTimeout(connect, 2000);
  };

  ws.onerror = () => ws.close();

  ws.onmessage = (event) => {
    try {
      const msg = JSON.parse(event.data);
      appendMessage(msg);
    } catch(e) {
      console.error('Parse error:', e);
    }
  };
}

function timeStr() {
  const d = new Date();
  return d.toLocaleTimeString('fr-FR', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function appendMessage(msg) {
  msgCount++;
  counterEl.textContent = `${msgCount} messages`;

  const el = document.createElement('div');
  el.classList.add('msg', msg.type);

  const time = `<span class="time">${timeStr()}</span>`;

  switch(msg.type) {
    case 'text':
      el.innerHTML = `${time}<span class="agent">[${msg.from}]</span> <span class="content">${esc(msg.content)}</span>`;
      break;
    case 'tx_notification':
      const shortId = msg.block_id && msg.block_id.length > 12 ? msg.block_id.slice(0, 12) + '...' : msg.block_id;
      el.innerHTML = `${time}<span class="agent">[${msg.from} → ${msg.to}]</span> <span class="content">${esc(msg.amount)} PMS (block ${shortId})</span>`;
      break;
    case 'info':
      el.innerHTML = `${time}<span class="agent">[${msg.from}]</span> <span class="content">${esc(JSON.stringify(msg.data))}</span>`;
      break;
    case 'error':
      el.innerHTML = `${time}<span class="agent">[${msg.from}] ERROR:</span> <span class="content">${esc(msg.error)}</span><span class="explanation">${esc(msg.explanation)}</span>`;
      break;
    default:
      el.innerHTML = `${time}<span class="content">${esc(JSON.stringify(msg))}</span>`;
  }

  messagesDiv.appendChild(el);

  // Keep max 500 messages in DOM
  while (messagesDiv.children.length > 500) {
    messagesDiv.removeChild(messagesDiv.firstChild);
  }

  // Auto-scroll
  messagesDiv.scrollTop = messagesDiv.scrollHeight;
}

function esc(s) {
  if (!s) return '';
  const d = document.createElement('div');
  d.textContent = String(s);
  return d.innerHTML;
}

connect();
</script>
</body>
</html>
"#;
