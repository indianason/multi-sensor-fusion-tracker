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

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8" />
<title>Sentinel</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600&family=JetBrains+Mono:wght@400;500&family=Space+Grotesk:wght@500;600&display=swap" rel="stylesheet">
<style>
  :root {
    --bg: #0a0e14;
    --panel: #10151d;
    --border: #1f2733;
    --text-primary: #e6e9ee;
    --text-secondary: #6b7684;
    --confirmed: #34d399;
    --tentative: #fbbf24;
    --coasting: #f97316;
    --dropped: #6b7684;
  }
  * { box-sizing: border-box; }
  html, body {
    margin: 0;
    height: 100%;
    background: var(--bg);
    color: var(--text-primary);
    font-family: 'Inter', sans-serif;
    overflow: hidden;
  }
  #app { display: flex; flex-direction: column; height: 100vh; }
  #topbar {
    height: 48px;
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
    background: var(--panel);
  }
  #wordmark {
    font-family: 'Space Grotesk', sans-serif;
    font-weight: 600;
    font-size: 15px;
    letter-spacing: 2px;
  }
  #topbar-stats { display: flex; gap: 18px; font-size: 12px; color: var(--text-secondary); }
  #topbar-stats .stat { display: flex; align-items: center; gap: 6px; }
  #topbar-stats .dot { width: 6px; height: 6px; border-radius: 50%; }
  .dot.confirmed { background: var(--confirmed); }
  .dot.tentative { background: var(--tentative); }
  .dot.coasting { background: var(--coasting); }
  #topbar-right {
    display: flex;
    align-items: center;
    gap: 16px;
    font-family: 'JetBrains Mono', monospace;
    font-size: 12px;
    color: var(--text-secondary);
  }
  #conn { display: flex; align-items: center; gap: 6px; }
  #conn-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--confirmed); }
  #conn-dot.down { background: #f04438; }
  #main { flex: 1; display: flex; overflow: hidden; min-height: 0; }
  #canvas-wrap { flex: 1; position: relative; min-width: 0; }
  canvas { display: block; }
  #sidebar {
    width: 300px;
    flex: 0 0 300px;
    border-left: 1px solid var(--border);
    background: var(--panel);
    overflow-y: auto;
  }
  #sidebar-header {
    padding: 12px 16px;
    font-size: 11px;
    letter-spacing: 1px;
    color: var(--text-secondary);
    border-bottom: 1px solid var(--border);
    display: flex;
    justify-content: space-between;
    position: sticky;
    top: 0;
    background: var(--panel);
  }
  .track-row {
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
  }
  .track-row .badge {
    display: inline-block;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.5px;
    padding: 2px 6px;
    border-radius: 3px;
    margin-bottom: 6px;
  }
  .badge.confirmed { color: var(--confirmed); background: rgba(52,211,153,0.12); }
  .badge.tentative { color: var(--tentative); background: rgba(251,191,36,0.12); }
  .badge.coasting { color: var(--coasting); background: rgba(249,115,22,0.12); }
  .badge.dropped { color: var(--dropped); background: rgba(107,118,132,0.12); }
  .track-row .id {
    font-family: 'JetBrains Mono', monospace;
    font-size: 11px;
    color: var(--text-secondary);
    margin-left: 6px;
  }
  .track-row .data {
    font-family: 'JetBrains Mono', monospace;
    font-size: 11px;
    color: var(--text-secondary);
    line-height: 1.6;
  }
  .track-row .data .val { color: var(--text-primary); }
  .track-row .sensors { margin-top: 4px; }
  .track-row .sensors .tag {
    display: inline-block;
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 1px 5px;
    margin-right: 4px;
    font-size: 10px;
    color: var(--text-secondary);
  }
  .empty-state {
    padding: 24px 16px;
    color: var(--text-secondary);
    font-size: 12px;
    text-align: center;
  }
</style>
</head>
<body>
<div id="app">
  <header id="topbar">
    <div id="wordmark">SENTINEL</div>
    <div id="topbar-stats">
      <span class="stat"><span class="dot confirmed"></span><span id="count-confirmed">0</span> CONFIRMED</span>
      <span class="stat"><span class="dot tentative"></span><span id="count-tentative">0</span> TENTATIVE</span>
      <span class="stat"><span class="dot coasting"></span><span id="count-coasting">0</span> COASTING</span>
    </div>
    <div id="topbar-right">
      <span id="uptime">00:00</span>
      <span id="conn"><span id="conn-dot"></span><span id="conn-text">CONNECTING</span></span>
    </div>
  </header>
  <main id="main">
    <div id="canvas-wrap">
      <canvas id="c"></canvas>
    </div>
    <aside id="sidebar">
      <div id="sidebar-header">TRACKS <span id="sidebar-count">0</span></div>
      <div id="track-list"></div>
    </aside>
  </main>
</div>
<script>
const canvas = document.getElementById('c');
const ctx = canvas.getContext('2d');
let latestTracks = [];
let connected = false;
const startTime = Date.now();

function worldToScreen(x, y) {
  const scale = Math.min(canvas.width, canvas.height) / 1000;
  return [canvas.width / 2 + x * scale, canvas.height / 2 + y * scale];
}

function statusColor(status) {
  if (status === 'Confirmed') return '#34d399';
  if (status === 'Tentative') return '#fbbf24';
  if (status === 'Coasting') return '#f97316';
  return '#6b7684';
}

