/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-12-10 09:19:05
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:46:02
 * @FilePath: /udx710-backend/backend/src/dbus.rs
 * @Description: 
 * 
 * Copyright (c) 2025 by 1orz, All Rights Reserved. 
 */
//! D-Bus 通信模块
//! 
//! 处理与 ofono D-Bus 服务的通信

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tracing::{info, warn};
use zbus::{proxy, zvariant::OwnedValue, Connection, Proxy};

use crate::config::ConfigManager;
use crate::models::{
    AirplaneModeResponse, ApnContext, DeviceInfoResponse, NetworkInfoResponse, QosInfoResponse, RadioMode,
    RadioModeResponse, ServingCell, SimInfoResponse,
};
use crate::serial::{with_modem_read, with_modem_write, with_serial, ReadSnapshot};
use crate::state::FrontendRuntime;

type Properties = HashMap<String, OwnedValue>;
type Contexts = Vec<(zbus::zvariant::OwnedObjectPath, Properties)>;
static CONTEXT_SNAPSHOT: ReadSnapshot<Contexts> = ReadSnapshot::new();

async fn snapshot_contexts(conn: &Connection) -> zbus::Result<Arc<Contexts>> {
    CONTEXT_SNAPSHOT.get(&snapshot_scope(conn), Duration::from_secs(1), async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.ConnectionManager").await?;
        proxy.call("GetContexts", &()).await
    }).await
}

// Volatile properties are shared across a dashboard refresh; slow-changing SIM
// and settings properties also have signal invalidation and a bounded fallback TTL.
static PROPERTY_SNAPSHOTS: LazyLock<HashMap<&'static str, (Duration, ReadSnapshot<Properties>)>> = LazyLock::new(|| {
    [
        ("org.ofono.Modem", 1),
        ("org.ofono.NetworkRegistration", 1),
        ("org.ofono.ConnectionManager", 1),
        ("org.ofono.RadioSettings", 30),
        ("org.ofono.SimManager", 30),
        ("org.ofono.MessageManager", 30),
        ("org.ofono.IpMultimediaSystem", 3),
    ].into_iter().map(|(interface, seconds)| (interface, (Duration::from_secs(seconds), ReadSnapshot::new()))).collect()
});

pub static CELL_SNAPSHOT: ReadSnapshot<crate::models::CellsResponse> = ReadSnapshot::new();
pub static LOCATION_SNAPSHOT: ReadSnapshot<crate::models::CellLocationResponse> = ReadSnapshot::new();
pub static CONNECTIVITY_SNAPSHOT: ReadSnapshot<crate::models::ConnectivityCheckResponse> = ReadSnapshot::new();
static SERVING_SNAPSHOT: ReadSnapshot<ServingCell> = ReadSnapshot::new();
static QOS_SNAPSHOT: ReadSnapshot<String> = ReadSnapshot::new();
static SLOT_SNAPSHOT: ReadSnapshot<String> = ReadSnapshot::new();
static IMEISV_SNAPSHOT: ReadSnapshot<Properties> = ReadSnapshot::new();
static OPERATOR_SNAPSHOT: ReadSnapshot<Contexts> = ReadSnapshot::new();

pub fn snapshot_scope(conn: &Connection) -> String {
    conn.unique_name().map(ToString::to_string).unwrap_or_default()
}

async fn snapshot_properties(conn: &Connection, interface: &str) -> zbus::Result<Arc<Properties>> {
    let (ttl, cache) = PROPERTY_SNAPSHOTS.get(interface).expect("known dashboard interface");
    cache.get(&snapshot_scope(conn), *ttl, async {
        let fetch = async {
            let proxy = Proxy::new(conn, "org.ofono", "/ril_0", interface).await?;
            proxy.call("GetProperties", &()).await
        };
        // Cheap oFono property snapshots do not queue behind slow diagnostics.
        // SIM/IMS keep their existing recovery/backoff policy.
        if matches!(interface, "org.ofono.SimManager" | "org.ofono.MessageManager" | "org.ofono.IpMultimediaSystem") {
            with_modem_read(interface, fetch).await
        } else {
            fetch.await
        }
    }).await
}

fn invalidate_diagnostics() {
    CELL_SNAPSHOT.invalidate();
    LOCATION_SNAPSHOT.invalidate();
    SERVING_SNAPSHOT.invalidate();
    QOS_SNAPSHOT.invalidate();
    CONNECTIVITY_SNAPSHOT.invalidate();
}

fn invalidate_property_snapshot(interface: &str, property: &str) {
    if let Some((_, cache)) = PROPERTY_SNAPSHOTS.get(interface) {
        cache.invalidate();
    }
    if interface == "org.ofono.ConnectionContext" {
        CONTEXT_SNAPSHOT.invalidate();
    }
    if interface == "org.ofono.NetworkOperator" || (interface == "org.ofono.NetworkRegistration"
        && matches!(property, "Status" | "Name" | "MobileCountryCode" | "MobileNetworkCode")) {
        OPERATOR_SNAPSHOT.invalidate();
    }
    match (interface, property) {
        ("org.ofono.Modem", "Online" | "Powered" | "Interfaces") |
        ("org.ofono.SimManager", "Present" | "CardIdentifier" | "SubscriberIdentity") => {
            crate::serial::invalidate_modem_snapshots();
        }
        ("org.ofono.NetworkRegistration", "Status" | "Technology" | "CellId" | "LocationAreaCode" | "MobileCountryCode" | "MobileNetworkCode") |
        ("org.ofono.NetworkMonitor", "Technology" | "CellId" | "NCellId" | "TrackingAreaCode") |
        ("org.ofono.ConnectionManager", "Attached" | "Bearer" | "NRConnected") |
        ("org.ofono.ConnectionContext", "Active" | "AccessPointName") => invalidate_diagnostics(),
        _ => {}
    }
}

/// Listen before serving requests. Signals only invalidate snapshots; they never
/// run modem commands. TTLs still guarantee refresh if a vendor omits a signal.
pub async fn start_snapshot_invalidation(conn: &Connection) -> zbus::Result<()> {
    use futures_util::StreamExt;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.ofono")?
        .build();
    let mut stream = zbus::MessageStream::for_match_rule(rule, conn, Some(128)).await?;
    let owner_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg("org.ofono")?
        .build();
    let mut owner_stream = zbus::MessageStream::for_match_rule(owner_rule, conn, Some(8)).await?;
    tokio::spawn(async move {
        while owner_stream.next().await.is_some() {
            crate::serial::invalidate_modem_snapshots();
        }
        crate::serial::invalidate_modem_snapshots();
    });
    tokio::spawn(async move {
        while let Some(message) = stream.next().await {
            let Ok(message) = message else {
                crate::serial::invalidate_modem_snapshots();
                continue;
            };
            let header = message.header();
            let interface = header.interface().map(|name| name.as_str()).unwrap_or("");
            let member = header.member().map(|name| name.as_str()).unwrap_or("");
            let path = header.path().map(|path| path.as_str()).unwrap_or("");
            if member == "PropertyChanged" && (path == "/ril_0" || path.starts_with("/ril_0/")) {
                if let Ok((property, _)) = message.body().deserialize::<(String, OwnedValue)>() {
                    invalidate_property_snapshot(interface, &property);
                }
            } else if interface == "org.ofono.NetworkRegistration" && matches!(member, "OperatorAdded" | "OperatorRemoved") {
                OPERATOR_SNAPSHOT.invalidate();
            } else if interface == "org.ofono.ConnectionManager" && matches!(member, "ContextAdded" | "ContextRemoved") {
                CONTEXT_SNAPSHOT.invalidate();
                invalidate_diagnostics();
            } else if interface == "org.ofono.Manager" && matches!(member, "ModemAdded" | "ModemRemoved") {
                crate::serial::invalidate_modem_snapshots();
            }
        }
        crate::serial::invalidate_modem_snapshots();
        warn!("oFono snapshot signal stream ended; snapshot TTL fallback remains active");
    });
    Ok(())
}

/// ofono NetworkMonitor 代理接口
#[proxy(
    interface = "org.ofono.NetworkMonitor",
    default_service = "org.ofono",
    default_path = "/ril_0",
    assume_defaults = true
)]
pub trait NetworkMonitor {
    /// 获取服务小区信息
    fn get_serving_cell_information(
        &self,
    ) -> zbus::Result<HashMap<String, zbus::zvariant::OwnedValue>>;
}

/// ofono ConnectionContext 代理接口
#[proxy(
    interface = "org.ofono.ConnectionContext",
    default_service = "org.ofono",
    default_path = "/ril_0/context2",
    assume_defaults = true
)]
pub trait ConnectionContext {
    /// 获取连接上下文的所有属性
    fn get_properties(&self) -> zbus::Result<HashMap<String, zbus::zvariant::OwnedValue>>;
    
    /// 设置连接上下文的属性
    fn set_property(&self, name: &str, value: zbus::zvariant::Value<'_>) -> zbus::Result<()>;
}

/// ofono SimManager 代理接口
#[proxy(
    interface = "org.ofono.SimManager",
    default_service = "org.ofono",
    default_path = "/ril_0",
    assume_defaults = true
)]
pub trait SimManager {
    /// 获取SIM卡所有属性
    fn get_properties(&self) -> zbus::Result<HashMap<String, zbus::zvariant::OwnedValue>>;

    /// 通知 Modem 重新读取 SIM 状态
    fn sim_refresh(&self) -> zbus::Result<()>;
}

