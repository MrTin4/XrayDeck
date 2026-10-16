use crate::settings::api_endpoint;
use anyhow::{Context, Result};
use futures::StreamExt;
use prost::Message;
use serde::Serialize;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};
use tonic::{
    Code, Request,
    transport::{Channel, Endpoint},
};

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub tag: String,
    pub protocol: String,
    pub address: Option<String>,
    pub port: Option<u32>,
    pub proxy: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Observation {
    pub tag: String,
    pub alive: bool,
    pub delay_ms: Option<i64>,
    pub last_try: i64,
    pub burst: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ApiSnapshot {
    pub services: Vec<String>,
    pub discovery_note: Option<String>,
    pub nodes: Vec<Node>,
    pub nodes_note: Option<String>,
    pub observations: Vec<Observation>,
    pub observations_note: Option<String>,
    pub checked_at: String,
    pub traffic: std::collections::HashMap<String, NodeTraffic>,
    pub traffic_note: Option<String>,
    pub selected_nodes: std::collections::BTreeSet<String>,
    pub balancers_note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NodeTraffic {
    pub uplink: Option<u64>,
    pub downlink: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSort {
    Newest,
    Oldest,
    Delay,
    DelayDescending,
    Api,
}

impl NodeSort {
    pub fn toggle_delay(self) -> Self {
        if self == Self::Delay {
            Self::DelayDescending
        } else {
            Self::Delay
        }
    }

    pub fn toggle_time(self) -> Self {
        if self == Self::Oldest {
            Self::Newest
        } else {
            Self::Oldest
        }
    }
}

impl ApiSnapshot {
    pub fn sorted_nodes(&self, sort: NodeSort) -> Vec<(&Node, Option<&Observation>)> {
        let observations: std::collections::HashMap<_, _> = self
            .observations
            .iter()
            .map(|observation| (observation.tag.as_str(), observation))
            .collect();
        let mut rows: Vec<_> = self
            .nodes
            .iter()
            .map(|node| (node, observations.get(node.tag.as_str()).copied()))
            .collect();
        if matches!(sort, NodeSort::Newest | NodeSort::Oldest) {
            let time = |observation: Option<&Observation>| {
                observation
                    .map(|o| o.last_try)
                    .filter(|&t| t > 0 && chrono::DateTime::from_timestamp(t, 0).is_some())
            };
            rows.sort_by(|a, b| match (time(a.1), time(b.1)) {
                (Some(a), Some(b)) if sort == NodeSort::Newest => b.cmp(&a),
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        } else if matches!(sort, NodeSort::Delay | NodeSort::DelayDescending) {
            let delay = |observation: Option<&Observation>| {
                observation
                    .filter(|o| o.alive)
                    .and_then(|o| o.delay_ms)
                    .filter(|&delay| delay >= 0)
            };
            rows.sort_by(|a, b| match (delay(a.1), delay(b.1)) {
                (Some(a), Some(b)) if sort == NodeSort::DelayDescending => b.cmp(&a),
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        }
        // Stable partition preserves the chosen secondary order within both groups.
        rows.sort_by_key(|(node, _)| !self.selected_nodes.contains(&node.tag));
        rows
    }
}

// Only fields needed by the panel are decoded. Credentials never enter its model.
#[derive(Clone, PartialEq, Message)]
pub struct Empty {}
#[derive(Clone, PartialEq, Message)]
struct TypedMessage {
    #[prost(string, tag = "1")]
    kind: String,
    #[prost(bytes = "vec", tag = "2")]
    value: Vec<u8>,
}
#[derive(Clone, PartialEq, Message)]
struct Outbound {
    #[prost(string, tag = "1")]
    tag: String,
    #[prost(message, optional, tag = "3")]
    proxy_settings: Option<TypedMessage>,
}
#[derive(Clone, PartialEq, Message)]
struct Outbounds {
    #[prost(message, repeated, tag = "1")]
    outbounds: Vec<Outbound>,
}
#[derive(Clone, PartialEq, Message)]
struct ProxyConfig {
    #[prost(message, optional, tag = "1")]
    server: Option<ServerEndpoint>,
}
#[derive(Clone, PartialEq, Message)]
struct ServerEndpoint {
    #[prost(message, optional, tag = "1")]
    address: Option<Address>,
    #[prost(uint32, tag = "2")]
    port: u32,
}
#[derive(Clone, PartialEq, Message)]
struct Address {
    #[prost(bytes = "vec", tag = "1")]
    ip: Vec<u8>,
    #[prost(string, tag = "2")]
    domain: String,
}
#[derive(Clone, PartialEq, Message)]
struct StatusResponse {
    #[prost(message, optional, tag = "1")]
    status: Option<Statuses>,
}
#[derive(Clone, PartialEq, Message)]
struct Statuses {
    #[prost(message, repeated, tag = "1")]
    status: Vec<OutboundStatus>,
}
#[derive(Clone, PartialEq, Message)]
struct OutboundStatus {
    #[prost(bool, tag = "1")]
    alive: bool,
    #[prost(int64, tag = "2")]
    delay: i64,
    #[prost(string, tag = "4")]
    tag: String,
    #[prost(int64, tag = "6")]
    last_try: i64,
    #[prost(message, optional, tag = "7")]
    health_ping: Option<Empty>,
}

fn decode_observation(status: OutboundStatus) -> Observation {
    Observation {
        tag: status.tag,
        alive: status.alive,
        delay_ms: (status.alive && status.delay >= 0).then_some(status.delay),
        last_try: status.last_try,
        burst: status.health_ping.is_some(),
    }
}
#[derive(Clone, PartialEq, Message)]
struct SystemStats {
    #[prost(uint32, tag = "10")]
    uptime: u32,
}

#[derive(Clone, PartialEq, Message)]
struct QueryStatsRequest {
    #[prost(string, tag = "1")]
    pattern: String,
    #[prost(bool, tag = "2")]
    reset: bool,
}
#[derive(Clone, PartialEq, Message)]
struct Stat {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(int64, tag = "2")]
    value: i64,
}
#[derive(Clone, PartialEq, Message)]
struct QueryStatsResponse {
    #[prost(message, repeated, tag = "1")]
    stat: Vec<Stat>,
}
#[derive(Clone, PartialEq, Message)]
struct BalancerRequest {
    #[prost(string, tag = "1")]
    tag: String,
}
#[derive(Clone, PartialEq, Message)]
struct BalancerResponse {
    #[prost(message, optional, tag = "1")]
    balancer: Option<BalancerInfo>,
}
#[derive(Clone, PartialEq, Message)]
struct BalancerInfo {
    #[prost(message, optional, tag = "5")]
    override_target: Option<OverrideTarget>,
    #[prost(message, optional, tag = "6")]
    principle_target: Option<PrincipleTarget>,
}
#[derive(Clone, PartialEq, Message)]
struct OverrideTarget {
    #[prost(string, tag = "2")]
    target: String,
}
#[derive(Clone, PartialEq, Message)]
struct PrincipleTarget {
    #[prost(string, repeated, tag = "1")]
    tag: Vec<String>,
}

fn decode_traffic(response: QueryStatsResponse) -> std::collections::HashMap<String, NodeTraffic> {
    let mut traffic = std::collections::HashMap::<String, NodeTraffic>::new();
    for stat in response.stat {
        let Some(name) = stat.name.strip_prefix("outbound>>>") else {
            continue;
        };
        let Some((tag, direction)) = name.rsplit_once(">>>traffic>>>") else {
            continue;
        };
        let Ok(value) = u64::try_from(stat.value) else {
            continue;
        };
        if tag.is_empty() || !matches!(direction, "uplink" | "downlink") {
            continue;
        }
        let entry = traffic.entry(tag.to_owned()).or_default();
        if direction == "uplink" {
            entry.uplink = Some(value);
        } else {
            entry.downlink = Some(value);
        }
    }
    traffic
}

fn balancer_targets(info: BalancerInfo) -> Option<Vec<String>> {
    if let Some(target) = info.override_target.filter(|o| !o.target.is_empty()) {
        return Some(vec![target.target]);
    }
    info.principle_target.map(|targets| {
        targets
            .tag
            .into_iter()
            .filter(|tag| !tag.is_empty())
            .collect()
    })
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionRequest {
    #[prost(oneof = "reflection_request::MessageRequest", tags = "7")]
    message_request: Option<reflection_request::MessageRequest>,
}
mod reflection_request {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum MessageRequest {
        #[prost(string, tag = "7")]
        ListServices(String),
    }
}
impl ReflectionRequest {
    fn list_services() -> Self {
        Self {
            // Presence matters: an empty oneof string must still be transmitted.
            message_request: Some(reflection_request::MessageRequest::ListServices(
                String::new(),
            )),
        }
    }
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionResponse {
    #[prost(message, optional, tag = "6")]
    list: Option<ServiceList>,
    #[prost(message, optional, tag = "7")]
    error: Option<ReflectionError>,
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionError {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}
#[derive(Clone, PartialEq, Message)]
struct ServiceList {
    #[prost(message, repeated, tag = "1")]
    services: Vec<ServiceName>,
}
#[derive(Clone, PartialEq, Message)]
struct ServiceName {
    #[prost(string, tag = "1")]
    name: String,
}

async fn unary<Resp: Message + Default + 'static>(
    channel: Channel,
    path: &'static str,
) -> Result<Resp, tonic::Status> {
    unary_with(channel, path, Empty {}).await
}

async fn unary_with<Req: Message + Default + 'static, Resp: Message + Default + 'static>(
    channel: Channel,
    path: &'static str,
    body: Req,
) -> Result<Resp, tonic::Status> {
    let mut grpc = tonic::client::Grpc::new(channel).max_decoding_message_size(16 * 1024 * 1024);
    grpc.ready()
        .await
        .map_err(|_| tonic::Status::unavailable("gRPC 连接不可用"))?;
    let mut request = Request::new(body);
    request.set_timeout(Duration::from_secs(8));
    grpc.unary(
        request,
        tonic::codegen::http::uri::PathAndQuery::from_static(path),
        tonic_prost::ProstCodec::<Req, Resp>::default(),
    )
    .await
    .map(|r| r.into_inner())
}

async fn reflect_at(channel: Channel, path: &'static str) -> Result<Vec<String>, tonic::Status> {
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready()
        .await
        .map_err(|_| tonic::Status::unavailable("gRPC 连接不可用"))?;
    let input = futures::stream::iter([ReflectionRequest::list_services()]);
    let mut request = Request::new(input);
    request.set_timeout(Duration::from_secs(8));
    let response = grpc
        .streaming(
            request,
            tonic::codegen::http::uri::PathAndQuery::from_static(path),
            tonic_prost::ProstCodec::<ReflectionRequest, ReflectionResponse>::default(),
        )
        .await?;
    let response = response
        .into_inner()
        .message()
        .await?
        .ok_or_else(|| tonic::Status::data_loss("Reflection 未返回响应"))?;
    if let Some(error) = response.error {
        return Err(tonic::Status::new(
            Code::from_i32(error.code),
            error.message,
        ));
    }
    let list = response
        .list
        .ok_or_else(|| tonic::Status::data_loss("Reflection 响应缺少服务列表"))?;
    let mut names: Vec<_> = list.services.into_iter().map(|s| s.name).collect();
    names.sort();
    names.dedup();
    Ok(names)
}

async fn reflect(channel: Channel) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for (version, path) in [
        (
            "v1",
            "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
        ),
        (
            "v1alpha",
            "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
        ),
    ] {
        let result =
            tokio::time::timeout(Duration::from_secs(8), reflect_at(channel.clone(), path))
                .await
                .unwrap_or_else(|_| Err(tonic::Status::deadline_exceeded("Reflection 请求超时")));
        match result {
            Ok(names) => return Ok(names),
            Err(status) => failures.push(format!(
                "{version}: {:?}（{}）",
                status.code(),
                status.message()
            )),
        }
    }
    anyhow::bail!(
        "无法完整发现 API 服务：{}；以下能力按只读调用验证",
        failures.join("；")
    )
}

pub fn capability_error(status: &tonic::Status) -> String {
    match status.code() {
        Code::Unimplemented => "未开启该服务或目标版本不支持此方法".into(),
        Code::DeadlineExceeded => "请求超时，能力尚未确认".into(),
        Code::Unavailable => "连接中断，能力尚未确认".into(),
        Code::PermissionDenied | Code::Unauthenticated => "访问被拒绝，能力尚未确认".into(),
        _ => format!("调用失败（{:?}），模块可能未配置", status.code()),
    }
}

const OBSERVATORY_SERVICE: &str = "xray.core.app.observatory.command.ObservatoryService";

fn observation_error(status: &tonic::Status, discovered: Option<&[String]>) -> String {
    if status.code() == Code::Unimplemented {
        return match discovered {
            Some(services) if !services.iter().any(|name| name == OBSERVATORY_SERVICE) =>
                "已确认当前 API 未注册 ObservatoryService。请在 Xray 的 api.services 中添加 ObservatoryService，并配置观测模块；重启核心后刷新。已有观测配置请保留。".into(),
            Some(_) => "ObservatoryService 已注册，但 GetOutboundStatus 方法返回 Unimplemented；请检查核心实现或版本兼容性。".into(),
            None => "GetOutboundStatus 返回 Unimplemented，但服务发现失败，尚不能确定是服务未开启还是方法不受支持。请开启 ReflectionService 后重试。".into(),
        };
    }
    format!("观测查询失败：{}", capability_error(status))
}

fn decode_node(outbound: Outbound) -> Node {
    let mut node = Node {
        tag: outbound.tag,
        protocol: "未知".into(),
        address: None,
        port: None,
        proxy: false,
    };
    let Some(settings) = outbound.proxy_settings else {
        return node;
    };
    let kind = settings
        .kind
        .strip_prefix("xray.proxy.")
        .or_else(|| settings.kind.strip_prefix("v2ray.core.proxy."));
    let Some(kind) = kind else { return node };
    node.protocol = kind.split('.').next().unwrap_or("未知").to_owned();
    node.proxy = !matches!(
        node.protocol.as_str(),
        "freedom" | "blackhole" | "dns" | "loopback"
    );
    // These schemas share ServerEndpoint at field 1; other protocols are not guessed.
    if matches!(
        kind,
        "vless.outbound.Config"
            | "vmess.outbound.Config"
            | "trojan.ClientConfig"
            | "shadowsocks.ClientConfig"
            | "http.ClientConfig"
            | "socks.ClientConfig"
    ) && let Ok(config) = ProxyConfig::decode(settings.value.as_slice())
        && let Some(server) = config.server
    {
        node.port = (server.port > 0 && server.port <= 65535).then_some(server.port);
        node.address = server.address.and_then(|a| {
            if !a.domain.is_empty() {
                return Some(a.domain);
            }
            match a.ip.len() {
                4 => Some(IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(a.ip).ok()?)).to_string()),
                16 => {
                    Some(IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(a.ip).ok()?)).to_string())
                }
                _ => None,
            }
        });
    }
    node
}

pub async fn inspect(address: &str) -> Result<ApiSnapshot> {
    let endpoint = api_endpoint(address)?;
    let channel = Endpoint::from_shared(endpoint)?
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .connect()
        .await
        .context("无法连接 Xray gRPC API")?;
    let discovery = reflect(channel.clone()).await;
    let discovered_services = discovery.as_ref().ok().cloned();
    let config = crate::routing_config::discover(address);
    let balancer_tags: Vec<_> = config
        .as_ref()
        .map(|balancers| {
            balancers
                .iter()
                .map(|balancer| balancer.tag.clone())
                .collect()
        })
        .unwrap_or_default();
    let (nodes, observation, stats, routing, traffic, balancers) = tokio::join!(
        unary::<Outbounds>(
            channel.clone(),
            "/xray.app.proxyman.command.HandlerService/ListOutbounds"
        ),
        unary::<StatusResponse>(
            channel.clone(),
            "/xray.core.app.observatory.command.ObservatoryService/GetOutboundStatus"
        ),
        unary::<SystemStats>(
            channel.clone(),
            "/xray.app.stats.command.StatsService/GetSysStats"
        ),
        unary::<Empty>(
            channel.clone(),
            "/xray.app.router.command.RoutingService/ListRule"
        ),
        unary_with::<_, QueryStatsResponse>(
            channel.clone(),
            "/xray.app.stats.command.StatsService/QueryStats",
            QueryStatsRequest {
                pattern: "outbound>>>".into(),
                reset: false
            }
        ),
        futures::stream::iter(balancer_tags.iter().cloned())
            .map(|tag| {
                let channel = channel.clone();
                async move {
                    let result = unary_with::<_, BalancerResponse>(
                        channel,
                        "/xray.app.router.command.RoutingService/GetBalancerInfo",
                        BalancerRequest { tag: tag.clone() },
                    )
                    .await;
                    (tag, result)
                }
            })
            .buffer_unordered(8)
            .collect::<Vec<_>>()
    );
    let mut snapshot = ApiSnapshot {
        services: discovery.as_ref().cloned().unwrap_or_default(),
        discovery_note: discovery.err().map(|e| e.to_string()),
        nodes: vec![],
        nodes_note: None,
        observations: vec![],
        observations_note: None,
        checked_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        ..Default::default()
    };
    match traffic {
        Ok(response) => {
            snapshot.traffic = decode_traffic(response);
            snapshot
                .services
                .push("StatsService/QueryStats（只读调用已验证）".into());
            if snapshot.traffic.is_empty() {
                snapshot.traffic_note = Some("未提供的流量显示 —；请启用 stats 及 policy.system.statsOutboundUplink / statsOutboundDownlink。累计值自核心启动或上次重置起计算。".into());
            }
        }
        Err(status) => {
            snapshot.traffic_note = Some(format!("流量读取：{}", capability_error(&status)))
        }
    }
    let mut notes = Vec::new();
    for (tag, result) in balancers {
        match result {
            Ok(response) => match response.balancer.and_then(balancer_targets) {
                Some(targets) if !targets.is_empty() => snapshot.selected_nodes.extend(targets),
                _ => notes.push(format!("{tag}：核心未返回选中节点（策略可能不支持此查询）")),
            },
            Err(status) => notes.push(format!(
                "{tag}：{}；请检查 tag 和 RoutingService",
                capability_error(&status)
            )),
        }
    }
    for (service, result) in [
        ("StatsService/GetSysStats", stats.map(|_| ())),
        ("RoutingService/ListRule", routing.map(|_| ())),
    ] {
        snapshot.services.push(match result {
            Ok(()) => format!("{service}（只读调用已验证）"),
            Err(e) => format!("{service}：{}", capability_error(&e)),
        });
    }
    match nodes {
        Ok(response) => {
            snapshot.nodes = response.outbounds.into_iter().map(decode_node).collect();
            snapshot
                .services
                .push("xray.app.proxyman.command.HandlerService（ListOutbounds 已验证）".into());
        }
        Err(status) => snapshot.nodes_note = Some(capability_error(&status)),
    }
    match observation {
        Ok(response) => {
            snapshot.observations = response
                .status
                .unwrap_or_default()
                .status
                .into_iter()
                .map(decode_observation)
                .collect();
            if snapshot
                .observations
                .iter()
                .any(|o| o.burst && o.last_try <= 0)
            {
                snapshot.observations_note = Some("Burst Observatory 未提供探测时间（核心返回 0），时间列显示“核心未提供”，无法据此比较节点的新旧；下方“最近读取”是客户端读取时间。".into());
            }
            snapshot
                .services
                .push("xray.core.app.observatory.command.ObservatoryService（已验证）".into());
        }
        Err(status) => {
            snapshot.observations_note =
                Some(observation_error(&status, discovered_services.as_deref()))
        }
    }
    // Balancer discovery supplies selection markers only; ListOutbounds defines the list.
    if let Err(error) = config {
        notes.push(format!(
            "负载均衡配置自动发现失败：{error}；无法读取选中状态，仍显示全部出站"
        ));
    }
    notes.sort();
    snapshot.balancers_note = (!notes.is_empty()).then(|| notes.join("；"));
    snapshot.nodes.sort_by(|a, b| a.tag.cmp(&b.tag));
    snapshot.services.sort();
    snapshot.services.dedup();
    Ok(snapshot)
}

pub fn inspect_blocking(address: &str) -> Result<ApiSnapshot> {
    tokio::runtime::Runtime::new()?.block_on(inspect(address))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_sort_preserves_all_outbounds_including_system_and_unselected_nodes() {
        let snapshot = ApiSnapshot {
            nodes: ["a-1", "a-2", "b-1", "direct", "override", "unused", "block"]
                .into_iter()
                .map(|tag| Node {
                    tag: tag.into(),
                    protocol: String::new(),
                    address: None,
                    port: None,
                    proxy: !matches!(tag, "direct" | "block"),
                })
                .collect(),
            selected_nodes: ["a-2".into(), "override".into()].into(),
            ..Default::default()
        };
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .map(|node| node.tag.as_str())
                .collect::<Vec<_>>(),
            ["a-1", "a-2", "b-1", "direct", "override", "unused", "block"]
        );
        let ordered = snapshot.sorted_nodes(NodeSort::Api);
        assert_eq!(
            ordered
                .iter()
                .map(|(node, _)| node.tag.as_str())
                .collect::<Vec<_>>(),
            ["a-2", "override", "a-1", "b-1", "direct", "unused", "block"]
        );
    }

    #[test]
    fn traffic_preserves_zero_missing_direction_and_exact_tags() {
        let response = QueryStatsResponse {
            stat: [
                ("outbound>>>a>>>traffic>>>uplink", 0),
                ("outbound>>>a>>>traffic>>>downlink", 2048),
                ("outbound>>>a>>>b>>>traffic>>>uplink", 42),
                ("outbound>>>negative>>>traffic>>>uplink", -1),
                ("inbound>>>a>>>traffic>>>uplink", 999),
                ("outbound>>>a>>>traffic>>>unknown", 999),
            ]
            .into_iter()
            .map(|(name, value)| Stat {
                name: name.into(),
                value,
            })
            .collect(),
        };
        let traffic = decode_traffic(
            QueryStatsResponse::decode(response.encode_to_vec().as_slice()).unwrap(),
        );
        assert_eq!(traffic.len(), 2);
        assert_eq!(traffic["a"].uplink, Some(0));
        assert_eq!(traffic["a"].downlink, Some(2048));
        assert_eq!(traffic["a>>>b"].uplink, Some(42));
        assert_eq!(traffic["a>>>b"].downlink, None);
        let request = QueryStatsRequest {
            pattern: "outbound>>>".into(),
            reset: false,
        };
        assert!(
            !QueryStatsRequest::decode(request.encode_to_vec().as_slice())
                .unwrap()
                .reset
        );
    }

    #[test]
    fn balancer_wire_fields_and_override_precedence() {
        // balancer=1, override=5, override.target=2, principle_target=6, tag=1.
        let response =
            BalancerResponse::decode(&b"\x0a\x0a\x2a\x03\x12\x01a\x32\x03\x0a\x01b"[..]).unwrap();
        assert_eq!(
            balancer_targets(response.balancer.unwrap()),
            Some(vec!["a".into()])
        );
        let candidates = BalancerInfo {
            override_target: Some(OverrideTarget {
                target: String::new(),
            }),
            principle_target: Some(PrincipleTarget {
                tag: vec!["b".into(), "c".into()],
            }),
        };
        assert_eq!(
            balancer_targets(candidates),
            Some(vec!["b".into(), "c".into()])
        );
        assert_eq!(balancer_targets(BalancerInfo::default()), None);
    }

    #[test]
    fn selected_nodes_precede_all_others_for_every_sort() {
        let mut snapshot = ApiSnapshot {
            nodes: ["fast", "chosen-slow", "chosen-missing", "slow"]
                .into_iter()
                .map(|tag| Node {
                    tag: tag.into(),
                    protocol: String::new(),
                    address: None,
                    port: None,
                    proxy: true,
                })
                .collect(),
            observations: [
                ("fast", 1, 100),
                ("chosen-slow", 500, 200),
                ("slow", 200, 300),
            ]
            .into_iter()
            .map(|(tag, delay, last_try)| Observation {
                tag: tag.into(),
                alive: true,
                delay_ms: Some(delay),
                last_try,
                burst: false,
            })
            .collect(),
            selected_nodes: ["chosen-slow".into(), "chosen-missing".into()].into(),
            ..Default::default()
        };
        for sort in [
            NodeSort::Api,
            NodeSort::Delay,
            NodeSort::DelayDescending,
            NodeSort::Newest,
            NodeSort::Oldest,
        ] {
            let rows = snapshot.sorted_nodes(sort);
            assert_eq!(rows[0].0.tag, "chosen-slow");
            assert_eq!(rows[1].0.tag, "chosen-missing");
            assert!(
                rows[2..]
                    .iter()
                    .all(|(node, _)| !snapshot.selected_nodes.contains(&node.tag))
            );
        }
        snapshot.selected_nodes = ["slow".into()].into();
        assert_eq!(snapshot.sorted_nodes(NodeSort::Delay)[0].0.tag, "slow");
    }

    #[test]
    fn decodes_core_probe_time_and_distinguishes_burst_without_a_timestamp() {
        // Official OutboundStatus wire fields: tag=4, last_try_time=6, health_ping=7.
        let ordinary =
            OutboundStatus::decode(&b"\x08\x01\x10\x2a\x22\x01a\x30\x80\xe2\xcf\xaa\x06"[..])
                .unwrap();
        let ordinary = decode_observation(ordinary);
        assert_eq!(ordinary.tag, "a");
        assert_eq!(ordinary.delay_ms, Some(42));
        assert_eq!(ordinary.last_try, 1_700_000_000);
        assert!(!ordinary.burst);
        assert!(chrono::DateTime::from_timestamp(ordinary.last_try, 0).is_some());
        // Burst includes health_ping statistics but omits both timestamp fields.
        let burst =
            OutboundStatus::decode(&b"\x08\x01\x10\x1e\x22\x01b\x3a\x04\x08\x0a\x10\x02"[..])
                .unwrap();
        let burst = decode_observation(burst);
        assert!(burst.burst);
        assert_eq!(burst.last_try, 0);
        assert_eq!(burst.delay_ms, Some(30));
    }

    #[test]
    fn each_sort_header_toggles_only_its_own_direction() {
        assert_eq!(NodeSort::Newest.toggle_delay(), NodeSort::Delay);
        assert_eq!(NodeSort::Delay.toggle_delay(), NodeSort::DelayDescending);
        assert_eq!(NodeSort::DelayDescending.toggle_delay(), NodeSort::Delay);
        assert_eq!(NodeSort::Delay.toggle_time(), NodeSort::Oldest);
        assert_eq!(NodeSort::Oldest.toggle_time(), NodeSort::Newest);
        assert_eq!(NodeSort::Newest.toggle_time(), NodeSort::Oldest);
    }

    #[test]
    fn delay_sort_keeps_unreachable_and_invalid_data_last() {
        let tags = [
            "slow", "missing", "dead", "zero", "fast", "tie", "negative", "no-delay",
        ];
        let mut snapshot = ApiSnapshot {
            services: vec![],
            discovery_note: None,
            nodes_note: None,
            observations_note: None,
            checked_at: String::new(),
            nodes: tags
                .iter()
                .map(|tag| Node {
                    tag: (*tag).into(),
                    protocol: "unknown".into(),
                    address: None,
                    port: None,
                    proxy: true,
                })
                .collect(),
            observations: [
                ("slow", true, Some(200)),
                ("dead", false, Some(1)),
                ("zero", true, Some(0)),
                ("fast", true, Some(20)),
                ("tie", true, Some(20)),
                ("negative", true, Some(-1)),
                ("no-delay", true, None),
            ]
            .into_iter()
            .map(|(tag, alive, delay_ms)| Observation {
                tag: tag.into(),
                alive,
                delay_ms,
                last_try: 100,
                burst: false,
            })
            .collect(),
            ..Default::default()
        };
        let ordered: Vec<_> = snapshot
            .sorted_nodes(NodeSort::Delay)
            .iter()
            .map(|(node, _)| node.tag.as_str())
            .collect();
        assert_eq!(
            ordered,
            [
                "zero", "fast", "tie", "slow", "missing", "dead", "negative", "no-delay"
            ]
        );
        let descending: Vec<_> = snapshot
            .sorted_nodes(NodeSort::DelayDescending)
            .iter()
            .map(|(node, _)| node.tag.as_str())
            .collect();
        assert_eq!(
            descending,
            [
                "slow", "fast", "tie", "zero", "missing", "dead", "negative", "no-delay"
            ]
        );
        snapshot.observations.clear();
        let ordered: Vec<_> = snapshot
            .sorted_nodes(NodeSort::Delay)
            .iter()
            .map(|(node, _)| node.tag.as_str())
            .collect();
        assert_eq!(ordered, tags);
        snapshot.nodes.clear();
        assert!(snapshot.sorted_nodes(NodeSort::Delay).is_empty());
    }

    #[test]
    fn observation_sort_handles_ties_missing_invalid_and_unreachable_nodes() {
        let tags = [
            "missing", "new", "zero", "old", "tie", "invalid", "negative",
        ];
        let mut snapshot = ApiSnapshot {
            services: vec![],
            discovery_note: None,
            nodes: tags
                .iter()
                .map(|tag| Node {
                    tag: (*tag).into(),
                    protocol: "unknown".into(),
                    address: None,
                    port: None,
                    proxy: true,
                })
                .collect(),
            nodes_note: None,
            observations: [
                ("old", 100),
                ("tie", 100),
                ("new", 200),
                ("zero", 0),
                ("invalid", i64::MAX),
                ("negative", -1),
            ]
            .into_iter()
            .map(|(tag, last_try)| Observation {
                tag: tag.into(),
                alive: false,
                delay_ms: None,
                last_try,
                burst: false,
            })
            .collect(),
            observations_note: None,
            checked_at: String::new(),
            ..Default::default()
        };
        let ordered = |snapshot: &ApiSnapshot, direction| {
            snapshot
                .sorted_nodes(direction)
                .iter()
                .map(|(node, _)| node.tag.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ordered(&snapshot, NodeSort::Newest),
            [
                "new", "old", "tie", "missing", "zero", "invalid", "negative"
            ]
        );
        assert_eq!(
            ordered(&snapshot, NodeSort::Oldest),
            [
                "old", "tie", "new", "missing", "zero", "invalid", "negative"
            ]
        );
        assert_eq!(ordered(&snapshot, NodeSort::Api), tags);
        snapshot.observations.clear();
        assert_eq!(ordered(&snapshot, NodeSort::Newest), tags);
        assert_eq!(ordered(&snapshot, NodeSort::Oldest), tags);
        snapshot.nodes.clear();
        assert!(snapshot.sorted_nodes(NodeSort::Newest).is_empty());
    }
    #[test]
    fn observation_diagnosis_distinguishes_unregistered_and_missing_method() {
        let error = tonic::Status::unimplemented("unknown service");
        let missing = observation_error(
            &error,
            Some(&["xray.app.stats.command.StatsService".into()]),
        );
        assert!(missing.contains("已确认当前 API 未注册"));
        assert!(missing.contains("api.services"));
        let present = observation_error(&error, Some(&[OBSERVATORY_SERVICE.into()]));
        assert!(present.contains("已注册"));
        assert!(!present.contains("未注册"));
        let unknown = observation_error(&error, None);
        assert!(unknown.contains("尚不能确定"));
    }

    #[test]
    fn observation_permissions_and_timeouts_do_not_suggest_enabling_service() {
        for error in [
            tonic::Status::permission_denied("private detail"),
            tonic::Status::deadline_exceeded("timeout"),
        ] {
            let note = observation_error(&error, Some(&[]));
            assert!(note.contains("尚未确认"));
            assert!(!note.contains("未注册"));
            assert!(!note.contains("private detail"));
        }
    }
    #[test]
    fn empty_list_services_preserves_oneof_presence_on_wire() {
        let bytes = ReflectionRequest::list_services().encode_to_vec();
        assert_eq!(bytes, [0x3a, 0x00]);
        let decoded =
            tonic_reflection::pb::v1::ServerReflectionRequest::decode(bytes.as_slice()).unwrap();
        assert!(
            matches!(decoded.message_request,Some(tonic_reflection::pb::v1::server_reflection_request::MessageRequest::ListServices(value)) if value.is_empty())
        );
    }

    async fn reflection_test_server(alpha_only: bool) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let builder = tonic_reflection::server::Builder::configure();
            if alpha_only {
                tonic::transport::Server::builder()
                    .add_service(builder.build_v1alpha().unwrap())
                    .serve_with_incoming_shutdown(incoming, async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            } else {
                tonic::transport::Server::builder()
                    .add_service(builder.build_v1().unwrap())
                    .serve_with_incoming_shutdown(incoming, async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            }
        });
        let channel = Endpoint::from_shared(format!("http://{address}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let names = reflect(channel).await.unwrap();
        let expected = if alpha_only {
            "grpc.reflection.v1alpha.ServerReflection"
        } else {
            "grpc.reflection.v1.ServerReflection"
        };
        assert!(names.iter().any(|name| name == expected));
        stop.send(()).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discovers_v1_using_standard_reflection_server() {
        reflection_test_server(false).await;
    }

    #[tokio::test]
    async fn falls_back_to_v1alpha_when_v1_is_unimplemented() {
        reflection_test_server(true).await;
    }
    #[test]
    fn decodes_endpoint_without_credentials() {
        let bytes = ProxyConfig {
            server: Some(ServerEndpoint {
                address: Some(Address {
                    domain: "example.com".into(),
                    ip: vec![],
                }),
                port: 443,
            }),
        }
        .encode_to_vec();
        let node = decode_node(Outbound {
            tag: "proxy-a".into(),
            proxy_settings: Some(TypedMessage {
                kind: "xray.proxy.vless.outbound.Config".into(),
                value: bytes,
            }),
        });
        assert_eq!(node.address.as_deref(), Some("example.com"));
        assert_eq!(node.port, Some(443));
        assert!(node.proxy);
    }
    #[test]
    fn malformed_or_new_protocol_does_not_break_list() {
        let node = decode_node(Outbound {
            tag: "new".into(),
            proxy_settings: Some(TypedMessage {
                kind: "xray.proxy.future.Config".into(),
                value: vec![255],
            }),
        });
        assert_eq!(node.protocol, "future");
        assert!(node.address.is_none());
        let direct = decode_node(Outbound {
            tag: "direct".into(),
            proxy_settings: Some(TypedMessage {
                kind: "xray.proxy.freedom.Config".into(),
                value: vec![],
            }),
        });
        assert!(!direct.proxy);
    }
    #[test]
    fn permissions_and_timeouts_are_not_reported_as_missing() {
        assert!(capability_error(&tonic::Status::permission_denied("secret")).contains("尚未确认"));
        assert!(!capability_error(&tonic::Status::internal("password")).contains("password"));
        assert!(capability_error(&tonic::Status::unimplemented("")).contains("不支持"));
    }
}
