// src/ws_server.rs
use axum::{
    extract::{State, ws::{WebSocket, WebSocketUpgrade, Message}},
    response::IntoResponse,
    routing::get,
    Router,
};
use futures::{StreamExt, SinkExt};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{RwLock, broadcast};

use crate::{info_log, warn_log};

#[derive(Debug, Clone, Serialize)]
pub struct ServerSnapshot {
    pub r#type: &'static str,
    pub rng_type: String,
    pub ore_snapshot: PoVAISnapShot,
    pub orb_snapshot: PoVAISnapShot
}

#[derive(Debug, Clone, Serialize)]
pub struct PoVAISnapShot {
    pub type_rng: usize, 
    pub status: String,
    pub round: usize,
    pub preds: Vec<usize>,
    pub total_round: usize,
    pub total_win: usize,
    pub win: usize,
    pub lose: usize,
    pub win_in_row: usize,
    pub lose_in_row: usize,
    pub winning_square: usize
}

#[derive(Debug, Clone, Serialize)]
pub struct InRowRound {
    pub r#type: &'static str,
    pub list_in_row: HashMap<u32, u32>
}

impl PoVAISnapShot {
    pub fn new() -> Self {
        Self {
            type_rng: 1,
            status: "init".to_string(),
            round: 0,
            preds: Vec::new(),
            total_round: 1,
            total_win: 0,
            win: 0,
            lose: 0,
            win_in_row: 0,
            lose_in_row: 0,
            winning_square: 0
        }
    }
    pub fn init(type_rng: usize) -> Self {
        Self {
            type_rng: type_rng,
            status: "init".to_string(),
            round: 0,
            preds: Vec::new(),
            total_round: 1,
            total_win: 0,
            win: 0,
            lose: 0,
            win_in_row: 0,
            lose_in_row: 0,
            winning_square: 0
        }
    }
}

#[derive(Clone)]
pub struct WsServerHandle {
    pub tx: broadcast::Sender<String>,
    pub snapshot: Arc<RwLock<Option<ServerSnapshot>>>,
}

pub fn build_router(handle: WsServerHandle) -> Router {
    Router::new()
        .route("/ws", get(ws_upgrade))
        .with_state(Arc::new(handle))
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(handle): State<Arc<WsServerHandle>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, handle))
}

async fn handle_socket(socket: WebSocket, handle: Arc<WsServerHandle>) {
    info_log!("WS client connected");
    // split sink & stream
    let (mut sender, mut receiver) = socket.split();

    if let Some(snap) = handle.snapshot.read().await.clone() {
        if let Ok(json) = serde_json::to_string(&snap) {
            // ignore send error (client may have closed)
            let _ = sender.send(Message::Text(json.into())).await;
        }
    }

    // subscribe to broadcast channel
    let mut rx = handle.tx.subscribe();

    // task: forward broadcast -> client
    let send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(json) => {
                    // convert String -> Utf8Bytes via into()
                    if sender.send(Message::Text(json.into())).await.is_err() {
                        // client disconnected
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn_log!("client lagged {} messages", n);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    // task: read incoming messages from client (optional)
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    info_log!("from client: {}", text);
                    // optionally parse commands
                }
                Message::Close(_) => {
                    info_log!("client closed");
                    break;
                }
                _ => {}
            }
        }
    });

    // wait until one finishes
    let _ = tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    };

    info_log!("WS client disconnected");
}