/// ofono MessageManager 代理接口
#[proxy(
    interface = "org.ofono.MessageManager",
    default_service = "org.ofono",
    default_path = "/ril_0",
    assume_defaults = true
)]
pub trait MessageManager {
    /// 获取消息管理器所有属性
    fn get_properties(&self) -> zbus::Result<HashMap<String, zbus::zvariant::OwnedValue>>;
}

/// ofono NetworkRegistration 代理接口
#[proxy(
    interface = "org.ofono.NetworkRegistration",
    default_service = "org.ofono",
    default_path = "/ril_0",
    assume_defaults = true
)]
pub trait NetworkRegistration {
    /// 获取网络注册所有属性
    fn get_properties(&self) -> zbus::Result<HashMap<String, zbus::zvariant::OwnedValue>>;
}

/// ofono RadioSettings 代理接口
#[proxy(
    interface = "org.ofono.RadioSettings",
    default_service = "org.ofono",
    default_path = "/ril_0",
    assume_defaults = true
)]
pub trait RadioSettings {
    /// 获取无线设置所有属性
    fn get_properties(&self) -> zbus::Result<HashMap<String, zbus::zvariant::OwnedValue>>;
    
    /// 设置无线设置属性
    fn set_property(&self, name: &str, value: zbus::zvariant::Value<'_>) -> zbus::Result<()>;
}

/// ofono Modem 代理接口
#[proxy(
    interface = "org.ofono.Modem",
    default_service = "org.ofono",
    default_path = "/ril_0",
    assume_defaults = true
)]
pub trait Modem {
    /// 获取调制解调器所有属性
    fn get_properties(&self) -> zbus::Result<HashMap<String, zbus::zvariant::OwnedValue>>;
    
    /// 设置调制解调器属性
    fn set_property(&self, name: &str, value: zbus::zvariant::Value<'_>) -> zbus::Result<()>;
}

/// 通过 D-Bus 发送 AT 指令
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `cmd` - AT 指令字符串
///
/// # Returns
/// AT 指令的响应结果
pub async fn send_at_command(conn: &Connection, cmd: &str) -> zbus::Result<String> {
    with_modem_write(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.Modem").await?;
        let result: String = proxy.call("SendAtcmd", &(cmd)).await?;
        Ok(result)
    }).await
}

/// Background AT reads share the command lock and busy backoff with SIM/cell reads.
pub async fn send_at_read_command(conn: &Connection, cmd: &str) -> zbus::Result<String> {
    with_modem_read(cmd, async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.Modem").await?;
        proxy.call("SendAtcmd", &(cmd)).await
    }).await
}

async fn require_registered_network(conn: &Connection) -> zbus::Result<()> {
    // Only registration is needed here. In particular, do not read
    // RadioSettings just to decide whether a cell/QoS query is safe.
    let modem = snapshot_properties(conn, "org.ofono.Modem").await?;
    let powered = modem.get("Powered").and_then(|value| bool::try_from(value.clone()).ok()).unwrap_or(false);
    let online = modem.get("Online").and_then(|value| bool::try_from(value.clone()).ok()).unwrap_or(false);
    let interfaces = modem.get("Interfaces").and_then(|value| Vec::<String>::try_from(value.clone()).ok());
    if !powered || !online || interfaces.as_ref().is_some_and(|items| !items.iter().any(|item| item == "org.ofono.NetworkRegistration")) {
        return Err(zbus::Error::Failure("ModemReadDeferred: waiting for network interface".into()));
    }
    let network = snapshot_properties(conn, "org.ofono.NetworkRegistration").await?;
    let status = network.get("Status").and_then(|value| <&str>::try_from(value).ok()).unwrap_or("");
    if !matches!(status, "registered" | "roaming") {
        return Err(zbus::Error::Failure("ModemReadDeferred: waiting for network registration".into()));
    }
    Ok(())
}

/// 获取服务小区信息
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// 服务小区信息结构
pub async fn get_serving_cell_info(conn: &Connection) -> zbus::Result<ServingCell> {
    require_registered_network(conn).await?;
    let snapshot = SERVING_SNAPSHOT.get(&snapshot_scope(conn), Duration::from_secs(3), with_modem_read("serving-cell", async {
        let proxy = NetworkMonitorProxy::new(conn).await?;
        let cell_info: HashMap<String, OwnedValue> = proxy.get_serving_cell_information().await?;

        let tech = cell_info
            .get("Technology")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "unknown".to_string());
        if tech.is_empty() || tech.eq_ignore_ascii_case("unknown") {
            return Err(zbus::Error::Failure("ModemReadDeferred: waiting for serving cell technology".into()));
        }

        let cell_id = parse_u32_from_keys(&cell_info, &["NCellId", "CellId", "NRCellID"]);
        let tac = parse_u32_from_keys(&cell_info, &["TrackingAreaCode"]);

        Ok(ServingCell { tech, cell_id, tac })
    })).await?;
    Ok(snapshot.as_ref().clone())
}

/// 查找第一个有效的 internet 类型 context 路径
///
/// 遍历所有 context，返回第一个类型为 internet 且配置了 APN 的 context 路径。
/// 如果没有配置 APN 的 context，则返回第一个 internet 类型的 context。
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// context 路径字符串
pub async fn find_internet_context(conn: &Connection) -> zbus::Result<String> {
    let contexts = snapshot_contexts(conn).await?;
    
    let mut first_internet_context: Option<String> = None;
    
    for (path, props) in contexts.iter() {
        let context_type = props
            .get("Type")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_default();
        
        if context_type == "internet" {
            let apn = props
                .get("AccessPointName")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default();
            
            // 如果配置了 APN，优先返回这个 context
            if !apn.is_empty() {
                return Ok(path.to_string());
            }
            
            // 记录第一个 internet 类型的 context
            if first_internet_context.is_none() {
                first_internet_context = Some(path.to_string());
            }
        }
    }
    
    // 返回第一个 internet context，如果没有则返回默认值
    Ok(first_internet_context.unwrap_or_else(|| "/ril_0/context2".to_string()))
}

/// 获取所有 APN Context 列表
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// APN Context 列表
pub async fn get_all_apn_contexts(conn: &Connection) -> zbus::Result<Vec<ApnContext>> {
    let contexts = snapshot_contexts(conn).await?;
    
    let mut result = Vec::new();
    
    for (path, props) in contexts.iter() {
        let context_type = props
            .get("Type")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_default();
        
        // 只返回 internet 类型的 context
        if context_type == "internet" {
            let apn_context = ApnContext {
                path: path.to_string(),
                name: props
                    .get("Name")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_else(|| "Internet".to_string()),
                active: props
                    .get("Active")
                    .and_then(|v| bool::try_from(v.clone()).ok())
                    .unwrap_or(false),
                apn: props
                    .get("AccessPointName")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_default(),
                protocol: props
                    .get("Protocol")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_else(|| "ip".to_string()),
                username: props
                    .get("Username")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_default(),
                password: props
                    .get("Password")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_default(),
                auth_method: props
                    .get("AuthenticationMethod")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_else(|| "chap".to_string()),
                context_type,
            };
            result.push(apn_context);
        }
    }
    
    Ok(result)
}

/// 设置 APN 属性
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `context_path` - context 的 D-Bus 路径
/// * `property` - 属性名
/// * `value` - 属性值
///
/// # Returns
/// 操作结果
pub async fn set_apn_property(
    conn: &Connection, 
    context_path: &str, 
    property: &str, 
    value: &str
) -> zbus::Result<()> {
    with_modem_write(async {
        let proxy = ConnectionContextProxy::builder(conn)
            .path(context_path)?
            .build()
            .await?;
        
        proxy.set_property(property, zbus::zvariant::Value::Str(value.into())).await?;
        Ok(())
    }).await
}

