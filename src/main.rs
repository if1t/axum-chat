use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures::{sink::SinkExt, stream::StreamExt};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

struct AppState {
    user_set: Mutex<HashMap<String, broadcast::Sender<String>>>,
    tx: broadcast::Sender<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("{}=trace", env!("CARGO_CRATE_NAME")).into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let user_set = Mutex::new(HashMap::new());
    let (tx, _rx) = broadcast::channel(100);

    let app_state = Arc::new(AppState { user_set, tx });

    let app = Router::new()
        .route("/", get(index))
        .route("/websocket", get(websocket_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| websocket(socket, state))
}

async fn websocket(stream: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = stream.split();

    let mut username = String::new();
    while let Some(Ok(message)) = receiver.next().await {
        if let Message::Text(name) = message {
            check_username(&state, &mut username, &name);

            if !username.is_empty() {
                break;
            } else {
                let _ = sender
                    .send(Message::Text(String::from("Username already taken.")))
                    .await;

                return;
            }
        }
    }

    let username_clone = username.clone(); // Clone username here

    let (tx, mut rx) = broadcast::channel(100);
    state
        .user_set
        .lock()
        .unwrap()
        .insert(username.clone(), tx.clone());

    let msg = format!("{username_clone} joined.");
    tracing::debug!("{msg}");
    let _ = state.tx.send(msg); // Send join message to general chat
    let state_clone = state.clone(); // Clone state here
    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = receiver.next().await {
            let mut parts = text.splitn(2, ':');
            let recipient = parts.next().unwrap().trim();
            let message = parts.next().unwrap().trim();

            // Send message to recipient
            if let Some(recipient_tx) = state_clone.user_set.lock().unwrap().get(recipient).cloned()
            {
                let _ = recipient_tx.send(format!("{username}: {message}"));
            } else {
                // Send to general chat
                let _ = state_clone.tx.send(format!("{username}: {message}"));
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    let msg = format!("{username_clone} left.");
    tracing::debug!("{msg}");
    let _ = state.tx.send(msg); // Send leave message to general chat

    state.user_set.lock().unwrap().remove(&username_clone);
}

fn check_username(state: &AppState, string: &mut String, name: &str) {
    let mut user_set = state.user_set.lock().unwrap();

    if !user_set.contains_key(name) {
        user_set.insert(name.to_owned(), broadcast::channel(100).0);

        string.push_str(name);
    }
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../chat.html"))
}
