use crate::types::Track;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use std::net::SocketAddr;
use tokio::sync::broadcast;
use tracing::info;

#[derive(Clone)]
struct AppState {
    snapshot_tx: broadcast::Sender<Vec<Track>>,
}

/// Serve the live dashboard: a single HTML page over HTTP plus a
/// WebSocket endpoint that streams track snapshots as they're broadcast
/// from the fusion loop in main.rs.
pub async fn run_dashboard_server(snapshot_tx: broadcast::Sender<Vec<Track>>, addr: SocketAddr) {
    let state = AppState { snapshot_tx };
    let app = Router::new()
        .route("/", get(serve_dashboard))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind dashboard address");
    info!("Dashboard listening on http://{}", addr);
    axum::serve(listener, app)
        .await
        .expect("dashboard server crashed");
}

async fn serve_dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    let rx = state.snapshot_tx.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, rx))
}

async fn handle_socket(mut socket: WebSocket, mut rx: broadcast::Receiver<Vec<Track>>) {
    loop {
        match rx.recv().await {
            Ok(tracks) => {
                let payload = match serde_json::to_string(&tracks) {
                    Ok(json) => json,
                    Err(_) => continue,
                };
                if socket.send(Message::Text(payload.into())).await.is_err() {
                    // Client disconnected (closed tab, network drop, etc).
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // A slow client skips ahead to the latest snapshot rather
                // than the connection dying or backing up the channel —
                // "current state" matters more than "every frame" here.
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8" />
<title>Sentinel Dashboard</title>
<style>
  body { background: #0b0f14; color: #c9d1d9; font-family: monospace; margin: 0; }
  #status { position: absolute; top: 8px; left: 8px; font-size: 12px; }
  canvas { display: block; background: #0b0f14; }
</style>
</head>
<body>
<div id="status">connecting...</div>
<canvas id="c" width="900" height="700"></canvas>
<script>
const canvas = document.getElementById('c');
const ctx = canvas.getContext('2d');
const status = document.getElementById('status');

// World coordinates roughly span [-500, 500] on both axes; map to canvas.
const scale = 0.7;
const originX = canvas.width / 2;
const originY = canvas.height / 2;

function worldToScreen(x, y) {
  return [originX + x * scale, originY + y * scale];
}

const statusColors = {
  Tentative: '#f1c40f',
  Confirmed: '#2ecc71',
  Coasting: '#e67e22',
  Dropped: '#7f8c8d',
};

function draw(tracks) {
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  for (const track of tracks) {
    const color = statusColors[track.status] || '#ffffff';

    // Trail from history
    ctx.beginPath();
    ctx.strokeStyle = color;
    ctx.globalAlpha = 0.4;
    ctx.lineWidth = 1;
    track.history.forEach((p, i) => {
      const [sx, sy] = worldToScreen(p.x, p.y);
      if (i === 0) ctx.moveTo(sx, sy);
      else ctx.lineTo(sx, sy);
    });
    ctx.stroke();
    ctx.globalAlpha = 1;

    // Current position
    const [sx, sy] = worldToScreen(track.position.x, track.position.y);
    ctx.beginPath();
    ctx.fillStyle = color;
    ctx.arc(sx, sy, 5, 0, Math.PI * 2);
    ctx.fill();

    // Label
    ctx.fillStyle = '#c9d1d9';
    ctx.font = '10px monospace';
    ctx.fillText(`${track.status} ${track.id.slice(0, 8)}`, sx + 8, sy - 8);
  }
}

function connect() {
  const ws = new WebSocket(`ws://${location.host}/ws`);
  ws.onopen = () => { status.textContent = 'connected'; };
  ws.onclose = () => {
    status.textContent = 'disconnected, retrying...';
    setTimeout(connect, 1000);
  };
  ws.onerror = () => ws.close();
  ws.onmessage = (event) => {
    const tracks = JSON.parse(event.data);
    status.textContent = `tracks: ${tracks.length}`;
    draw(tracks);
  };
}
connect();
</script>
</body>
</html>
"#;