/// 批量设置 APN 属性
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `context_path` - context 的 D-Bus 路径
/// * `apn` - APN 名称（可选）
/// * `protocol` - 协议（可选）
/// * `username` - 用户名（可选）
/// * `password` - 密码（可选）
/// * `auth_method` - 认证方式（可选）
///
/// # Returns
/// 操作结果
pub async fn set_apn_properties(
    conn: &Connection,
    context_path: &str,
    apn: Option<&str>,
    protocol: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
    auth_method: Option<&str>,
) -> zbus::Result<()> {
    // 先检查 context 是否激活，如果激活需要先关闭
    let proxy = ConnectionContextProxy::builder(conn)
        .path(context_path)?
        .build()
        .await?;
    
    let props = proxy.get_properties().await?;
    let was_active = props
        .get("Active")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    
    // 如果 context 是激活状态，先关闭它
    if was_active {
        with_modem_write(async {
            let proxy = ConnectionContextProxy::builder(conn)
                .path(context_path)?
                .build()
                .await?;
            proxy.set_property("Active", zbus::zvariant::Value::Bool(false)).await?;
            Ok::<(), zbus::Error>(())
        }).await?;
        
        // 等待一下让状态稳定
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
    
    // 设置各个属性
    if let Some(apn_val) = apn {
        set_apn_property(conn, context_path, "AccessPointName", apn_val).await?;
    }
    
    if let Some(protocol_val) = protocol {
        set_apn_property(conn, context_path, "Protocol", protocol_val).await?;
    }
    
    if let Some(username_val) = username {
        set_apn_property(conn, context_path, "Username", username_val).await?;
    }
    
    if let Some(password_val) = password {
        set_apn_property(conn, context_path, "Password", password_val).await?;
    }
    
    if let Some(auth_method_val) = auth_method {
        set_apn_property(conn, context_path, "AuthenticationMethod", auth_method_val).await?;
    }
    
    // 如果之前是激活状态，重新激活
    if was_active {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        with_modem_write(async {
            let proxy = ConnectionContextProxy::builder(conn)
                .path(context_path)?
                .build()
                .await?;
            proxy.set_property("Active", zbus::zvariant::Value::Bool(true)).await?;
            Ok::<(), zbus::Error>(())
        }).await?;
    }
    
    Ok(())
}

/// 设置数据连接状态
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `active` - true 开启数据流量，false 关闭数据流量
///
/// # Returns
/// 操作结果
pub async fn set_data_connection(conn: &Connection, active: bool) -> zbus::Result<()> {
    with_modem_write(async {
        // 自动查找有效的 internet context
        let context_path = find_internet_context(conn).await?;
        
        let proxy = ConnectionContextProxy::builder(conn)
            .path(context_path)?
            .build()
            .await?;
        proxy.set_property("Active", zbus::zvariant::Value::Bool(active)).await?;
        Ok(())
    }).await
}

/// 获取数据连接状态
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// 数据连接是否激活
pub async fn get_data_connection_status(conn: &Connection) -> zbus::Result<bool> {
    let contexts = snapshot_contexts(conn).await?;
    // GetContexts already includes Active. Preserve the configured-APN priority
    // used by find_internet_context without a second GetProperties round trip.
    let internet = |props: &Properties| props.get("Type").and_then(|v| <&str>::try_from(v).ok()) == Some("internet");
    let selected = contexts.iter().find(|(_, props)| internet(props) && props.get("AccessPointName")
        .and_then(|v| <&str>::try_from(v).ok()).is_some_and(|apn| !apn.is_empty()))
        .or_else(|| contexts.iter().find(|(_, props)| internet(props)));
    let properties = if let Some((_, props)) = selected {
        props
    } else {
        // Retain the legacy context2 fallback when no internet context exists.
        let proxy = ConnectionContextProxy::new(conn).await?;
        return Ok(proxy.get_properties().await?.get("Active")
            .and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false));
    };
    
    let active = properties
        .get("Active")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    
    Ok(active)
}

/// 获取漫游状态
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// (roaming_allowed, is_roaming) 元组
pub async fn get_roaming_status(conn: &Connection) -> zbus::Result<(bool, bool)> {
    // 获取 ConnectionManager 的 RoamingAllowed 属性
    let cm_props = snapshot_properties(conn, "org.ofono.ConnectionManager").await?;
    
    let roaming_allowed = cm_props
        .get("RoamingAllowed")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    
    // 获取 NetworkRegistration 的 Status 属性判断是否漫游
    let net_props = snapshot_properties(conn, "org.ofono.NetworkRegistration").await?;
    
    let status = net_props
        .get("Status")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "unknown".to_string());
    
    let is_roaming = status == "roaming";
    
    Ok((roaming_allowed, is_roaming))
}

/// 设置漫游开关
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `allowed` - true 允许漫游数据，false 禁止漫游数据
///
/// # Returns
/// 操作结果
pub async fn set_roaming_allowed(conn: &Connection, allowed: bool) -> zbus::Result<()> {
    with_modem_write(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.ConnectionManager").await?;
        let value = zbus::zvariant::Value::Bool(allowed);
        proxy.call::<_, _, ()>("SetProperty", &("RoamingAllowed", value)).await?;
        Ok(())
    }).await
}

/// 初始化数据连接（程序启动时调用）
///
/// 检查当前数据连接状态，如果未激活则尝试自动激活。
/// 这个函数会在后台静默执行，不会阻塞服务启动。
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// 初始化结果消息
pub async fn init_data_connection(conn: &Connection) -> String {
    // 1. 先检查网络注册状态
    match NetworkRegistrationProxy::new(conn).await {
        Ok(net_proxy) => {
            if let Ok(props) = net_proxy.get_properties().await {
                let status = props
                    .get("Status")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_else(|| "unknown".to_string());
                
                if status != "registered" && status != "roaming" {
                    return format!("Network not registered (status: {}), skipping data connection", status);
                }
            }
        }
        Err(e) => {
            return format!("Failed to check network status: {}", e);
        }
    }
    
    // 2. 自动查找有效的 internet context
    let context_path = match find_internet_context(conn).await {
        Ok(path) => path,
        Err(e) => {
            return format!("Failed to find internet context: {}", e);
        }
    };
    
    // 3. 获取 context 的属性
    let proxy = match ConnectionContextProxy::builder(conn)
        .path(context_path.as_str())
        .and_then(|b| Ok(b))
    {
        Ok(builder) => match builder.build().await {
            Ok(p) => p,
            Err(e) => return format!("Failed to create context proxy: {}", e),
        },
        Err(e) => return format!("Failed to build context path: {}", e),
    };
    
    let props = match proxy.get_properties().await {
        Ok(p) => p,
        Err(e) => return format!("Failed to get context properties: {}", e),
    };
    
    // 4. 检查是否已激活
    let active = props
        .get("Active")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    
    if active {
        return format!("Data connection already active ({})", context_path);
    }
    
    // 5. 检查 APN 是否配置
    let apn = props
        .get("AccessPointName")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();
    
    if apn.is_empty() {
        return format!("APN not configured on {}, skipping auto-connect", context_path);
    }
    
    // 6. 尝试激活数据连接
    match set_data_connection(conn, true).await {
        Ok(_) => format!("Data connection activated on {} (APN: {})", context_path, apn),
        Err(e) => format!("Failed to activate data connection: {}", e),
    }
}

/// 根据 MCC/MNC 获取推荐的 APN 配置
///
/// # Arguments
/// * `mcc` - 移动国家代码
/// * `mnc` - 移动网络代码
///
/// # Returns
/// (apn, protocol) 元组，如果未找到则返回 None
fn get_recommended_apn(mcc: &str, mnc: &str) -> Option<(&'static str, &'static str)> {
    match (mcc, mnc) {
        // 中国移动 (46000, 46002, 46007, 46008)
        ("460", "00") | ("460", "02") | ("460", "07") | ("460", "08") => Some(("cmnet", "dual")),
        // 中国联通 (46001, 46006, 46009)
        ("460", "01") | ("460", "06") | ("460", "09") => Some(("3gnet", "dual")),
        // 中国电信 (46003, 46005, 46011)
        ("460", "03") | ("460", "05") | ("460", "11") => Some(("ctnet", "dual")),
        // 中国广电 (46015)
        ("460", "15") => Some(("cbnet", "dual")),
        _ => None,
    }
}

/// 自动配置 APN（根据 SIM 卡运营商）
///
/// 根据 SIM 卡的 MCC/MNC 自动查找并设置推荐的 APN 配置
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `context_path` - 要配置的 context 路径
///
/// # Returns
/// 配置结果消息
async fn auto_configure_apn(conn: &Connection, context_path: &str) -> Result<String, String> {
    // 1. 获取网络注册信息中的 MCC/MNC
    let net_proxy = NetworkRegistrationProxy::new(conn)
        .await
        .map_err(|e| format!("Failed to create network proxy: {}", e))?;
    
    let props = net_proxy
        .get_properties()
        .await
        .map_err(|e| format!("Failed to get network properties: {}", e))?;
    
    let mcc = props
        .get("MobileCountryCode")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();
    
    let mnc = props
        .get("MobileNetworkCode")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();
    
    if mcc.is_empty() || mnc.is_empty() {
        return Err("MCC/MNC not available".to_string());
    }
    
    // 2. 查找推荐 APN
    let (apn, protocol) = get_recommended_apn(&mcc, &mnc)
        .ok_or_else(|| format!("No recommended APN for MCC={} MNC={}", mcc, mnc))?;
    
    // 3. 设置 APN 和协议
    set_apn_property(conn, context_path, "AccessPointName", apn)
        .await
        .map_err(|e| format!("Failed to set APN: {}", e))?;
    
    set_apn_property(conn, context_path, "Protocol", protocol)
        .await
        .map_err(|e| format!("Failed to set protocol: {}", e))?;
    
    Ok(format!("Auto-configured APN: {} ({})", apn, protocol))
}

/// 检查并恢复数据连接
///
/// 这个函数被 watchdog 调用，检查数据连接状态并在需要时恢复
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// 当前状态描述字符串
async fn check_and_restore_data_connection(conn: &Connection) -> String {
    // 1. 检查网络注册状态
    let net_status = match NetworkRegistrationProxy::new(conn).await {
        Ok(net_proxy) => {
            match net_proxy.get_properties().await {
                Ok(props) => props
                    .get("Status")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_else(|| "unknown".to_string()),
                Err(_) => "unknown".to_string(),
            }
        }
        Err(_) => return "Network proxy unavailable".to_string(),
    };
    
    // 网络未注册时不尝试恢复
    if net_status != "registered" && net_status != "roaming" {
        return format!("Waiting for network (status: {})", net_status);
    }
    
    // 2. 查找 internet context
    let context_path = match find_internet_context(conn).await {
        Ok(path) => path,
        Err(e) => return format!("No internet context: {}", e),
    };
    
    // 3. 获取 context 属性
    let proxy = match ConnectionContextProxy::builder(conn)
        .path(context_path.as_str())
        .and_then(|b| Ok(b))
    {
        Ok(builder) => match builder.build().await {
            Ok(p) => p,
            Err(e) => return format!("Context proxy error: {}", e),
        },
        Err(e) => return format!("Context path error: {}", e),
    };
    
    let props = match proxy.get_properties().await {
        Ok(p) => p,
        Err(e) => return format!("Get properties error: {}", e),
    };
    
    let apn = props
        .get("AccessPointName")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();
    
    let active = props
        .get("Active")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    
    // 4. 如果 APN 为空，尝试自动配置
    if apn.is_empty() {
        match auto_configure_apn(conn, &context_path).await {
            Ok(msg) => {
                // APN 配置成功后，继续尝试激活
                match set_data_connection(conn, true).await {
                    Ok(_) => return format!("{}, connection activated", msg),
                    Err(e) => return format!("{}, but activation failed: {}", msg, e),
                }
            }
            Err(e) => return format!("APN not configured: {}", e),
        }
    }
    
    // 5. 如果连接未激活，尝试激活
    if !active {
        match set_data_connection(conn, true).await {
            Ok(_) => return format!("Connection restored (APN: {})", apn),
            Err(e) => return format!("Activation failed: {}", e),
        }
    }
    
    // 6. 连接正常
    format!("Connected (APN: {})", apn)
}

