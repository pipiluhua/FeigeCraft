//! 设备发现：IPv4 组播（224.0.0.167:53317）+ 子网 HTTP 扫描兜底。
//!
//! **多网段**：一台机器同时接着有线、Wi-Fi、虚拟网卡时，Windows 只会用默认路由
//! 那一个接口收发组播，其它网段上的设备永远发现不了。这里给每个 IPv4 接口各建
//! 一对套接字（一个收、一个发），逐个接口加入组播组并绑定出口，
//! 于是每个网段都被覆盖；扫描兜底也按接口逐个网段来。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::stream::{self, StreamExt};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::time::interval;

use crate::events::AppEvent;
use crate::model::{DiscoveredDevice, DiscoveryMessage, DeviceInfo, DEFAULT_PORT, MULTICAST_ADDR};
use crate::state::SharedState;
use crate::tls;
use crate::util;

/// 设备多久没消息就认为离线。
const STALE_AFTER: Duration = Duration::from_secs(120);
/// 组播广播间隔。
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(5);
/// 单个接口做扫描兜底时最多探测多少个地址（避免 /16 把并发池打满）。
const SCAN_PER_INTERFACE: usize = 1024;

pub struct Discovery {
    state: Arc<SharedState>,
    /// 每个 IPv4 接口一个接收套接字（绑 `接口:53317`），用来听别人主动广播的组播包。
    listeners: Vec<Arc<UdpSocket>>,
    /// 每个接口一个发送套接字（绑随机端口 + 指定组播出口）：
    /// 广播后对方的回包会**单播**回到这里。Windows 上同一个端口被多个进程
    /// 重复绑定时组播包只会投递给其中一个，用它发广播能保证「一定收得到回包」。
    senders: Vec<Arc<UdpSocket>>,
    /// 本机所有接口的地址 + 掩码，用来判断对端地址是否与自己同网段。
    local: Vec<(Ipv4Addr, Ipv4Addr)>,
    devices: Mutex<HashMap<String, DiscoveredDevice>>,
    running: AtomicBool,
}

