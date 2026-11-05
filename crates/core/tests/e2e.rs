//! 端到端测试：在本机启动两个节点，真实走一遍
//! TLS 握手 → prepare-upload → 流式 upload → 落盘。

use std::path::PathBuf;
use std::time::Duration;

use feigecraft_core::chat::{peer_key, MessageKind, MessageStatus};
use feigecraft_core::client::{collect_files, send_chat, send_files};
use feigecraft_core::config::Config;
use feigecraft_core::events::AppEvent;
use feigecraft_core::model::DiscoveredDevice;
use feigecraft_core::node::Node;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("feige-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录失败");
    dir
}

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_and_receive_over_tls() {
    let root = temp_dir("e2e");
    let config_dir = root.join("config");

    let recv_dir = root.join("recv");
    let recv_config = Config {
        alias: "接收端".to_string(),
        port: 53421,
        download_dir: recv_dir.clone(),
        auto_accept: true,
        ..Config::default()
    };
    let receiver = Node::start_in(recv_config, config_dir.join("recv")).await.expect("接收端启动失败");

    let send_config = Config {
        alias: "发送端".to_string(),
        port: 53422,
        download_dir: root.join("send-dl"),
        ..Config::default()
    };
    let sender = Node::start_in(send_config, config_dir.join("send")).await.expect("发送端启动失败");

    let target = DiscoveredDevice {
        info: receiver.device_info(),
        ip: "127.0.0.1".parse().expect("ip"),
        last_seen: std::time::Instant::now(),
        via_multicast: false,
    };

    let source = root.join("payload.bin");
    let data = payload(3 * 1024 * 1024);
    std::fs::write(&source, &data).expect("写入源文件失败");

    let files = collect_files(&[source]);
    assert_eq!(files.len(), 1);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let state = sender.state.clone();
    let send = tokio::spawn(async move { send_files(state, target, files, cancel_rx).await });
    let report = tokio::time::timeout(Duration::from_secs(90), send)
        .await
        .expect("发送超时")
        .expect("发送任务 panic")
        .expect("发送失败");

    assert_eq!(report.files, 1);
    assert_eq!(report.bytes, data.len() as u64);

    let received_path = recv_dir.join("payload.bin");
    for _ in 0..100 {
        if received_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let received = std::fs::read(&received_path).expect("接收端没有落盘");
    assert_eq!(received.len(), data.len(), "文件大小不一致");
    assert!(received == data, "文件内容不一致");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_transfer_reports_error() {
    let root = temp_dir("reject");
    let config_dir = root.join("config");

    let recv_config = Config {
        alias: "拒绝端".to_string(),
        port: 53431,
        download_dir: root.join("recv"),
        auto_accept: false,
        ..Config::default()
    };
    let receiver = Node::start_in(recv_config, config_dir.join("recv")).await.expect("接收端启动失败");

    let send_config = Config {
        alias: "发送端".to_string(),
        port: 53432,
        ..Config::default()
    };
    let sender = Node::start_in(send_config, config_dir.join("send")).await.expect("发送端启动失败");

    // 收到请求后立刻拒绝
    let receiver_for_task = receiver.clone();
    let mut events = receiver.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if let feigecraft_core::events::AppEvent::IncomingRequest { session_id, .. } = event {
                receiver_for_task.receiver.resolve(&session_id, false);
                break;
            }
        }
    });

    let target = DiscoveredDevice {
        info: receiver.device_info(),
        ip: "127.0.0.1".parse().expect("ip"),
        last_seen: std::time::Instant::now(),
        via_multicast: false,
    };
    let source = root.join("small.txt");
    std::fs::write(&source, b"hello").expect("写入失败");
    let files = collect_files(&[source]);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let result = send_files(sender.state.clone(), target, files, cancel_rx).await;
    let err = result.expect_err("对方拒绝时应当返回错误");
    assert!(err.to_string().contains("拒绝"), "错误信息应说明被拒绝: {err}");
    assert!(!root.join("recv").join("small.txt").exists());
}

/// 聊天协议：发送端本地落库 → POST /api/feigecraft/chat → 接收端入库并广播事件。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_message_reaches_peer() {
    let root = temp_dir("chat");
    let config_dir = root.join("config");

    let recv_config = Config {
        alias: "聊天接收端".to_string(),
        port: 53441,
        download_dir: root.join("recv"),
        auto_accept: true,
        ..Config::default()
    };
    let receiver =
        Node::start_in(recv_config, config_dir.join("recv")).await.expect("接收端启动失败");

    let send_config = Config {
        alias: "聊天发送端".to_string(),
        port: 53442,
        ..Config::default()
    };
    let sender = Node::start_in(send_config, config_dir.join("send")).await.expect("发送端启动失败");

    let target = DiscoveredDevice {
        info: receiver.device_info(),
        ip: "127.0.0.1".parse().expect("ip"),
        last_seen: std::time::Instant::now(),
        via_multicast: false,
    };

    let mut receiver_events = receiver.subscribe();
    let text = "你好 FeigeCraft 👋 这条消息应当完整到达"; 
    let sent = send_chat(sender.state.clone(), &target, text).await.expect("发送失败");
    assert_eq!(sent.kind, MessageKind::Text);
    assert!(sent.outgoing, "自己发出的消息应为 outgoing");

    // 发送端本地状态应被标记为已送达
    let own_key = peer_key(&receiver.device_info());
    let own_thread = sender.state.chats.thread(&own_key);
    assert_eq!(own_thread.len(), 1);
    assert_eq!(own_thread[0].status, MessageStatus::Sent);
    assert_eq!(own_thread[0].text, text);

    // 接收端应收到事件并入库，且归属到发送端的会话
    let received = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(event) = receiver_events.recv().await {
            if let AppEvent::ChatReceived { peer, text, .. } = event {
                return (peer, text);
            }
        }
        panic!("事件通道关闭");
    })
    .await
    .expect("没有收到 ChatReceived 事件");
    assert_eq!(received.1, text);

    let sender_key = peer_key(&sender.device_info());
    assert_eq!(received.0, sender_key, "会话应由发送端指纹标识");

    let stored = receiver.state.chats.thread(&sender_key);
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].outgoing, "收到的消息应为 incoming");
    assert_eq!(stored[0].text, text);
    assert_eq!(
        receiver.state.chats.alias(&sender_key).as_deref(),
        Some("聊天发送端"),
        "接收端应记录对方别名"
    );
}