/// 数据连接 Watchdog - 后台轮询监控并自动恢复
///
/// 持续监控数据连接状态，在断开时自动尝试恢复。
/// 支持自动识别运营商并配置 APN。
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `interval_secs` - 检查间隔（秒）
pub async fn data_connection_watchdog(
    conn: Arc<Connection>,
    config_manager: Arc<ConfigManager>,
    frontend_runtime: Arc<FrontendRuntime>,
) {
    use crate::iptables::{flush_iptables, get_iptables_rule_count};
    
    let mut last_data_log = String::new();
    let mut last_iptables_action = false; // 上次是否清空了 iptables
    
    loop {
        let refresh = config_manager.get_refresh();
        let heartbeat_timeout = Duration::from_millis(refresh.heartbeat_timeout_ms());
        let interval = if frontend_runtime.is_recent(heartbeat_timeout) {
            Duration::from_millis(refresh.active_watchdog_interval_ms())
        } else {
            Duration::from_millis(refresh.idle_watchdog_interval_ms())
        };

        tokio::time::sleep(interval).await;
        
        // 1. 检查并清空 iptables 规则
        match get_iptables_rule_count().await {
            Ok(count) => {
                if count.has_rules() {
                    // 有规则，执行清空
                    if let Err(e) = flush_iptables().await {
                        warn!(error = %e, "Watchdog: iptables flush failed");
                    } else {
                        if !last_iptables_action {
                            // 只在首次清空时打印日志
                            info!(
                                total = count.total(),
                                ipv4 = count.ipv4_rules,
                                ipv6 = count.ipv6_rules,
                                "Watchdog: iptables flushed"
                            );
                        }
                        last_iptables_action = true;
                    }
                } else {
                    // 无规则，重置标志
                    last_iptables_action = false;
                }
            }
            Err(e) => {
                warn!(error = %e, "Watchdog: iptables check failed");
            }
        }
        
        // 2. 检查并恢复数据连接
        let result = check_and_restore_data_connection(&conn).await;
        
        // 只在状态变化时打印日志，避免刷屏
        if result != last_data_log {
            info!(status = %result, "Watchdog: data connection");
            last_data_log = result;
        }
    }
}

/// 获取 SIM 卡信息（整合所有 SIM 相关信息）
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// SIM 卡信息结构（整合 SimManager + MessageManager）
pub async fn get_sim_info_data(conn: &Connection) -> zbus::Result<SimInfoResponse> {
    let sim_props = snapshot_properties(conn, "org.ofono.SimManager").await?;
    let msg_props = snapshot_properties(conn, "org.ofono.MessageManager").await?;

    // 基本状态
    let present = sim_props
        .get("Present")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);

    // ICCID
    let iccid = sim_props
        .get("CardIdentifier")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    // IMSI
    let imsi = sim_props
        .get("SubscriberIdentity")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    // 手机号码列表
    let phone_numbers: Vec<String> = sim_props
        .get("SubscriberNumbers")
        .and_then(|v| <Vec<String>>::try_from(v.clone()).ok())
        .unwrap_or_default();

    // 短信中心
    let sms_center = msg_props
        .get("ServiceCenterAddress")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    // MCC/MNC
    let mcc = sim_props
        .get("MobileCountryCode")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    let mnc = sim_props
        .get("MobileNetworkCode")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    // PIN 状态
    let pin_required = sim_props
        .get("PinRequired")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "none".to_string());

    // 首选语言
    let preferred_languages: Vec<String> = sim_props
        .get("PreferredLanguages")
        .and_then(|v| <Vec<String>>::try_from(v.clone()).ok())
        .unwrap_or_default();

    Ok(SimInfoResponse {
        present,
        iccid,
        imsi,
        phone_numbers,
        sms_center,
        mcc,
        mnc,
        pin_required,
        preferred_languages,
    })
}

/// 请求 Modem 刷新当前 SIM/eUICC 状态
pub async fn refresh_sim(conn: &Connection) -> zbus::Result<()> {
    let _modem_change = crate::serial::modem_change();
    with_serial(async {
        let proxy = SimManagerProxy::new(conn).await?;
        proxy.sim_refresh().await
    })
    .await
}

/// 读取当前注册网络的 MCC（不走全局锁）
pub async fn get_registered_mcc(conn: &Connection) -> Option<String> {
    let props = snapshot_properties(conn, "org.ofono.NetworkRegistration").await.ok()?;
    props
        .get("MobileCountryCode")
        .and_then(|value| String::try_from(value.clone()).ok())
        .filter(|mcc| !mcc.is_empty())
}

/// 获取网络信息
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// 网络信息结构
pub async fn get_network_info_data(conn: &Connection) -> zbus::Result<NetworkInfoResponse> {
    let props = snapshot_properties(conn, "org.ofono.Modem").await?;
    let powered = props.get("Powered").and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false);
    let online = props.get("Online").and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false);
    if !powered || !online {
        return Ok(NetworkInfoResponse {
            registration_status: if powered { "offline" } else { "powered-off" }.into(),
            ..Default::default()
        });
    }
    let interfaces = props.get("Interfaces")
        .and_then(|v| Vec::<String>::try_from(v.clone()).ok());
    if interfaces.as_ref().is_some_and(|items| !items.iter().any(|item| item == "org.ofono.NetworkRegistration")) {
        return Ok(NetworkInfoResponse { registration_status: "initializing".into(), ..Default::default() });
    }
    let net_props = match snapshot_properties(conn, "org.ofono.NetworkRegistration").await {
        Ok(properties) => properties,
        Err(error) if error.to_string().contains("UnknownMethod") || error.to_string().contains("UnknownInterface") => {
            // Interfaces may disappear between the Modem snapshot and this call.
            return Err(zbus::Error::Failure("ModemReadDeferred: network interface is restarting".into()));
        }
        Err(error) => return Err(error),
    };
    let radio_props = if interfaces.as_ref().is_none_or(|items| items.iter().any(|item| item == "org.ofono.RadioSettings")) {
        match snapshot_properties(conn, "org.ofono.RadioSettings").await {
            Ok(properties) => properties,
            Err(error) if error.to_string().contains("UnknownMethod") || error.to_string().contains("UnknownInterface") => Arc::new(HashMap::new()),
            Err(error) => return Err(error),
        }
    } else { Arc::new(HashMap::new()) };

    let operator_name = net_props
        .get("Name")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    let registration_status = net_props
        .get("Status")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "unknown".to_string());

    let technology_preference = radio_props
        .get("TechnologyPreference")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    let signal_strength = net_props
        .get("Strength")
        .and_then(|v| u8::try_from(v.clone()).ok())
        .unwrap_or(0);

    let mcc = net_props
        .get("MobileCountryCode")
        .and_then(|v| String::try_from(v.clone()).ok());

    let mnc = net_props
        .get("MobileNetworkCode")
        .and_then(|v| String::try_from(v.clone()).ok());

    Ok(NetworkInfoResponse {
        operator_name,
        registration_status,
        technology_preference,
        technology: net_props.get("Technology").and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default(),
        signal_strength,
        mcc,
        mnc,
    })
}

/// 获取设备信息（来自 D-Bus Modem 接口）
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// 设备信息结构
pub async fn get_device_info_data(conn: &Connection) -> zbus::Result<DeviceInfoResponse> {
    let props = snapshot_properties(conn, "org.ofono.Modem").await?;

    let imei = props
        .get("Serial")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    let manufacturer = props
        .get("Manufacturer")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    let model = props
        .get("Model")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();

    let revision = props
        .get("Revision")
        .and_then(|v| String::try_from(v.clone()).ok());

    let online = props
        .get("Online")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);

    let powered = props
        .get("Powered")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);

    Ok(DeviceInfoResponse {
        imei,
        manufacturer,
        model,
        revision,
        online,
        powered,
    })
}

/// 获取QoS信息
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// QoS信息结构
pub async fn get_qos_info_data(conn: &Connection) -> zbus::Result<QosInfoResponse> {
    require_registered_network(conn).await?;
    let response = QOS_SNAPSHOT.get(&snapshot_scope(conn), Duration::from_secs(5),
        send_at_read_command(conn, "AT+CGEQOSRDP")).await?;
    
    // 解析 +CGEQOSRDP: <cid>,<QCI>,[<DL_GBR>,<UL_GBR>],[<DL_MBR>,<UL_MBR>],[<DL_AMBR>,<UL_AMBR>]
    let parsed = parse_qos_response(&response);
    
    Ok(parsed)
}

