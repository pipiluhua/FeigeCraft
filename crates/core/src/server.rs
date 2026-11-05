//! 接收端：基于 HTTPS + LocalSend v2 REST API 的文件接收服务。
//!
//! 端点：
//! - `GET  /api/localsend/v2/info`            返回本机设备信息
//! - `POST /api/localsend/v2/register`        对方注册
//! - `POST /api/localsend/v2/prepare-upload`  协商会话，返回会话 ID 与每个文件的 token
//! - `POST /api/localsend/v2/upload`          上传单个文件（流式落盘）
//! - `POST /api/localsend/v2/cancel`          取消会话
//!
//! FeigeCraft 扩展端点：
//! - `POST /api/feigecraft/chat`               接收局域网聊天消息

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

use crate::chat::{peer_key, ChatEnvelope, ChatMessage};
use crate::events::AppEvent;
use crate::model::{
    DeviceInfo, Direction, FileMetadata, PrepareUploadRequest, PrepareUploadResponse,
    RegisterResponse,
};
use crate::state::SharedState;
use crate::util;

/// 等待用户点击「接受」的最长时间。
const DECISION_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Receiver {
    state: Arc<SharedState>,
    sessions: Mutex<HashMap<String, Session>>,
    pending: Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    port: u16,
}

struct Session {
    sender: DeviceInfo,
    files: HashMap<String, SessionFile>,
    remaining: usize,
    received_bytes: u64,
    cancelled: Arc<AtomicBool>,
}

struct SessionFile {
    meta: FileMetadata,
    token: String,
    finished: bool,
}

impl Receiver {
    /// 绑定端口（被占用时自动顺延）并启动 HTTPS 服务。
    pub async fn bind(state: Arc<SharedState>) -> Result<Arc<Self>> {
        state.config().ensure_download_dir().ok();

        let preferred = state.config().port;
        let mut listener = None;
        let mut chosen = preferred;
        let mut last_error = None;
        for offset in 0..20u16 {
            let port = preferred.saturating_add(offset);
            match TcpListener::bind(("0.0.0.0", port)).await {
                Ok(bound) => {
                    listener = Some(bound);
                    chosen = port;
                    last_error = None;
                    break;
                }
                Err(err) => {
                    last_error = Some(err);
                }
            }
        }
        let listener = match listener {
            Some(listener) => listener,
            None => {
                return Err(anyhow!(
                    "无法监听端口 {preferred}~{}: {}",
                    preferred.saturating_add(19),
                    last_error.map(|e| e.to_string()).unwrap_or_default()
                ))
            }
        };

        if chosen != preferred {
            state.set_port(chosen);
            state.emit(AppEvent::log_warn(format!(
                "端口 {preferred} 已被占用，改用 {chosen}"
            )));
        }

        let tls_config = state.identity.server_config()?;
        let acceptor = TlsAcceptor::from(tls_config);
        let receiver = Arc::new(Self {
            state: state.clone(),
            sessions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            port: chosen,
        });

        let app = receiver.clone().router();
        tokio::spawn(async move { accept_loop(listener, acceptor, app).await });
        Ok(receiver)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 用户对某个接收请求做出裁决。
    pub fn resolve(&self, session_id: &str, accept: bool) -> bool {
        if let Some(sender) = self
            .pending
            .lock()
            .expect("pending mutex poisoned")
            .remove(session_id)
        {
            sender.send(accept).is_ok()
        } else {
            false
        }
    }

    /// 本地主动取消某个接收会话（GUI 的「取消」按钮）。
    pub fn cancel_session(&self, session_id: &str) -> bool {
        if let Some(sender) = self
            .pending
            .lock()
            .expect("pending mutex poisoned")
            .remove(session_id)
        {
            let _ = sender.send(false);
        }
        match self
            .sessions
            .lock()
            .expect("sessions mutex poisoned")
            .remove(session_id)
        {
            Some(session) => {
                session.cancelled.store(true, Ordering::Relaxed);
                self.state.emit(AppEvent::SessionFailed {
                    session_id: session_id.to_string(),
                    direction: Direction::Receive,
                    error: "已取消".to_string(),
                });
                true
            }
            None => false,
        }
    }

    fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/api/localsend/v2/info", get(info_handler))
            .route("/api/localsend/v2/register", post(register_handler))
            .route("/api/localsend/v2/prepare-upload", post(prepare_handler))
            .route("/api/localsend/v2/upload", post(upload_handler))
            .route("/api/localsend/v2/cancel", post(cancel_handler))
            .route("/api/feigecraft/chat", post(chat_handler))
            .with_state(self)
    }
}