impl Discovery {
    /// 为每个接口绑定组播端口并启动接收 / 广播任务。
    pub async fn start(state: Arc<SharedState>) -> Result<Arc<Self>> {
        let local = util::local_ipv4();
        let mut interfaces: Vec<Ipv4Addr> = local.iter().map(|(ip, _)| *ip).collect();
        if interfaces.is_empty() {
            // 枚举不到网卡（容器 / 权限受限）时退回通配地址，行为等同单接口。
            interfaces.push(Ipv4Addr::UNSPECIFIED);
        }

        let mut listeners = Vec::new();
        let mut senders = Vec::new();
        for interface in &interfaces {
            let label = if interface.is_unspecified() {
                "默认接口".to_string()
            } else {
                interface.to_string()
            };
            match create_socket(*interface, 0, false) {
                Ok(socket) => senders.push(Arc::new(socket)),
                Err(err) => tracing::warn!("接口 {label} 的广播套接字创建失败：{err}"),
            }
            match create_socket(*interface, DEFAULT_PORT, true) {
                Ok(socket) => {
                    tracing::debug!("已在接口 {label} 上监听组播端口 {DEFAULT_PORT}");
                    listeners.push(Arc::new(socket));
                }
                Err(err) => {
                    tracing::warn!("接口 {label} 无法监听 UDP {DEFAULT_PORT}（{err}）");
                }
            }
        }
        if senders.is_empty() {
            // 所有具体接口都失败时，至少留一个通配发送口，别让发现功能整体不可用。
            let fallback = create_socket(Ipv4Addr::UNSPECIFIED, 0, false)
                .context("创建组播发送套接字失败")?;
            senders.push(Arc::new(fallback));
        }
        if listeners.is_empty() {
            tracing::warn!("所有接口都无法监听 {DEFAULT_PORT}，仅使用主动广播 + 单播回包");
        }

        let discovery = Arc::new(Self {
            state,
            listeners,
            senders,
            local,
            devices: Mutex::new(HashMap::new()),
            running: AtomicBool::new(true),
        });

        for listener in &discovery.listeners {
            let this = discovery.clone();
            let socket = listener.clone();
            tokio::spawn(async move { this.recv_loop(socket).await });
        }
        for sender in &discovery.senders {
            let this = discovery.clone();
            let socket = sender.clone();
            tokio::spawn(async move { this.recv_loop(socket).await });
        }
        {
            let this = discovery.clone();
            tokio::spawn(async move { this.announce_loop().await });
        }

        discovery.announce().await;
        Ok(discovery)
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// 当前已发现的设备（按别名排序）。
    pub fn devices(&self) -> Vec<DiscoveredDevice> {
        let mut list: Vec<_> = self
            .devices
            .lock()
            .expect("devices mutex poisoned")
            .values()
            .cloned()
            .collect();
        list.sort_by_key(|item| item.info.alias.to_lowercase());
        list
    }

    pub fn get(&self, key: &str) -> Option<DiscoveredDevice> {
        self.devices
            .lock()
            .expect("devices mutex poisoned")
            .get(key)
            .cloned()
    }

    /// 通过别名或 IP 查找设备。
    pub fn find(&self, query: &str) -> Option<DiscoveredDevice> {
        let query_lower = query.to_lowercase();
        self.devices()
            .into_iter()
            .find(|d| {
                d.info.alias.to_lowercase() == query_lower
                    || d.ip.to_string() == query
                    || d.info.fingerprint == query
            })
    }

    /// 主动在每个接口上各广播一次。
    pub async fn announce(&self) {
        let message = DiscoveryMessage::announce(&self.state.device_info());
        let Ok(payload) = serde_json::to_vec(&message) else { return };
        let target = SocketAddr::from((Ipv4Addr::from(MULTICAST_ADDR), DEFAULT_PORT));
        for sender in &self.senders {
            if let Err(err) = sender.send_to(&payload, target).await {
                tracing::debug!("组播广播失败: {err}");
            }
        }
    }

    async fn announce_loop(self: Arc<Self>) {
        let mut ticker = interval(ANNOUNCE_INTERVAL);
        loop {
            ticker.tick().await;
            if !self.running.load(Ordering::Relaxed) {
                return;
            }
            self.announce().await;
            self.prune_stale();
        }
    }

    async fn recv_loop(self: Arc<Self>, socket: Arc<UdpSocket>) {
        let mut buffer = vec![0u8; 64 * 1024];
        while self.running.load(Ordering::Relaxed) {
            // 带超时：`stop()` 之后循环才能及时退出，不然任务会一直挂在 recv 上。
            let received =
                tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut buffer)).await;
            match received {
                Ok(Ok((len, source))) => {
                    self.handle_packet(&buffer[..len], source, socket.clone()).await;
                }
                Ok(Err(err)) => {
                    tracing::debug!("组播接收错误: {err}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(_) => {}
            }
        }
    }

    async fn handle_packet(&self, data: &[u8], source: SocketAddr, socket: Arc<UdpSocket>) {
        let Ok(message) = serde_json::from_slice::<DiscoveryMessage>(data) else {
            return;
        };
        let own = self.state.device_info();
        if !message.info.fingerprint.is_empty() && message.info.fingerprint == own.fingerprint {
            return; // 自己发的包
        }
        if message.info.fingerprint.is_empty() && message.info.alias == own.alias {
            return;
        }

        let mut device = DiscoveredDevice {
            info: message.info.clone(),
            ip: source.ip(),
            last_seen: Instant::now(),
            via_multicast: true,
        };
        if device.info.port == 0 {
            device.info.port = DEFAULT_PORT;
        }
        tracing::debug!("收到设备广播 {} @ {}", device.info.alias, source);
        let key = device_key(&device);
        // 同一设备每 5 秒就会广播一次，只有「新设备」或「信息变化」才通知上层
        let changed = {
            let mut guard = self.devices.lock().expect("devices mutex poisoned");
            // 一台设备有多个网卡时会从多个源地址各广播一次：
            // 选一个和自己同网段的地址，避免地址在两条广播之间来回跳。
            if let Some(previous) = guard.get(&key) {
                let fresh = previous.last_seen.elapsed() < Duration::from_secs(30);
                if fresh && previous.ip != device.ip {
                    device.ip = util::prefer_lan_ip(previous.ip, device.ip, &self.local);
                }
            }
            let changed = match guard.get(&key) {
                Some(previous) => {
                    previous.info.alias != device.info.alias
                        || previous.info.port != device.info.port
                        || previous.ip != device.ip
                        || previous.info.device_type != device.info.device_type
                }
                None => true,
            };
            guard.insert(key, device.clone());
            changed
        };

        if changed {
            self.state.emit(AppEvent::DeviceFound(device));
        }

        if message.is_announce() {
            // 对方在广播，直接单播回应，让它也能看到我们
            let answer = DiscoveryMessage::answer(&own);
            if let Ok(payload) = serde_json::to_vec(&answer) {
                let _ = socket.send_to(&payload, source).await;
            }
        }
    }

    fn prune_stale(&self) {
        let mut guard = self.devices.lock().expect("devices mutex poisoned");
        let expired: Vec<String> = guard
            .iter()
            .filter(|(_, d)| d.last_seen.elapsed() > STALE_AFTER)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            guard.remove(&key);
            self.state.emit(AppEvent::DeviceLost { fingerprint: key });
        }
    }