/// 解析QoS响应
///
/// 格式: +CGEQOSRDP: <cid>,<QCI>,[<DL_GBR>,<UL_GBR>],[<DL_MBR>,<UL_MBR>],[<DL_AMBR>,<UL_AMBR>]
/// 示例: +CGEQOSRDP: 11,5,0,0,0,0,30000,30000
fn parse_qos_response(response: &str) -> QosInfoResponse {
    // 查找 +CGEQOSRDP: 开头的行
    for line in response.lines() {
        let line = line.trim();
        if line.starts_with("+CGEQOSRDP:") {
            // 提取冒号后面的部分
            if let Some(data) = line.strip_prefix("+CGEQOSRDP:") {
                let parts: Vec<&str> = data.trim().split(',').collect();
                
                if parts.len() >= 8 {
                    // 解析各个字段
                    let qci = parts.get(1).and_then(|s| s.trim().parse::<u8>().ok()).unwrap_or(0);
                    let dl_gbr = parts.get(2).and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
                    let ul_gbr = parts.get(3).and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
                    let dl_mbr = parts.get(4).and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
                    let ul_mbr = parts.get(5).and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
                    let dl_ambr = parts.get(6).and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
                    let ul_ambr = parts.get(7).and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
                    
                    // 优先使用 GBR，如果为0则使用 MBR，如果还是0则使用 AMBR
                    let dl_speed = if dl_gbr > 0 { dl_gbr } else if dl_mbr > 0 { dl_mbr } else { dl_ambr };
                    let ul_speed = if ul_gbr > 0 { ul_gbr } else if ul_mbr > 0 { ul_mbr } else { ul_ambr };
                    
                    return QosInfoResponse {
                        qci,
                        dl_speed,
                        ul_speed,
                        raw_response: None, // 不返回原始响应，保持简洁
                    };
                }
            }
        }
    }
    
    // 如果解析失败，返回默认值
    QosInfoResponse {
        qci: 0,
        dl_speed: 0,
        ul_speed: 0,
        raw_response: Some(response.to_string()),
    }
}

/// 从多个可能的键中解析 u32 值
///
/// 不同的 udx710 设备可能使用不同的键名和值类型
fn parse_u32_from_keys(cell_info: &HashMap<String, OwnedValue>, keys: &[&str]) -> u32 {
    for key in keys {
        if let Some(value) = cell_info.get(*key) {
            // 尝试直接转换为 u32
            if let Ok(num) = u32::try_from(value) {
                return num;
            }
            // 尝试转换为字符串后再解析
            if let Ok(s) = String::try_from(value.clone()) {
                // 尝试十进制解析
                if let Ok(num) = s.parse::<u32>() {
                    return num;
                }
                // 尝试十六进制解析
                if let Ok(num) = u32::from_str_radix(&s, 16) {
                    return num;
                }
            }
        }
    }
    0
}

/// 设置飞行模式
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `enabled` - true 开启飞行模式（关闭射频），false 关闭飞行模式（开启射频）
///
/// # Returns
/// 操作结果
///
/// # 说明
/// 飞行模式通过设置 Modem 的 Online 属性实现：
/// - Online = false: 关闭射频，进入飞行模式（但 Modem 保持上电）
/// - Online = true: 开启射频，退出飞行模式
pub async fn set_airplane_mode(conn: &Connection, enabled: bool) -> zbus::Result<AirplaneModeResponse> {
    let _modem_change = crate::serial::modem_change();
    // Include time queued behind another command and bus setup in the limit.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let work = with_serial(async {
        // Give this write its own longer D-Bus deadline. Other dashboard calls
        // keep their usual timeout and stay deferred until verification ends.
        let write_conn = tokio::time::timeout(Duration::from_secs(5),
            zbus::connection::Builder::system()?.method_timeout(Duration::from_secs(60)).build())
            .await.map_err(|_| zbus::Error::Failure("无法连接 oFono 系统总线".into()))??;
        let write = async {
            let proxy = ModemProxy::new(&write_conn).await?;
            proxy.set_property("Online", zbus::zvariant::Value::Bool(!enabled)).await
        };
        crate::serial::confirm_modem_state(write, || async {
            // Bypass snapshots: confirmation must inspect a fresh, explicit
            // Online boolean, not an absent property's false default.
            let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.Modem").await?;
            let props: Properties = proxy.call("GetProperties", &()).await?;
            let online = props.get("Online").and_then(|v| bool::try_from(v).ok());
            if online != Some(!enabled) { return Ok(None); }
            let powered = props.get("Powered").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
            Ok(Some(AirplaneModeResponse { enabled, powered, online: !enabled }))
        }, Duration::from_secs(60), Duration::from_secs(10)).await
    });
    tokio::time::timeout_at(deadline, work).await.map_err(|_| zbus::Error::Failure(
        "飞行模式切换等待超时（60 秒），设备仍可能继续处理，请稍后检查状态。".into()))?
}

/// 获取飞行模式状态
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// 飞行模式响应结构，包含飞行模式状态、Powered 和 Online 属性
///
/// # 说明
/// 飞行模式状态判断：
/// - enabled = !Online (Online=false 表示飞行模式已启用)
pub async fn get_airplane_mode(conn: &Connection) -> zbus::Result<AirplaneModeResponse> {
    let props = snapshot_properties(conn, "org.ofono.Modem").await?;
    
    let powered = props
        .get("Powered")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    
    let online = props
        .get("Online")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    
    // 飞行模式状态：Online=false 表示飞行模式已启用
    let enabled = !online;
    
    Ok(AirplaneModeResponse {
        enabled,
        powered,
        online,
    })
}

/// 获取射频模式
///
/// # Arguments
/// * `conn` - D-Bus 连接
///
/// # Returns
/// 射频模式响应结构
///
/// # 说明
/// 通过 RadioSettings.GetProperties 获取 TechnologyPreference 属性
pub async fn get_radio_mode(conn: &Connection) -> zbus::Result<RadioModeResponse> {
    let props = snapshot_properties(conn, "org.ofono.RadioSettings").await?;

    let technology_preference = props
        .get("TechnologyPreference")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "unknown".to_string());

    // 尝试映射为标准模式
    let mode = RadioMode::from_ofono_value(&technology_preference)
        .map(|m| match m {
            RadioMode::Auto => "auto",
            RadioMode::LteOnly => "lte",
            RadioMode::NrOnly => "nr",
        })
        .unwrap_or("unknown")
        .to_string();

    Ok(RadioModeResponse {
        mode,
        technology_preference,
    })
}

/// 设置射频模式
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `mode` - 目标射频模式
///
/// # Returns
/// 操作结果
///
/// # 说明
/// 通过 RadioSettings.SetProperty 设置 TechnologyPreference 属性
pub async fn set_radio_mode(conn: &Connection, mode: RadioMode) -> zbus::Result<()> {
    with_modem_write(async {
        let proxy = RadioSettingsProxy::new(conn).await?;
        let ofono_value = mode.to_ofono_value();
        
        proxy
            .set_property(
                "TechnologyPreference",
                zbus::zvariant::Value::Str(ofono_value.into()),
            )
            .await?;
        
        Ok(())
    }).await
}

// ============ 电话相关 D-Bus 接口 ============

use crate::models::CallInfo;

/// ofono VoiceCallManager 代理接口
#[proxy(
    interface = "org.ofono.VoiceCallManager",
    default_service = "org.ofono",
    default_path = "/ril_0",
    assume_defaults = true
)]
pub trait VoiceCallManager {
    /// 获取所有通话
    fn get_calls(&self) -> zbus::Result<Vec<(zbus::zvariant::OwnedObjectPath, HashMap<String, OwnedValue>)>>;
    
    /// 拨打电话
    fn dial(&self, number: &str, hide_callerid: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    
    /// 挂断所有通话
    fn hangup_all(&self) -> zbus::Result<()>;
}

/// ofono VoiceCall 代理接口（单个通话）
#[proxy(
    interface = "org.ofono.VoiceCall",
    default_service = "org.ofono",
    assume_defaults = true
)]
pub trait VoiceCall {
    /// 挂断此通话
    fn hangup(&self) -> zbus::Result<()>;
    
    /// 接听来电
    fn answer(&self) -> zbus::Result<()>;
    
