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

mod consts;

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
    let (tx, _) = broadcast::channel(100);

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

            if username == consts::GLOBAL_CHAT_NAME {
                let _ = sender
                    .send(Message::Text(String::from("This word is reserved.")))
                    .await;

                return;
            } else if username.is_empty() {
                let _ = sender
                    .send(Message::Text(String::from("Username already taken.")))
                    .await;

                return;
            } else {
                break;
            }
        }
    }

    let (tx, mut rx) = broadcast::channel(100);
    state
        .user_set
        .lock()
        .unwrap()
        .insert(username.clone(), tx.clone());

    let username_clone = username.clone();

    let msg = format!("{username_clone} joined.");
    tracing::debug!("{msg}");
    let _ = state.tx.send(msg);
    let common_state = state.clone();
    let mut send_task = tokio::spawn(async move {
        let mut common_rx = common_state.tx.subscribe();

        loop {
            let msg = tokio::select! {
                msg = rx.recv() => msg,
                msg = common_rx.recv() => msg,
            };

            if sender.send(Message::Text(msg.unwrap())).await.is_err() {
                break;
            }
        }
    });

    let global_state = state.clone();
    let user_set_state = state.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = receiver.next().await {
            let (recipient, message) = parse_ws_message(text);

            if recipient == consts::GLOBAL_CHAT_NAME {
                let global = consts::GLOBAL_CHAT_NAME;
                let _ = global_state
                    .tx
                    .send(format!("{global}{username}: {message}"));
            } else if let Some(recipient_tx) = user_set_state
                .user_set
                .lock()
                .unwrap()
                .get(recipient.as_str())
                .cloned()
            {
                let recipient_message = format!("{username}: {message}");
                let _ = recipient_tx.send(recipient_message.clone());
                let _ = tx.send(recipient_message);
            } else {
                let _ = tx.send(format!(
                    "Recipient with username \"{recipient}\" not found."
                ));
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    let msg = format!("{username_clone} left.");
    tracing::debug!("{msg}");
    let _ = state.tx.send(msg);

    state.user_set.lock().unwrap().remove(&username_clone);
}

fn parse_ws_message(ws_message: String) -> (String, String) {
    let mut recipient = String::from(consts::GLOBAL_CHAT_NAME);
    let mut message = ws_message.clone().to_string();

    if ws_message.contains(':') {
        let mut parts = ws_message.splitn(2, ':');
        recipient = parts.next().unwrap().trim().to_string();
        message = parts.next().unwrap().trim().to_string();
    }

    (recipient, message)
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