function drawGrid() {
  const step = 40;
  ctx.strokeStyle = 'rgba(255,255,255,0.035)';
  ctx.lineWidth = 1;
  for (let x = 0; x < canvas.width; x += step) {
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, canvas.height);
    ctx.stroke();
  }
  for (let y = 0; y < canvas.height; y += step) {
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(canvas.width, y);
    ctx.stroke();
  }
  const [cx, cy] = worldToScreen(0, 0);
  ctx.strokeStyle = 'rgba(255,255,255,0.08)';
  ctx.beginPath();
  ctx.moveTo(cx, 0);
  ctx.lineTo(cx, canvas.height);
  ctx.stroke();
  ctx.beginPath();
  ctx.moveTo(0, cy);
  ctx.lineTo(canvas.width, cy);
  ctx.stroke();
}

function drawTrack(track) {
  const color = statusColor(track.status);
  const [x, y] = worldToScreen(track.position.x, track.position.y);

  const hist = track.history;
  ctx.lineWidth = 1.25;
  for (let i = 1; i < hist.length; i++) {
    const [x0, y0] = worldToScreen(hist[i - 1].x, hist[i - 1].y);
    const [x1, y1] = worldToScreen(hist[i].x, hist[i].y);
    ctx.beginPath();
    ctx.strokeStyle = color;
    ctx.globalAlpha = 0.15 + (i / hist.length) * 0.35;
    ctx.moveTo(x0, y0);
    ctx.lineTo(x1, y1);
    ctx.stroke();
  }
  ctx.globalAlpha = 1;

  const vx = track.velocity[0];
  const vy = track.velocity[1];
  const speed = Math.hypot(vx, vy);
  const angle = speed > 0.5 ? Math.atan2(vy, vx) : 0;

  ctx.save();
  ctx.translate(x, y);
  ctx.rotate(angle);
  ctx.beginPath();
  ctx.moveTo(8, 0);
  ctx.lineTo(-5, 4.5);
  ctx.lineTo(-5, -4.5);
  ctx.closePath();
  ctx.fillStyle = color;
  ctx.fill();
  ctx.restore();

  ctx.font = '11px "JetBrains Mono", monospace';
  ctx.fillStyle = 'rgba(230,233,238,0.55)';
  ctx.fillText(track.id.slice(0, 8).toUpperCase(), x + 10, y - 8);
}

function resizeCanvas() {
  const wrap = document.getElementById('canvas-wrap');
  canvas.width = wrap.clientWidth;
  canvas.height = wrap.clientHeight;
}

function renderScene() {
  resizeCanvas();
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  drawGrid();
  latestTracks.forEach(drawTrack);
}

function renderSidebarAndStats() {
  const confirmed = latestTracks.filter((t) => t.status === 'Confirmed').length;
  const tentative = latestTracks.filter((t) => t.status === 'Tentative').length;
  const coasting = latestTracks.filter((t) => t.status === 'Coasting').length;

  document.getElementById('count-confirmed').textContent = confirmed;
  document.getElementById('count-tentative').textContent = tentative;
  document.getElementById('count-coasting').textContent = coasting;
  document.getElementById('sidebar-count').textContent = latestTracks.length;

  const list = document.getElementById('track-list');
  if (latestTracks.length === 0) {
    list.innerHTML = '<div class="empty-state">NO ACTIVE TRACKS</div>';
    return;
  }
  list.innerHTML = latestTracks
    .map((t) => {
      const statusClass = t.status.toLowerCase();
      const sensors = t.contributing_sensors
        .map((s) => '<span class="tag">' + s.toUpperCase() + '</span>')
        .join('');
      return (
        '<div class="track-row">' +
        '<span class="badge ' + statusClass + '">' + t.status.toUpperCase() + '</span>' +
        '<span class="id">' + t.id.slice(0, 8).toUpperCase() + '</span>' +
        '<div class="data">' +
        'POS <span class="val">' + t.position.x.toFixed(1) + ', ' + t.position.y.toFixed(1) + '</span><br>' +
        'VEL <span class="val">' + t.velocity[0].toFixed(1) + ', ' + t.velocity[1].toFixed(1) + '</span><br>' +
        'CONF <span class="val">' + (t.confidence * 100).toFixed(0) + '%</span>' +
        '</div>' +
        '<div class="sensors">' + sensors + '</div>' +
        '</div>'
      );
    })
    .join('');
}

function onDataUpdate() {
  renderScene();
  renderSidebarAndStats();
}

function updateConnUi() {
  const dot = document.getElementById('conn-dot');
  const text = document.getElementById('conn-text');
  dot.classList.toggle('down', !connected);
  text.textContent = connected ? 'LIVE' : 'OFFLINE';
}

function updateUptime() {
  const elapsed = Math.floor((Date.now() - startTime) / 1000);
  const mm = String(Math.floor(elapsed / 60)).padStart(2, '0');
  const ss = String(elapsed % 60).padStart(2, '0');
  document.getElementById('uptime').textContent = mm + ':' + ss;
}
setInterval(updateUptime, 1000);

window.addEventListener('resize', renderScene);

function connect() {
  const ws = new WebSocket('ws://' + location.host + '/ws');
  ws.onopen = () => {
    connected = true;
    updateConnUi();
  };
  ws.onclose = () => {
    connected = false;
    updateConnUi();
    setTimeout(connect, 1000);
  };
  ws.onerror = () => ws.close();
  ws.onmessage = (event) => {
    latestTracks = JSON.parse(event.data);
    onDataUpdate();
  };
}
connect();
</script>
</body>
</html>
"##;