    /// 获取通话属性
    fn get_properties(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

/// 获取当前活动的通话列表
pub async fn get_active_calls(conn: &Connection) -> zbus::Result<Vec<CallInfo>> {
    with_serial(async {
        let proxy = VoiceCallManagerProxy::new(conn).await?;
        let calls = proxy.get_calls().await?;
        
        let mut result = Vec::new();
        for (path, props) in calls {
            let phone_number = props
                .get("LineIdentification")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_else(|| "Unknown".to_string());
            
            let state = props
                .get("State")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_else(|| "unknown".to_string());
            
            let start_time = props
                .get("StartTime")
                .and_then(|v| String::try_from(v.clone()).ok());
            
            // 判断方向：incoming 或 outgoing
            let direction = if state == "incoming" {
                "incoming".to_string()
            } else {
                "outgoing".to_string()
            };
            
            result.push(CallInfo {
                path: path.to_string(),
                phone_number,
                state,
                direction,
                start_time,
            });
        }
        
        Ok(result)
    }).await
}

/// 拨打电话
pub async fn dial_call(conn: &Connection, phone_number: &str) -> zbus::Result<CallInfo> {
    with_serial(async {
        let proxy = VoiceCallManagerProxy::new(conn).await?;
        let path = proxy.dial(phone_number, "default").await?;
        
        Ok(CallInfo {
            path: path.to_string(),
            phone_number: phone_number.to_string(),
            state: "dialing".to_string(),
            direction: "outgoing".to_string(),
            start_time: Some(chrono::Utc::now().to_rfc3339()),
        })
    }).await
}

/// 挂断指定通话
pub async fn hangup_call(conn: &Connection, call_path: &str) -> zbus::Result<()> {
    with_serial(async {
        let proxy = VoiceCallProxy::builder(conn)
            .path(call_path)?
            .build()
            .await?;
        
        proxy.hangup().await
    }).await
}

/// 挂断所有通话
pub async fn hangup_all_calls(conn: &Connection) -> zbus::Result<usize> {
    with_serial(async {
        let proxy = VoiceCallManagerProxy::new(conn).await?;
        let calls = proxy.get_calls().await?;
        let count = calls.len();
        
        if count > 0 {
            proxy.hangup_all().await?;
        }
        
        Ok(count)
    }).await
}

/// 接听来电
pub async fn answer_call(conn: &Connection, call_path: &str) -> zbus::Result<()> {
    with_serial(async {
        let proxy = VoiceCallProxy::builder(conn)
            .path(call_path)?
            .build()
            .await?;
        
        proxy.answer().await
    }).await
}

// ============ 短信相关 D-Bus 接口 ============

/// 发送短信
pub async fn send_sms(conn: &Connection, phone_number: &str, content: &str) -> zbus::Result<String> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.MessageManager").await?;
        let message_path: zbus::zvariant::OwnedObjectPath = proxy.call("SendMessage", &(phone_number, content)).await?;
        Ok(message_path.to_string())
    }).await
}

// ============ 新增功能接口 ============

use crate::models::{
    ImeisvResponse, SignalStrengthResponse, CallForwardingResponse, CallSettingsResponse,
    OperatorInfo, OperatorListResponse, NitzTimeResponse, ImsStatusResponse,
    CallVolumeResponse, VoicemailStatusResponse,
};

/// 获取 IMEISV（软件版本号）
pub async fn get_imeisv(conn: &Connection) -> zbus::Result<ImeisvResponse> {
    let result = IMEISV_SNAPSHOT.get(&snapshot_scope(conn), Duration::from_secs(300), with_modem_read("imeisv", async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.Modem").await?;
        proxy.call("GetImeisv", &()).await
    })).await?;

    let svn = result
        .get("SoftwareVersionNumber")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "Unknown".to_string());

    Ok(ImeisvResponse {
        software_version_number: svn,
    })
}

/// 获取信号强度详细信息
pub async fn get_signal_strength(conn: &Connection) -> zbus::Result<SignalStrengthResponse> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.NetworkRegistration").await?;
        let result: HashMap<String, OwnedValue> = proxy.call("GetSignalStrength", &()).await?;
        
        let strength = result
            .get("Strength")
            .and_then(|v| i32::try_from(v.clone()).ok())
            .unwrap_or(0);
        
        Ok(SignalStrengthResponse { strength })
    }).await
}

/// 获取 NITZ 网络时间
pub async fn get_nitz_time(conn: &Connection) -> zbus::Result<NitzTimeResponse> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.Modem").await?;
        
        match proxy.call("GetNITZ", &()).await {
            Ok(time_string) => Ok(NitzTimeResponse {
                time_string,
                available: true,
            }),
            Err(_) => Ok(NitzTimeResponse {
                time_string: String::new(),
                available: false,
            }),
        }
    }).await
}

/// 获取 IMS 状态
pub async fn get_ims_status(conn: &Connection) -> zbus::Result<ImsStatusResponse> {
    let props = snapshot_properties(conn, "org.ofono.IpMultimediaSystem").await?;
    let registered = props
        .get("Registered")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    let voice_capable = props
        .get("VoiceCapable")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    let sms_capable = props
        .get("SmsCapable")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    Ok(ImsStatusResponse { registered, voice_capable, sms_capable })
}

/// 获取通话音量
pub async fn get_call_volume(conn: &Connection) -> zbus::Result<CallVolumeResponse> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.CallVolume").await?;
        let props: HashMap<String, OwnedValue> = proxy.call("GetProperties", &()).await?;
        
        let speaker_volume = props
            .get("SpeakerVolume")
            .and_then(|v| u8::try_from(v.clone()).ok())
            .unwrap_or(0);
        
        let microphone_volume = props
            .get("MicrophoneVolume")
            .and_then(|v| u8::try_from(v.clone()).ok())
            .unwrap_or(0);
        
        let muted = props
            .get("Muted")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false);
        
        Ok(CallVolumeResponse {
            speaker_volume,
            microphone_volume,
            muted,
        })
    }).await
}

/// 设置通话音量
pub async fn set_call_volume(
    conn: &Connection,
    speaker: Option<u8>,
    microphone: Option<u8>,
    muted: Option<bool>,
) -> zbus::Result<()> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.CallVolume").await?;
        
        if let Some(vol) = speaker {
            let val = zbus::zvariant::Value::new(vol);
            proxy.call::<_, _, ()>("SetProperty", &("SpeakerVolume", val)).await?;
        }
        
        if let Some(vol) = microphone {
            let val = zbus::zvariant::Value::new(vol);
            proxy.call::<_, _, ()>("SetProperty", &("MicrophoneVolume", val)).await?;
        }
        
        if let Some(m) = muted {
            let val = zbus::zvariant::Value::new(m);
            proxy.call::<_, _, ()>("SetProperty", &("Muted", val)).await?;
        }
        
        Ok(())
    }).await
}

/// 获取语音留言状态
pub async fn get_voicemail_status(conn: &Connection) -> zbus::Result<VoicemailStatusResponse> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.MessageWaiting").await?;
        let props: HashMap<String, OwnedValue> = proxy.call("GetProperties", &()).await?;
        
        let waiting = props
            .get("VoicemailWaiting")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false);
        
        let message_count = props
            .get("VoicemailMessageCount")
            .and_then(|v| u8::try_from(v.clone()).ok())
            .unwrap_or(0);
        
        let mailbox_number = props
            .get("VoicemailMailboxNumber")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| String::new());
        
        Ok(VoicemailStatusResponse {
            waiting,
            message_count,
            mailbox_number,
        })
    }).await
}

/// 获取运营商列表（快速，仅返回当前）
pub async fn get_operators(conn: &Connection) -> zbus::Result<OperatorListResponse> {
    let result = OPERATOR_SNAPSHOT.get(&snapshot_scope(conn), Duration::from_secs(10), with_modem_read("operators", async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.NetworkRegistration").await?;
        proxy.call("GetOperators", &()).await
    })).await?;

    let mut operators = Vec::new();
    for (path, props) in result.iter() {
        let props = props.iter().map(|(key, value)| Ok((key.clone(), value.try_clone()?)))
            .collect::<zbus::Result<Properties>>()?;
        operators.push(parse_operator_info(path.to_string(), props));
    }

    Ok(OperatorListResponse { operators })
}

/// 扫描运营商（慢，返回所有可用）
pub async fn scan_operators(conn: &Connection) -> zbus::Result<OperatorListResponse> {
    with_modem_write(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.NetworkRegistration").await?;
        let result: Vec<(zbus::zvariant::OwnedObjectPath, HashMap<String, OwnedValue>)> = 
            proxy.call("Scan", &()).await?;
        
        let mut operators = Vec::new();
        for (path, props) in result {
            operators.push(parse_operator_info(path.to_string(), props));
        }
        
        Ok(OperatorListResponse { operators })
    }).await
}

/// 解析运营商信息
fn parse_operator_info(path: String, props: HashMap<String, OwnedValue>) -> OperatorInfo {
    let name = props
        .get("Name")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "Unknown".to_string());
    
    let status = props
        .get("Status")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "unknown".to_string());
    
    let mcc = props
        .get("MobileCountryCode")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "".to_string());
    
    let mnc = props
        .get("MobileNetworkCode")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| "".to_string());
    
    let technologies: Vec<String> = props
        .get("Technologies")
        .and_then(|v| {
            // 尝试将 Value 转换为数组
            if let Ok(arr) = <Vec<String>>::try_from(v.clone()) {
                Some(arr)
            } else {
                None
            }
        })
        .unwrap_or_else(Vec::new);
    
    OperatorInfo {
        path,
        name,
        status,
        mcc,
        mnc,
        technologies,
    }
}

/// 手动注册到指定运营商
pub async fn register_operator_manual(conn: &Connection, mccmnc: &str) -> zbus::Result<()> {
    let _modem_change = crate::serial::modem_change();
    with_serial(async {
        let radio_proxy = RadioSettingsProxy::new(conn).await?;
        let radio_props = radio_proxy.get_properties().await?;
        let technology_preference = radio_props
            .get("TechnologyPreference")
            .and_then(|value| String::try_from(value.clone()).ok())
            .ok_or_else(|| zbus::Error::Failure("Missing TechnologyPreference".to_string()))?;

        let registration_proxy =
            Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.NetworkRegistration").await?;

        match RadioMode::from_ofono_value(&technology_preference) {
            Some(RadioMode::Auto) => {
                match registration_proxy
                    .call(
                        "RegisterManually",
                        &(mccmnc, zbus::zvariant::Value::Str("NR".into())),
                    )
                    .await
                {
                    Ok(()) => Ok(()),
                    Err(nr_error) => {
                        warn!("NR manual registration failed, retrying with LTE: {}", nr_error);
                        registration_proxy
                            .call(
                                "RegisterManually",
                                &(mccmnc, zbus::zvariant::Value::Str("LTE".into())),
                            )
                            .await
                    }
                }
            }
            Some(RadioMode::LteOnly) => {
                registration_proxy
                    .call(
                        "RegisterManually",
                        &(mccmnc, zbus::zvariant::Value::Str("LTE".into())),
                    )
                    .await
            }
            Some(RadioMode::NrOnly) => {
                registration_proxy
                    .call(
                        "RegisterManually",
                        &(mccmnc, zbus::zvariant::Value::Str("NR".into())),
                    )
                    .await
            }
            None => Err(zbus::Error::Failure(format!(
                "Unsupported TechnologyPreference: {}",
                technology_preference
            ))),
        }
    }).await
}