async fn accept_loop(listener: TcpListener, acceptor: TlsAcceptor, app: Router) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let acceptor = acceptor.clone();
                let app = app.clone();
                tokio::spawn(async move {
                    let _ = stream.set_nodelay(true);
                    let tls = match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await {
                        Ok(Ok(tls)) => tls,
                        Ok(Err(err)) => {
                            tracing::debug!("TLS 握手失败 ({peer}): {err}");
                            return;
                        }
                        Err(_) => {
                            tracing::debug!("TLS 握手超时 ({peer})");
                            return;
                        }
                    };
                    let io = TokioIo::new(tls);
                    let service = TowerToHyperService::new(app);
                    if let Err(err) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, service)
                        .await
                    {
                        tracing::debug!("连接结束 ({peer}): {err}");
                    }
                });
            }
            Err(err) => {
                tracing::warn!("接受连接失败: {err}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

async fn info_handler(State(receiver): State<Arc<Receiver>>) -> Response {
    Json(receiver.state.device_info()).into_response()
}

async fn register_handler(
    State(receiver): State<Arc<Receiver>>,
    Json(info): Json<DeviceInfo>,
) -> Response {
    receiver
        .state
        .emit(AppEvent::log_info(format!("{} 已连接", info.alias)));
    // 记录名字，离线时也能在会话列表里显示
    receiver.state.chats.touch_alias(&peer_key(&info), &info.alias);
    let own = receiver.state.device_info();
    Json(RegisterResponse {
        alias: own.alias,
        fingerprint: own.fingerprint,
        device_model: own.device_model,
        device_type: own.device_type,
    })
    .into_response()
}

async fn prepare_handler(
    State(receiver): State<Arc<Receiver>>,
    Json(request): Json<PrepareUploadRequest>,
) -> Response {
    if request.files.is_empty() {
        return (StatusCode::BAD_REQUEST, "文件列表为空").into_response();
    }

    let session_id = Uuid::new_v4().to_string();
    let mut files = HashMap::new();
    let mut ordered: Vec<FileMetadata> = Vec::new();
    let mut total_bytes = 0u64;
    for (id, meta) in request.files.iter() {
        files.insert(
            id.clone(),
            SessionFile { meta: meta.clone(), token: Uuid::new_v4().to_string(), finished: false },
        );
        ordered.push(meta.clone());
        total_bytes = total_bytes.saturating_add(meta.size);
    }
    ordered.sort_by(|a, b| a.file_name.cmp(&b.file_name));

    let file_count = ordered.len();
    let session = Session {
        sender: request.info.clone(),
        files,
        remaining: file_count,
        received_bytes: 0,
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    receiver
        .sessions
        .lock()
        .expect("sessions mutex poisoned")
        .insert(session_id.clone(), session);

    let (sender, decision) = tokio::sync::oneshot::channel();
    receiver
        .pending
        .lock()
        .expect("pending mutex poisoned")
        .insert(session_id.clone(), sender);

    receiver.state.emit(AppEvent::IncomingRequest {
        session_id: session_id.clone(),
        sender: request.info.clone(),
        files: ordered.clone(),
        total_bytes,
    });

    let auto_accept = receiver.state.auto_accept();
    let accepted = if auto_accept {
        true
    } else {
        match tokio::time::timeout(DECISION_TIMEOUT, decision).await {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => false,
            Err(_) => {
                receiver
                    .pending
                    .lock()
                    .expect("pending mutex poisoned")
                    .remove(&session_id);
                receiver
                    .state
                    .emit(AppEvent::log_warn(format!("{} 的请求超时未处理", request.info.alias)));
                false
            }
        }
    };
    receiver
        .pending
        .lock()
        .expect("pending mutex poisoned")
        .remove(&session_id);
    receiver
        .state
        .emit(AppEvent::RequestResolved { session_id: session_id.clone(), accepted });

    if !accepted {
        receiver
            .sessions
            .lock()
            .expect("sessions mutex poisoned")
            .remove(&session_id);
        return (StatusCode::FORBIDDEN, "已拒绝接收").into_response();
    }

    let tokens: HashMap<String, String> = receiver
        .sessions
        .lock()
        .expect("sessions mutex poisoned")
        .get(&session_id)
        .map(|session| {
            session
                .files
                .iter()
                .map(|(id, file)| (id.clone(), file.token.clone()))
                .collect()
        })
        .unwrap_or_default();

    receiver.state.emit(AppEvent::SessionStarted {
        session_id: session_id.clone(),
        direction: Direction::Receive,
        peer: request.info.alias.clone(),
        files: file_count,
        total_bytes,
    });

    Json(PrepareUploadResponse { session_id, files: tokens }).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadQuery {
    session_id: Option<String>,
    file_id: Option<String>,
    token: Option<String>,
}

fn header_value(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .map(|value| value.to_string())
}

async fn upload_handler(
    State(receiver): State<Arc<Receiver>>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let session_id = query
        .session_id
        .or_else(|| header_value(&headers, &["session-id", "sessionid"]))
        .unwrap_or_default();
    let file_id = query
        .file_id
        .or_else(|| header_value(&headers, &["file-id", "fileid"]))
        .unwrap_or_default();
    let token = query
        .token
        .or_else(|| header_value(&headers, &["token"]))
        .unwrap_or_default();

    // 取出需要的信息，尽快释放锁
    let prepared = {
        let guard = receiver.sessions.lock().expect("sessions mutex poisoned");
        match guard.get(&session_id) {
            None => Err((StatusCode::FORBIDDEN, "会话不存在或已结束".to_string())),
            Some(session) => match session.files.get(&file_id) {
                None => Err((StatusCode::NOT_FOUND, "文件 ID 不存在".to_string())),
                Some(file) if file.token != token => {
                    Err((StatusCode::FORBIDDEN, "token 校验失败".to_string()))
                }
                Some(file) if file.finished => {
                    Err((StatusCode::CONFLICT, "该文件已接收".to_string()))
                }
                Some(file) => Ok((file.meta.clone(), session.cancelled.clone())),
            },
        }
    };
    let (meta, cancelled) = match prepared {
        Ok(value) => value,
        Err((status, message)) => return (status, message).into_response(),
    };

    let dir = receiver.state.download_dir();
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        receiver
            .state
            .emit(AppEvent::log_error(format!("创建下载目录失败: {err}")));
        return (StatusCode::INTERNAL_SERVER_ERROR, "无法创建下载目录").into_response();
    }

    let part_path = dir.join(format!(".feigecraft-{}.part", Uuid::new_v4()));
    let mut file = match tokio::fs::File::create(&part_path).await {
        Ok(file) => file,
        Err(err) => {
            receiver
                .state
                .emit(AppEvent::log_error(format!("创建临时文件失败: {err}")));
            return (StatusCode::INTERNAL_SERVER_ERROR, "无法写入下载目录").into_response();
        }
    };

    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    let mut last_emit = Instant::now();
    let mut failure: Option<String> = None;

    while let Some(chunk) = stream.next().await {
        if cancelled.load(Ordering::Relaxed) {
            failure = Some("传输已被取消".to_string());
            break;
        }
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                failure = Some(format!("读取数据失败: {err}"));
                break;
            }
        };
        if let Err(err) = file.write_all(&chunk).await {
            failure = Some(format!("写入磁盘失败: {err}"));
            break;
        }
        written += chunk.len() as u64;
        if last_emit.elapsed() >= Duration::from_millis(100) {
            last_emit = Instant::now();
            receiver.state.emit(AppEvent::FileProgress {
                session_id: session_id.clone(),
                file_name: meta.file_name.clone(),
                transferred: written,
                total: meta.size.max(written),
                direction: Direction::Receive,
            });
        }
    }

    if let Some(reason) = failure {
        let _ = file.flush().await;
        drop(file);
        let _ = tokio::fs::remove_file(&part_path).await;
        receiver.sessions.lock().expect("sessions mutex poisoned").remove(&session_id);
        receiver.state.emit(AppEvent::SessionFailed {
            session_id,
            direction: Direction::Receive,
            error: reason,
        });
        return (StatusCode::INTERNAL_SERVER_ERROR, "传输失败").into_response();
    }

    if let Err(err) = file.flush().await {
        let _ = tokio::fs::remove_file(&part_path).await;
        receiver
            .state
            .emit(AppEvent::log_error(format!("落盘失败: {err}")));
        return (StatusCode::INTERNAL_SERVER_ERROR, "落盘失败").into_response();
    }
    drop(file);

    let final_path = util::unique_path(&dir, &meta.file_name);
    if let Err(err) = tokio::fs::rename(&part_path, &final_path).await {
        let _ = tokio::fs::remove_file(&part_path).await;
        receiver
            .state
            .emit(AppEvent::log_error(format!("重命名文件失败: {err}")));
        return (StatusCode::INTERNAL_SERVER_ERROR, "保存文件失败").into_response();
    }

    let finished_all = {
        let mut guard = receiver.sessions.lock().expect("sessions mutex poisoned");
        if let Some(session) = guard.get_mut(&session_id) {
            if let Some(entry) = session.files.get_mut(&file_id) {
                entry.finished = true;
            }
            session.remaining = session.remaining.saturating_sub(1);
            session.received_bytes = session.received_bytes.saturating_add(written);
            let done = session.remaining == 0;
            let (files, bytes, peer) = (
                session.files.len(),
                session.received_bytes,
                session.sender.alias.clone(),
            );
            if done {
                guard.remove(&session_id);
                Some((files, bytes, peer))
            } else {
                None
            }
        } else {
            None
        }
    };

    receiver.state.emit(AppEvent::FileProgress {
        session_id: session_id.clone(),
        file_name: meta.file_name.clone(),
        transferred: written,
        total: meta.size.max(written),
        direction: Direction::Receive,
    });
    receiver.state.emit(AppEvent::FileFinished {
        session_id: session_id.clone(),
        file_name: meta.file_name.clone(),
        path: Some(final_path),
        size: written,
        direction: Direction::Receive,
    });

    if let Some((files, bytes, peer)) = finished_all {
        receiver.state.emit(AppEvent::SessionFinished {
            session_id,
            direction: Direction::Receive,
            files,
            bytes,
            dir: Some(dir.clone()),
        });
        receiver
            .state
            .emit(AppEvent::log_info(format!("已接收来自 {peer} 的 {files} 个文件")));
    }

    StatusCode::OK.into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelQuery {
    session_id: Option<String>,
}

async fn cancel_handler(
    State(receiver): State<Arc<Receiver>>,
    Query(query): Query<CancelQuery>,
    headers: HeaderMap,
) -> Response {
    let session_id = query
        .session_id
        .or_else(|| header_value(&headers, &["session-id", "sessionid"]))
        .unwrap_or_default();
    if let Some(sender) = receiver
        .pending
        .lock()
        .expect("pending mutex poisoned")
        .remove(&session_id)
    {
        let _ = sender.send(false);
    }
    if let Some(session) = receiver
        .sessions
        .lock()
        .expect("sessions mutex poisoned")
        .remove(&session_id)
    {
        session.cancelled.store(true, Ordering::Relaxed);
        receiver.state.emit(AppEvent::SessionFailed {
            session_id,
            direction: Direction::Receive,
            error: "对方取消了传输".to_string(),
        });
    }
    StatusCode::OK.into_response()
}

/// 接收聊天消息。一次请求可以携带多条，便于将来做批量/离线补发。
async fn chat_handler(
    State(receiver): State<Arc<Receiver>>,
    Json(envelope): Json<ChatEnvelope>,
) -> Response {
    if envelope.messages.is_empty() {
        return (StatusCode::BAD_REQUEST, "空消息").into_response();
    }
    let peer = peer_key(&envelope.info);
    let alias = envelope.info.alias.clone();
    receiver.state.chats.touch_alias(&peer, &alias);

    let mut delivered = 0usize;
    for payload in envelope.messages {
        let text = payload.text.trim();
        if text.is_empty() {
            continue;
        }
        let mut message = ChatMessage::text(&peer, false, text);
        if !payload.id.is_empty() {
            message.id = payload.id;
        }
        if payload.at > 0 {
            message.at = payload.at;
        }
        receiver.state.chats.append(message.clone());
        receiver.state.emit(AppEvent::ChatReceived {
            peer: peer.clone(),
            alias: alias.clone(),
            text: message.text.clone(),
        });
        delivered += 1;
    }

    receiver.state.emit(AppEvent::ChatUpdated { peer });
    Json(serde_json::json!({ "ok": true, "received": delivered })).into_response()
}