    /// 组播被屏蔽时的兜底：逐个接口扫描自己所在的子网。
    ///
    /// 多网段（例如有线 192.168.1.x 与 Wi-Fi 10.0.0.x）会分别扫描，
    /// 每个网段的上限独立计算，避免一个 /16 把别的网段挤掉。
    pub async fn scan(&self) -> Result<usize> {
        let tls_config = tls::client_config(None)?;
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls_config)
            .http1_only()
            .connect_timeout(Duration::from_millis(600))
            .timeout(Duration::from_millis(1500))
            .build()
            .context("构建扫描用 HTTP 客户端失败")?;

        let local = if self.local.is_empty() { util::local_ipv4() } else { self.local.clone() };
        let mut targets = Vec::new();
        for (ip, mask) in &local {
            let mut hosts = util::subnet_hosts(*ip, *mask);
            hosts.truncate(SCAN_PER_INTERFACE);
            targets.extend(hosts);
        }
        self.scan_targets(client, targets).await
    }

    /// 扫描指定网段。跨网段（路由器不转发组播）时由界面手动触发。
    pub async fn scan_subnet(&self, network: Ipv4Addr, mask: Ipv4Addr) -> Result<usize> {
        let tls_config = tls::client_config(None)?;
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls_config)
            .http1_only()
            .connect_timeout(Duration::from_millis(600))
            .timeout(Duration::from_millis(1500))
            .build()
            .context("构建扫描用 HTTP 客户端失败")?;
        let targets = util::subnet_hosts(network, mask);
        self.scan_targets(client, targets).await
    }

    async fn scan_targets(
        &self,
        client: reqwest::Client,
        mut targets: Vec<Ipv4Addr>,
    ) -> Result<usize> {
        targets.sort();
        targets.dedup();
        if targets.is_empty() {
            return Ok(0);
        }

        let found: Vec<(IpAddr, DeviceInfo)> = stream::iter(targets)
            .map(|ip| {
                let client = client.clone();
                async move {
                    let url = format!("https://{ip}:{DEFAULT_PORT}/api/localsend/v2/info");
                    let response = client.get(url).send().await.ok()?;
                    if !response.status().is_success() {
                        return None;
                    }
                    let info = response.json::<DeviceInfo>().await.ok()?;
                    Some((IpAddr::V4(ip), info))
                }
            })
            .buffer_unordered(64)
            .filter_map(|item| async move { item })
            .collect()
            .await;

        let own = self.state.fingerprint();
        let mut count = 0usize;
        for (ip, info) in found {
            if !info.fingerprint.is_empty() && info.fingerprint == own {
                continue;
            }
            let device = DiscoveredDevice {
                info,
                ip,
                last_seen: Instant::now(),
                via_multicast: false,
            };
            let key = device_key(&device);
            let is_new = self
                .devices
                .lock()
                .expect("devices mutex poisoned")
                .insert(key, device.clone())
                .is_none();
            if is_new {
                self.state.emit(AppEvent::DeviceFound(device));
                count += 1;
            }
        }
        Ok(count)
    }
}