// Verify the Modem rather than trusting ofono's cached NetworkRegistration.Mode.
fn parse_cops_mode(response: &str) -> Option<u8> {
    if !response.lines().any(|line| line.trim() == "OK")
        || response.lines().any(|line| {
            let line = line.trim();
            line == "ERROR" || line.starts_with("+CME ERROR:") || line.starts_with("+CMS ERROR:")
        })
    {
        return None;
    }
    response.lines().find_map(|line| {
        line.trim().strip_prefix("+COPS:")?
            .split(',').next()?.trim().parse::<u8>().ok().filter(|mode| *mode <= 4)
    })
}

async fn ensure_automatic_selection<C, Command, P, Pause>(
    mut command: C,
    mut pause: P,
) -> Result<(), String>
where
    C: FnMut(&'static str) -> Command,
    Command: std::future::Future<Output = Result<String, String>>,
    P: FnMut() -> Pause,
    Pause: std::future::Future<Output = ()>,
{
    let mut last_state = "operator selection mode is unavailable".to_string();
    let mut mode_known = false;
    // SIM refresh can briefly remove the AT interface. Retry reads, not writes.
    for attempt in 0..4 {
        match command("AT+COPS?").await {
            Ok(response) => match parse_cops_mode(&response) {
                Some(0) => return Ok(()),
                Some(mode) => {
                    last_state = format!("AT+COPS? reports mode {mode}");
                    mode_known = true;
                    break;
                }
                None => last_state = format!("Invalid AT+COPS? response: {}", response.trim()),
            },
            Err(error) => last_state = error,
        }
        if attempt < 3 { pause().await; }
    }
    if !mode_known {
        return Err(format!("Cannot verify automatic operator selection: {last_state}"));
    }

    // One request only: Failed may still have initiated automatic selection.
    let selection_error = command("AT+COPS=0").await.err();
    for _ in 0..6 {
        pause().await;
        match command("AT+COPS?").await {
            Ok(response) => match parse_cops_mode(&response) {
                Some(0) => return Ok(()),
                Some(mode) => last_state = format!("AT+COPS? still reports mode {mode}"),
                None => last_state = format!("Invalid AT+COPS? response: {}", response.trim()),
            },
            Err(error) => last_state = error,
        }
    }
    Err(format!("Automatic operator selection was not confirmed: {last_state}{}",
        selection_error.map(|error| format!("; AT+COPS=0: {error}")).unwrap_or_default()))
}

/// 恢复自动选网并通过 AT 回读确认，不等待网络注册或数据附着完成。
pub async fn register_operator_auto(conn: &Connection) -> zbus::Result<()> {
    let _modem_change = crate::serial::modem_change();
    with_serial(async {
        let modem = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.Modem").await?;
        let properties: HashMap<String, OwnedValue> = tokio::time::timeout(
            Duration::from_secs(10), modem.call("GetProperties", &()),
        ).await.map_err(|_| zbus::Error::Failure("Reading Modem state timed out".into()))??;
        let powered = properties.get("Powered").and_then(|value| bool::try_from(value.clone()).ok()).unwrap_or(false);
        let online = properties.get("Online").and_then(|value| bool::try_from(value.clone()).ok()).unwrap_or(false);
        if !powered || !online {
            // Never enable the radio implicitly while the user is in airplane mode.
            return Err(zbus::Error::Failure("Modem is offline; restore automatic selection after enabling the radio".into()));
        }

        let registration = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.NetworkRegistration").await?;
        // Use ofono first so its own selection policy is updated as well.
        let registered: zbus::Result<()> = match tokio::time::timeout(
            Duration::from_secs(30), registration.call("Register", &()),
        ).await {
            Ok(result) => result,
            Err(_) => Err(zbus::Error::Failure("Automatic registration request timed out".into())),
        };
        if let Err(error) = registered {
            warn!("ofono automatic registration did not confirm success; checking Modem mode: {}", error);
        }

        ensure_automatic_selection(|command| {
            let modem = &modem;
            async move {
                let timeout = if command == "AT+COPS=0" { 150 } else { 10 };
                match tokio::time::timeout(Duration::from_secs(timeout),
                    modem.call::<_, _, String>("SendAtcmd", &(command)),
                ).await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => Err(format!("{command} timed out")),
                }
            }
        }, || tokio::time::sleep(Duration::from_secs(2)))
            .await.map_err(zbus::Error::Failure)
    }).await
}

type AutoSelectionResult = Option<Result<(), String>>;
type AutoSelectionReceiver = tokio::sync::watch::Receiver<AutoSelectionResult>;
static PROFILE_AUTO_SELECTION: tokio::sync::Mutex<Option<AutoSelectionReceiver>> = tokio::sync::Mutex::const_new(None);

// Coalesce overlapping profile switches: automatic selection is a global Modem
// policy, so a pending recovery can serve the latest enabled profile as well.
async fn start_profile_auto_selection<F>(
    state: &tokio::sync::Mutex<Option<AutoSelectionReceiver>>,
    work: F,
) -> AutoSelectionReceiver
where
    F: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    let mut state = state.lock().await;
    if let Some(receiver) = state.as_ref() {
        if receiver.borrow().is_none() && receiver.has_changed().is_ok() {
            return receiver.clone();
        }
    }
    let (sender, receiver) = tokio::sync::watch::channel(None);
    tokio::spawn(async move {
        // Bound the lifetime of the Modem change guard and command lock too.
        let result = match tokio::time::timeout(Duration::from_secs(120), work).await {
            Ok(result) => result,
            Err(_) => Err("Automatic selection confirmation timed out; the Modem may still be registering".into()),
        };
        if let Err(error) = &result {
            warn!("Profile automatic selection remains unconfirmed (registration may still succeed): {}", error);
        }
        let _ = sender.send(Some(result));
    });
    *state = Some(receiver.clone());
    receiver
}

async fn wait_profile_auto_selection(mut receiver: AutoSelectionReceiver, wait: Duration) -> (bool, Option<String>) {
    // Timing out this observer does not cancel the submitted background job.
    if receiver.borrow().is_none() {
        let _ = tokio::time::timeout(wait, receiver.changed()).await;
    }
    let result = receiver.borrow().clone();
    match result {
        Some(Ok(())) => (false, None),
        Some(Err(error)) => (false, Some(error)),
        None => (true, Some("自动选网结果尚未确认，已转入后台处理；设备可能暂时返回失败或超时".into())),
    }
}

/// Submit recovery after a confirmed profile switch, allowing at most two extra
/// seconds in the HTTP request. Background recovery retains Modem read guards.
pub async fn submit_profile_auto_selection(conn: Arc<Connection>) -> (bool, Option<String>) {
    let receiver = start_profile_auto_selection(&PROFILE_AUTO_SELECTION, async move {
        register_operator_auto(&conn).await.map_err(|error| error.to_string())
    }).await;
    wait_profile_auto_selection(receiver, Duration::from_secs(2)).await
}

/// 获取呼叫转移设置
pub async fn get_call_forwarding(conn: &Connection) -> zbus::Result<CallForwardingResponse> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.CallForwarding").await?;
        let props: HashMap<String, OwnedValue> = proxy.call("GetProperties", &()).await?;
        
        let voice_unconditional = props
            .get("VoiceUnconditional")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| String::new());
        
        let voice_busy = props
            .get("VoiceBusy")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| String::new());
        
        let voice_no_reply = props
            .get("VoiceNoReply")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| String::new());
        
        let voice_no_reply_timeout = props
            .get("VoiceNoReplyTimeout")
            .and_then(|v| u16::try_from(v.clone()).ok())
            .unwrap_or(20);
        
        let voice_not_reachable = props
            .get("VoiceNotReachable")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| String::new());
        
        let forwarding_flag_on_sim = props
            .get("ForwardingFlagOnSim")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false);
        
        Ok(CallForwardingResponse {
            voice_unconditional,
            voice_busy,
            voice_no_reply,
            voice_no_reply_timeout,
            voice_not_reachable,
            forwarding_flag_on_sim,
        })
    }).await
}

/// 设置呼叫转移
pub async fn set_call_forwarding(
    conn: &Connection,
    forward_type: &str,
    number: &str,
    timeout: Option<u16>,
) -> zbus::Result<()> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.CallForwarding").await?;
        
        let property_name = match forward_type {
            "unconditional" => "VoiceUnconditional",
            "busy" => "VoiceBusy",
            "noreply" => "VoiceNoReply",
            "notreachable" => "VoiceNotReachable",
            _ => return Err(zbus::Error::Failure("Invalid forward type".to_string())),
        };
        
        let number_value = zbus::zvariant::Value::new(number);
        proxy.call::<_, _, ()>("SetProperty", &(property_name, number_value)).await?;
        
        // 如果是 noreply 类型且提供了超时时间
        if forward_type == "noreply" && timeout.is_some() {
            let timeout_value = zbus::zvariant::Value::new(timeout.unwrap());
            proxy.call::<_, _, ()>("SetProperty", &("VoiceNoReplyTimeout", timeout_value)).await?;
        }
        
        Ok(())
    }).await
}

/// 获取通话设置
pub async fn get_call_settings(conn: &Connection) -> zbus::Result<CallSettingsResponse> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.CallSettings").await?;
        let props: HashMap<String, OwnedValue> = proxy.call("GetProperties", &()).await?;
        
        let calling_line_presentation = props
            .get("CallingLinePresentation")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "unknown".to_string());
        
        let calling_name_presentation = props
            .get("CallingNamePresentation")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "unknown".to_string());
        
        let connected_line_presentation = props
            .get("ConnectedLinePresentation")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "unknown".to_string());
        
        let connected_line_restriction = props
            .get("ConnectedLineRestriction")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "unknown".to_string());
        
        let called_line_presentation = props
            .get("CalledLinePresentation")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "unknown".to_string());
        
        let calling_line_restriction = props
            .get("CallingLineRestriction")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "unknown".to_string());
        
        let hide_caller_id = props
            .get("HideCallerId")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "default".to_string());
        
        let voice_call_waiting = props
            .get("VoiceCallWaiting")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "unknown".to_string());
        
        Ok(CallSettingsResponse {
            calling_line_presentation,
            calling_name_presentation,
            connected_line_presentation,
            connected_line_restriction,
            called_line_presentation,
            calling_line_restriction,
            hide_caller_id,
            voice_call_waiting,
        })
    }).await
}

/// 设置通话设置
pub async fn set_call_setting(conn: &Connection, property: &str, value: &str) -> zbus::Result<()> {
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.CallSettings").await?;
        let value_variant = zbus::zvariant::Value::new(value);
        proxy.call("SetProperty", &(property, value_variant)).await
    }).await
}

// ============ SIM 卡槽功能 ============

use crate::models::SimSlotResponse;

/// 获取 SIM 卡槽信息
pub async fn get_sim_slot(conn: &Connection) -> zbus::Result<SimSlotResponse> {
    let response = SLOT_SNAPSHOT.get(&snapshot_scope(conn), Duration::from_secs(60), async {
        let response = send_at_read_command(conn, "AT+SPCONFIGSIMSLOT?").await?;
        if !response.lines().any(|line| line.trim() == "OK") || !response.contains("+SPCONFIGSIMSLOT:") {
            return Err(zbus::Error::Failure(format!("Invalid SIM slot response: {}", response.trim())));
        }
        Ok(response)
    }).await?;

    // 解析响应：+SPCONFIGSIMSLOT: 66051
    let raw_value = response
        .lines()
        .find(|line| line.contains("+SPCONFIGSIMSLOT:"))
        .and_then(|line| line.split(':').nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| String::new());

    // 根据值判断卡槽（这个需要根据实际设备的规则来解析）
    // 66051 可能表示卡槽 1，66306 可能表示卡槽 2
    // 您需要提供切换命令来确认规则
    let active_slot = if raw_value.contains("66051") { 1 } else if raw_value.contains("66306") { 2 } else { 0 };

    Ok(SimSlotResponse {
        active_slot,
        raw_value,
    })
}

/// 切换 SIM 卡槽
pub async fn switch_sim_slot(conn: &Connection, slot: u8) -> zbus::Result<String> {
    let _modem_change = crate::serial::modem_change();
    with_serial(async {
        let proxy = Proxy::new(conn, "org.ofono", "/ril_0", "org.ofono.Modem").await?;
        
        // 根据卡槽号生成 AT 命令
        // 注意：这个命令格式需要根据您的设备文档确认
        // 可能是 AT+SPCONFIGSIMSLOT=1 或 AT+SPCONFIGSIMSLOT=66051
        let value = match slot {
            1 => "66051",  // 卡槽 1 的值
            2 => "66306",  // 卡槽 2 的值（猜测，需要您确认）
            _ => return Err(zbus::Error::Failure("Invalid slot number, must be 1 or 2".to_string())),
        };
        
        let cmd = format!("AT+SPCONFIGSIMSLOT={}", value);
        let response: String = proxy.call("SendAtcmd", &(cmd.as_str())).await?;
        
        Ok(response)
    }).await
}

#[cfg(test)]
mod profile_auto_selection_tests {
    use super::{start_profile_auto_selection, wait_profile_auto_selection};
    use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

    #[tokio::test]
    async fn timeout_keeps_recovery_running_and_repeated_switches_share_the_job() {
        let state = tokio::sync::Mutex::new(None);
        let calls = Arc::new(AtomicUsize::new(0));
        let first_calls = calls.clone();
        let (release, wait) = tokio::sync::oneshot::channel();
        let receiver = start_profile_auto_selection(&state, async move {
            first_calls.fetch_add(1, Ordering::SeqCst);
            wait.await.unwrap();
            Ok(())
        }).await;
        let pending = wait_profile_auto_selection(receiver, Duration::from_millis(1)).await;
        assert!(pending.0);
        assert!(pending.1.unwrap().contains("后台处理"));
        let second_calls = calls.clone();
        let mut shared = start_profile_auto_selection(&state, async move {
            second_calls.fetch_add(1, Ordering::SeqCst);
            Err("A duplicate job must not run".into())
        }).await;
        release.send(()).unwrap();
        shared.changed().await.unwrap();
        assert_eq!(wait_profile_auto_selection(shared, Duration::from_secs(1)).await, (false, None));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let failed = start_profile_auto_selection(&state, async { Err("org.ofono.Error.Failed".into()) }).await;
        assert_eq!(wait_profile_auto_selection(failed, Duration::from_secs(1)).await,
            (false, Some("org.ofono.Error.Failed".into())));
    }
}

#[cfg(test)]
mod operator_selection_tests {
    use super::{ensure_automatic_selection, parse_cops_mode};
    use std::{collections::VecDeque, future::{ready, Future}, task::{Context, Poll, Waker}};

    fn run_script(script: Vec<(&'static str, Result<&'static str, &'static str>)>) -> Result<(), String> {
        let mut script = VecDeque::from(script);
        let result = {
            let mut future = Box::pin(ensure_automatic_selection(|command| {
                let (expected, result) = script.pop_front().expect("Unexpected Modem command");
                assert_eq!(command, expected);
                ready(result.map(str::to_string).map_err(str::to_string))
            }, || ready(())));
            let mut context = Context::from_waker(Waker::noop());
            match future.as_mut().poll(&mut context) {
                Poll::Ready(result) => result,
                Poll::Pending => panic!("Scripted commands must complete immediately"),
            }
        };
        assert!(script.is_empty(), "Not all expected Modem commands were used");
        result
    }

    #[test]
    fn parses_real_cops_responses_without_accepting_errors() {
        assert_eq!(parse_cops_mode("\r\n+COPS: 0,0,\"HK CSL\",13\r\nOK\r\n"), Some(0));
        assert_eq!(parse_cops_mode("+COPS: 1\nOK\n"), Some(1));
        assert_eq!(parse_cops_mode("AT+COPS?\n+CEREG: 2,1\n+COPS: 0\nOK\n"), Some(0));
        for response in ["+COPS: 0", "+COPS: 0\nERROR\n", "OK\n", "+COPS: bad\nOK\n", "+COPS: 99\nOK\n"] {
            assert_eq!(parse_cops_mode(response), None);
        }
    }

    #[test]
    fn failed_command_with_confirmed_automatic_mode_succeeds() {
        assert!(run_script(vec![
            ("AT+COPS?", Ok("+COPS: 1\nOK\n")),
            ("AT+COPS=0", Err("org.ofono.Error.Failed: Operation failed")),
            ("AT+COPS?", Ok("+COPS: 1\nOK\n")),
            ("AT+COPS?", Ok("+COPS: 0,0,\"HK CSL\",13\nOK\n")),
        ]).is_ok());
    }

    #[test]
    fn successful_command_without_mode_change_is_not_success() {
        let mut script = vec![
            ("AT+COPS?", Ok("+COPS: 1\nOK\n")),
            ("AT+COPS=0", Ok("OK\n")),
        ];
        script.extend(std::iter::repeat_n(("AT+COPS?", Ok("+COPS: 1\nOK\n")), 6));
        assert!(run_script(script).unwrap_err().contains("still reports mode 1"));
    }

    #[test]
    fn automatic_mode_needs_no_extra_selection_command() {
        assert!(run_script(vec![("AT+COPS?", Ok("+COPS: 0\nOK\n"))]).is_ok());
    }

    #[test]
    fn unreadable_mode_is_not_treated_as_automatic() {
        assert!(run_script(vec![("AT+COPS?", Err("AT interface unavailable")); 4]).is_err());
    }

    #[test]
    fn transient_interface_loss_is_retried_without_repeating_selection() {
        assert!(run_script(vec![
            ("AT+COPS?", Err("AT interface unavailable")),
            ("AT+COPS?", Ok("+COPS: 4\nOK\n")),
            ("AT+COPS=0", Ok("OK\n")),
            ("AT+COPS?", Err("AT interface restarting")),
            ("AT+COPS?", Ok("+COPS: 0\nOK\n")),
        ]).is_ok());
    }
}