fn device_key(device: &DiscoveredDevice) -> String {
    if !device.info.fingerprint.is_empty() {
        device.info.fingerprint.clone()
    } else {
        format!("{}@{}", device.info.alias, device.ip)
    }
}

/// 在指定接口上创建 UDP 套接字。
///
/// `interface` 同时决定三件事：绑定地址（只收这个网段的包）、组播出口
/// （`IP_MULTICAST_IF`）、加入组播组时的接口。这是跨网段能互相发现的关键——
/// 只绑 `0.0.0.0` 时 Windows 永远走默认路由那一张网卡。
fn create_socket(interface: Ipv4Addr, port: u16, join_group: bool) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .context("创建 UDP 套接字失败")?;
    socket.set_reuse_address(true).context("设置 SO_REUSEADDR 失败")?;
    #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
    {
        let _ = socket.set_reuse_port(true);
    }
    socket.set_broadcast(true).ok();
    socket
        .bind(&SocketAddr::from((interface, port)).into())
        .with_context(|| format!("绑定 UDP {interface}:{port} 失败"))?;
    if join_group {
        if let Err(err) = socket
            .join_multicast_v4(&Ipv4Addr::from(MULTICAST_ADDR), &interface)
        {
            tracing::warn!("接口 {interface} 加入组播组失败（{err}），仍会主动广播");
        }
    }
    // 多实例测试需要自己也能收到自己的广播
    socket.set_multicast_loop_v4(true).ok();
    socket.set_multicast_ttl_v4(1).ok();
    let _ = socket.set_multicast_if_v4(&interface);
    socket.set_nonblocking(true)?;
    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket).context("转换为异步 UDP 套接字失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 多网段的前提：同一个端口能在多个接口地址上各绑一个套接字。
    /// Windows 上这不是默认行为，靠 `SO_REUSEADDR`；一旦不成立，
    /// 第二个网卡就永远收不到组播，所以单独盯一下。
    #[tokio::test]
    async fn binds_same_port_on_multiple_interfaces() {
        let port = 53991;
        let mut sockets = Vec::new();
        sockets.push(create_socket(Ipv4Addr::LOCALHOST, port, true).expect("绑定回环失败"));
        for (ip, _) in util::local_ipv4().into_iter().take(2) {
            sockets
                .push(create_socket(ip, port, true).unwrap_or_else(|err| panic!("绑定 {ip} 失败: {err}")));
        }
        assert!(sockets.len() >= 2, "至少要能绑到两个接口上");
    }

    /// 同一地址重复绑定（多开测试场景）也必须成功。
    #[tokio::test]
    async fn rebinding_same_address_works() {
        let addr: Ipv4Addr = "127.0.0.1".parse().unwrap();
        let _first = create_socket(addr, 53992, false).expect("第一次绑定失败");
        let _second = create_socket(addr, 53992, false).expect("第二次绑定失败（SO_REUSEADDR 没生效）");
    }
}
