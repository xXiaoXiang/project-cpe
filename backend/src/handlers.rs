/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-12-11 17:44:29
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:46:04
 * @FilePath: /udx710-backend/backend/src/handlers.rs
 * @Description: 
 * 
 * Copyright (c) 2025 by 1orz, All Rights Reserved. 
 */
//! API 处理器模块
//! 
//! 包含所有 HTTP API 的处理函数

use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::stream;
use serde_json::{json, Value};
use std::{convert::Infallible, process::Stdio, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader},
    process::Command as TokioCommand,
    sync::mpsc,
};
use zbus::Connection;

use crate::{
    config::{ConfigManager, RefreshConfig},
    dbus::{
        get_airplane_mode, get_all_apn_contexts, get_data_connection_status, get_device_info_data,
        get_network_info_data, get_qos_info_data, get_radio_mode, get_roaming_status, get_serving_cell_info,
        get_sim_info_data, send_at_command, send_at_read_command, set_airplane_mode, set_apn_properties, set_data_connection,
        set_radio_mode, set_roaming_allowed,
    },
    iptables::flush_iptables,
    models::*,
    usb_switch,
    utils::{
        bands_to_bitmask, bitmask_to_bands, build_splband_lte_command, build_splband_nr_command,
        format_uptime, get_active_interfaces, get_cell_command_config,
        parse_at_response_to_2d_vec, parse_neighbor_cells, parse_primary_cell,
        parse_splband_lte_response, parse_splband_nr_response, read_cpu_info, read_cpu_load_sync,
        read_disk_info, read_interface_stats, read_memory_info, read_network_interfaces, read_system_info,
        read_uptime, sample_cpu_usage,
    },
};
use crate::state::FrontendRuntime;
use std::process::Command;
use crate::serial::ReadSnapshot;

static BAND_LOCK_SNAPSHOT: ReadSnapshot<BandLockStatus> = ReadSnapshot::new();
static CELL_LOCK_SNAPSHOT: ReadSnapshot<CellLockStatusResponse> = ReadSnapshot::new();
static STATS_SNAPSHOT: ReadSnapshot<SystemStatsResponse> = ReadSnapshot::new();
static SYSTEM_INFO_SNAPSHOT: ReadSnapshot<SystemInfo> = ReadSnapshot::new();
static EUICC_SNAPSHOT: ReadSnapshot<EuiccInfoResponse> = ReadSnapshot::new();
static EUICC_PROBE_SNAPSHOT: ReadSnapshot<Result<Value, String>> = ReadSnapshot::new();
static NOTIFICATION_SNAPSHOT: ReadSnapshot<Vec<EuiccNotification>> = ReadSnapshot::new();

struct EuiccCacheChange;

impl EuiccCacheChange {
    fn new() -> Self {
        Self::invalidate();
        Self
    }

    fn invalidate() {
        EUICC_SNAPSHOT.invalidate();
        EUICC_PROBE_SNAPSHOT.invalidate();
        NOTIFICATION_SNAPSHOT.invalidate();
    }
}

impl Drop for EuiccCacheChange {
    fn drop(&mut self) { Self::invalidate(); }
}

async fn with_euicc_write<T>(work: impl std::future::Future<Output = T>) -> T {
    let _change = EuiccCacheChange::new();
    crate::serial::with_euicc_serial(work).await
}

fn lpac_command(args: &[String]) -> TokioCommand {
    let path = std::env::var_os("LPAC_PATH")
        .unwrap_or_else(|| "/home/root/lpac/lpac".into());
    let apdu = std::env::var_os("LPAC_APDU").unwrap_or_else(|| "at".into());
    let at_device = std::env::var_os("LPAC_APDU_AT_DEVICE")
        .unwrap_or_else(|| "/dev/stty_lte30".into());

    let mut command = TokioCommand::new(path);
    command
        .args(args)
        .env("LPAC_APDU", apdu)
        .env("LPAC_APDU_AT_DEVICE", at_device)
        .kill_on_drop(true);
    command
}

#[derive(serde::Deserialize, serde::Serialize)]
struct LpacResponse {
    #[serde(rename = "type")]
    event_type: String,
    payload: LpacPayload,
}

#[derive(serde::Deserialize, serde::Serialize, Clone)]
struct LpacPayload {
    code: i64,
    message: String,
    data: Value,
}

// LPAC can emit several progress objects before its final lpa object.
fn parse_lpac_events(text: &str) -> Vec<LpacResponse> {
    text.lines()
        .flat_map(|line| {
            let start = line.find('{').unwrap_or(line.len());
            serde_json::Deserializer::from_str(&line[start..])
                .into_iter::<LpacResponse>()
                .map_while(Result::ok)
        })
        .collect()
}

fn lpac_failure(payload: &LpacPayload) -> String {
    let detail = match &payload.data {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        value => value.to_string(),
    };
    format!("LPAC ({}): {}{}{}", payload.code, payload.message,
        if detail.is_empty() { "" } else { ": " }, detail)
}

// A broken APDU driver must not exhaust the web server's memory by repeatedly
// printing errors. Allow profile icons, but bound both pipes independently.
const LPAC_STDOUT_LIMIT: usize = 4 * 1024 * 1024;
const LPAC_STDERR_LIMIT: usize = 64 * 1024;

async fn read_lpac_output<R: AsyncRead + Unpin>(reader: R, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut output).await?;
    if output.len() > limit {
        return Err(std::io::Error::other(format!("LPAC output exceeded {limit} bytes")));
    }
    Ok(output)
}

async fn capture_lpac_output(
    mut command: TokioCommand,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    let mut child = command.kill_on_drop(true)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .map_err(|error| format!("Failed to start lpac: {error}"))?;
    let captured = tokio::time::timeout(timeout, async {
        let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("Missing LPAC stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| std::io::Error::other("Missing LPAC stderr"))?;
        // Read both pipes while waiting; return immediately on oversized output.
        let (status, stdout, stderr) = tokio::try_join!(
            child.wait(),
            read_lpac_output(stdout, LPAC_STDOUT_LIMIT),
            read_lpac_output(stderr, LPAC_STDERR_LIMIT),
        )?;
        Ok::<_, std::io::Error>(std::process::Output { status, stdout, stderr })
    }).await;
    let error = match captured {
        Ok(Ok(output)) => return Ok(output),
        Err(_) => format!("LPAC timed out after {} seconds", timeout.as_secs()),
        Ok(Err(error)) => format!("Failed to read LPAC output: {error}"),
    };
    // Tokio sends SIGKILL on Unix (not SIGINT/SIGTERM), then waits to reap the
    // process. This also handles LPAC ignoring Ctrl+C while reading the AT port.
    if let Err(cleanup_error) = child.kill().await {
        return Err(format!("{error}; failed to terminate/reap LPAC: {cleanup_error}"));
    }
    Err(error)
}

fn lpac_diagnostic(bytes: &[u8]) -> String {
    // Bound HTTP error messages too, and preserve UTF-8 character boundaries.
    let text = String::from_utf8_lossy(bytes);
    let mut tail: Vec<char> = text.chars().rev().take(1024).collect();
    tail.reverse();
    tail.into_iter().collect()
}

fn decode_lpac_output(output: std::process::Output) -> Result<Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let response = parse_lpac_events(&stdout)
        .into_iter()
        .filter(|event| event.event_type == "lpa")
        .last()
        .ok_or_else(|| {
            format!("LPAC exited with {} without a final lpa result: {} {}",
                output.status, lpac_diagnostic(&output.stdout), lpac_diagnostic(&output.stderr))
        })?;

    if !output.status.success() || response.payload.code != 0 {
        return Err(format!("{} ({}). {}", lpac_failure(&response.payload),
            output.status, lpac_diagnostic(&output.stderr)));
    }

    Ok(response.payload.data)
}

async fn run_lpac_json_timeout(args: &[String], seconds: u64) -> Result<Value, String> {
    let result = match capture_lpac_output(lpac_command(args), std::time::Duration::from_secs(seconds)).await {
        Ok(output) => decode_lpac_output(output),
        Err(error) => Err(error),
    };
    if let Err(error) = &result {
        // Do not log activation codes, nicknames, or the full Profile list.
        tracing::warn!(command = ?args.iter().take(2).collect::<Vec<_>>(), error = %error, "LPAC request failed");
    }
    result
}

async fn run_lpac_json(args: &[String]) -> Result<Value, String> {
    run_lpac_json_timeout(args, 120).await
}

#[cfg(test)]
mod lpac_process_tests {
    use super::*;

    fn fixture(mode: &str) -> TokioCommand {
        let mut command = TokioCommand::new(std::env::current_exe().unwrap());
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        command.args(["--ignored", "--exact", &format!("{module}::child"), "--nocapture"])
            .env("LPAC_TEST_CHILD_MODE", mode);
        command
    }

    // A separate process exercises the actual pipe/exit/kill handling on both
    // Windows test hosts and Linux. It never accesses a modem or SIM.
    #[test]
    #[ignore]
    fn child() {
        match std::env::var("LPAC_TEST_CHILD_MODE").unwrap().as_str() {
            "hung" => std::thread::sleep(std::time::Duration::from_secs(60)),
            "overflow" => {
                use std::io::Write;
                let _ = std::io::stderr().write_all(&vec![b'x'; LPAC_STDERR_LIMIT + 1]);
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
            "failed" => std::process::exit(7),
            "malformed" => println!("ordinary SIM: ISD-R application not found"),
            "lpa_failure" => println!(r#"{{"type":"lpa","payload":{{"code":-1,"message":"euicc_init","data":null}}}}"#),
            mode => {
                println!(r#"{{"type":"lpa","payload":{{"code":0,"message":"success","data":{{"eidValue":"test-eid"}}}}}}"#);
                if mode == "false_success" { std::process::exit(1); }
            }
        }
    }

    #[tokio::test]
    async fn process_failures_do_not_prevent_the_next_request() {
        for mode in ["failed", "malformed", "lpa_failure", "false_success"] {
            let output = capture_lpac_output(fixture(mode), std::time::Duration::from_secs(5)).await.unwrap();
            let error = decode_lpac_output(output).unwrap_err();
            assert!(!error.is_empty(), "{mode}");
            if mode == "failed" { assert!(error.contains('7'), "{error}"); }
        }
        let output = capture_lpac_output(fixture("success"), std::time::Duration::from_secs(5)).await.unwrap();
        assert_eq!(decode_lpac_output(output).unwrap()["eidValue"], "test-eid");
    }

    #[tokio::test]
    async fn timeout_and_excessive_output_terminate_the_child_promptly() {
        let started = std::time::Instant::now();
        let error = capture_lpac_output(fixture("hung"), std::time::Duration::from_millis(500)).await.unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let output = capture_lpac_output(fixture("success"), std::time::Duration::from_secs(5)).await.unwrap();
        assert_eq!(decode_lpac_output(output).unwrap()["eidValue"], "test-eid");
        let error = capture_lpac_output(fixture("overflow"), std::time::Duration::from_secs(5)).await.unwrap_err();
        assert!(error.contains("exceeded"), "{error}");
    }

    #[tokio::test]
    async fn output_limit_handles_exact_boundary_and_unterminated_lines() {
        assert_eq!(read_lpac_output(&b"1234"[..], 4).await.unwrap(), b"1234");
        assert!(read_lpac_output(&b"12345"[..], 4).await.is_err());
        let text = "测试".repeat(2000);
        let tail = lpac_diagnostic(text.as_bytes());
        assert_eq!(tail.chars().count(), 1024);
    }
}

fn euicc_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiResponse::<Value>::error(message)),
    )
        .into_response()
}

fn euicc_failure(message: impl Into<String>) -> Response {
    euicc_error(StatusCode::OK, message)
}

fn validate_euicc_identifier(iccid: &str) -> Result<(), String> {
    if iccid.is_empty()
        || iccid.len() > 64
        || !iccid.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("Invalid ICCID/Profile identifier".to_string());
    }
    Ok(())
}

/// GET /api/euicc - 检查 eUICC 并列出已安装 Profile
pub async fn get_euicc_info(State(_conn): State<Arc<Connection>>) -> Response {
    let result = EUICC_SNAPSHOT.get("lpac", std::time::Duration::from_secs(10), async {
        crate::serial::with_euicc_serial(async {
            let chip_args = vec!["chip".to_string(), "info".to_string()];
            // A regular SIM can hang chip info until its hard timeout. Briefly
            // reuse a failed probe as well, instead of spawning it for every client.
            let probe = EUICC_PROBE_SNAPSHOT.get("lpac", std::time::Duration::from_secs(10), async {
                Ok(run_lpac_json_timeout(&chip_args, 10).await)
            }).await.map_err(|error| error.to_string())?;
            let chip_info = probe.as_ref().as_ref()
                .map_err(|error| format!("无法读取 eUICC：请确认插入的是 eUICC 卡，并等待 SIM 初始化完成。{error}"))?;
            let eid = chip_info
                .get("eidValue")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "lpac chip info did not return an EID".to_string())?
                .to_string();
            let firmware_version = chip_info
                .get("EUICCInfo2")
                .and_then(|value| value.get("euiccFirmwareVer"))
                .and_then(Value::as_str)
                .map(str::to_string);

            let profile_args = vec!["profile".to_string(), "list".to_string()];
            let profile_data = run_lpac_json_timeout(&profile_args, 20).await?;
            let profiles = serde_json::from_value(profile_data)
                .map_err(|error| format!("Invalid lpac profile list: {error}"))?;

            Ok::<_, String>(EuiccInfoResponse {
                eid,
                firmware_version,
                profiles,
            })
        }).await.map_err(zbus::Error::Failure)
    }).await;

    match result {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("eUICC ready", data.as_ref().clone())),
        )
            .into_response(),
        Err(error) => euicc_failure(error.to_string()),
    }
}

/// POST /api/euicc/profile/enable - 启用 Profile，确认切换成功后返回最新 Profile 列表
pub async fn enable_euicc_profile(
    State(conn): State<Arc<Connection>>,
    Json(request): Json<EuiccProfileRequest>,
) -> Response {
    if let Err(error) = validate_euicc_identifier(&request.iccid) {
        return euicc_failure(error);
    }

    let _modem_change = crate::serial::modem_change();

    let args = vec![
        "profile".to_string(),
        "enable".to_string(),
        request.iccid.clone(),
        "1".to_string(),
    ];
    let result: Result<Vec<EuiccProfile>, String> = with_euicc_write(async {
        run_lpac_json(&args).await?;

        let list_args = vec!["profile".to_string(), "list".to_string()];
        let mut last_error = String::new();
        for _ in 0..4 {
            // eUICC 切换后需要时间恢复响应，过早查询会失败或仍显示旧状态
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            match run_lpac_json(&list_args).await {
                Ok(data) => match serde_json::from_value::<Vec<EuiccProfile>>(data) {
                    Ok(profiles)
                        if profiles.iter().any(|profile| {
                            profile.iccid == request.iccid
                                && profile.profile_state.eq_ignore_ascii_case("enabled")
                        }) =>
                    {
                        return Ok(profiles);
                    }
                    Ok(_) => last_error = "Profile list does not show the target as enabled".to_string(),
                    Err(error) => last_error = format!("Invalid lpac profile list: {error}"),
                },
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    })
    .await;

    match result {
        Ok(profiles) => {
            let (selection_pending, selection_warning) = crate::dbus::submit_profile_auto_selection(conn).await;
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message(
                    "Profile enabled; automatic selection submitted; registration may take 1-2 minutes",
                    json!({ "iccid": request.iccid, "profiles": profiles,
                        "network_selection_submitted": true,
                        "network_selection_pending": selection_pending,
                        "network_selection_warning": selection_warning }),
                )),
            ).into_response()
        },
        Err(error) => euicc_failure(error),
    }
}

/// POST /api/euicc/refresh-sim - 手动触发 Modem 重新读取 SIM
pub async fn refresh_euicc_sim(State(conn): State<Arc<Connection>>) -> Response {
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::dbus::refresh_sim(&conn),
    )
    .await
    {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("SIM refreshed", json!({}))),
        )
            .into_response(),
        Ok(Err(error)) => euicc_failure(format!("SIM refresh failed: {error}")),
        Err(_) => euicc_failure("SIM refresh timed out"),
    }
}

/// POST /api/euicc/profile/delete - 删除 Profile
pub async fn delete_euicc_profile(
    State(_conn): State<Arc<Connection>>,
    Json(request): Json<EuiccProfileRequest>,
) -> Response {
    if let Err(error) = validate_euicc_identifier(&request.iccid) {
        return euicc_failure(error);
    }

    let args = vec![
        "profile".to_string(),
        "delete".to_string(),
        request.iccid.clone(),
    ];
    let result = with_euicc_write(async { run_lpac_json(&args).await }).await;
    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "Profile deleted",
                json!({ "iccid": request.iccid }),
            )),
        )
            .into_response(),
        Err(error) => euicc_failure(error),
    }
}

pub async fn nickname_euicc_profile(
    Json(request): Json<EuiccNicknameRequest>,
) -> Response {
    if let Err(error) = validate_euicc_identifier(&request.iccid) {
        return euicc_failure(error);
    }
    if request.nickname.chars().count() > 32 || request.nickname.chars().any(char::is_control) {
        return euicc_failure("昵称最多 32 个字符，且不能包含控制字符");
    }
    let args = vec!["profile".into(), "nickname".into(), request.iccid.clone(), request.nickname.clone()];
    match with_euicc_write(async { run_lpac_json(&args).await }).await {
        Ok(_) => Json(ApiResponse::success_with_message("昵称已保存", json!({
            "iccid": request.iccid, "nickname": request.nickname,
        }))).into_response(),
        Err(error) => euicc_failure(error),
    }
}

async fn read_euicc_notifications() -> Result<Vec<EuiccNotification>, String> {
    let data = run_lpac_json(&["notification".into(), "list".into()]).await?;
    serde_json::from_value(data).map_err(|error| format!("Invalid notification list: {error}"))
}

pub async fn list_euicc_notifications() -> Response {
    match NOTIFICATION_SNAPSHOT.get("lpac", std::time::Duration::from_secs(10), async {
        crate::serial::with_euicc_serial(read_euicc_notifications()).await.map_err(zbus::Error::Failure)
    }).await {
        Ok(notifications) => Json(ApiResponse::success_with_message("通知已读取", notifications.as_ref().clone())).into_response(),
        Err(error) => euicc_failure(error.to_string()),
    }
}

fn notification_target(request: &EuiccNotificationActionRequest) -> Result<String, String> {
    match (request.all, request.seq_number) {
        (true, None) => Ok("-a".into()),
        (false, Some(seq)) => Ok(seq.to_string()),
        _ => Err("必须指定一个通知序号，或选择全部通知".into()),
    }
}

pub async fn process_euicc_notifications(
    Json(request): Json<EuiccNotificationActionRequest>,
) -> Response {
    let target = match notification_target(&request) {
        Ok(target) => target,
        Err(error) => return euicc_failure(error),
    };
    let args = vec!["notification".into(), "process".into(), target];
    match with_euicc_write(async { run_lpac_json(&args).await }).await {
        Ok(_) => Json(ApiResponse::success_with_message("通知已发送，记录保留供自行管理", json!({}))).into_response(),
        Err(error) => euicc_failure(error),
    }
}

fn is_safe_notification(notification: &EuiccNotification) -> bool {
    matches!(notification.profile_management_operation.as_str(), "enable" | "disable")
}

pub async fn remove_euicc_notifications(
    Json(request): Json<EuiccNotificationActionRequest>,
) -> Response {
    if let Err(error) = notification_target(&request) {
        return euicc_failure(error);
    }
    let result = with_euicc_write(async {
        // Read and remove under the same serial lock. Never use remove -a:
        // it would also delete operator notifications that must be preserved.
        let notifications = read_euicc_notifications().await?;
        let mut skipped = 0;
        let targets: Vec<_> = if request.all {
            notifications.into_iter().filter(|notification| {
                if is_safe_notification(notification) { true } else { skipped += 1; false }
            }).collect()
        } else {
            let notification = notifications.into_iter()
                .find(|notification| Some(notification.seq_number) == request.seq_number)
                .ok_or_else(|| "通知已不存在，请刷新列表".to_string())?;
            if !is_safe_notification(&notification) && !request.confirm_sensitive {
                return Err(format!("删除 {} 通知需要再次确认", notification.profile_management_operation));
            }
            vec![notification]
        };
        let mut removed = Vec::new();
        let mut failed = Vec::new();
        for notification in targets {
            let args = vec!["notification".into(), "remove".into(), notification.seq_number.to_string()];
            match run_lpac_json(&args).await {
                Ok(_) => removed.push(notification.seq_number),
                Err(error) => failed.push(json!({ "seqNumber": notification.seq_number, "message": error })),
            }
        }
        Ok(json!({ "removed": removed, "failed": failed, "skipped": skipped }))
    }).await;
    match result {
        Ok(summary) => Json(ApiResponse::success_with_message("通知删除操作已完成", summary)).into_response(),
        Err(error) => euicc_failure(error),
    }
}

async fn forward_lpac_output<R>(reader: R, sender: mpsc::Sender<Bytes>, parse_events: bool)
    -> std::io::Result<Option<LpacPayload>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    let mut result = None;
    loop {
        buffer.clear();
        if reader.read_until(b'\n', &mut buffer).await? == 0 { break; }
        // Decode complete lines so UTF-8 characters split across OS reads survive.
        let text = String::from_utf8_lossy(&buffer);
        let event = json!({
            "type": "output",
            "text": text,
        });
        let _ = sender.send(Bytes::from(format!("{event}\n"))).await;
        if parse_events {
            for event in parse_lpac_events(&text) {
                if event.event_type == "lpa" { result = Some(event.payload.clone()); }
                let event = json!({ "type": "lpac", "event": event });
                let _ = sender.send(Bytes::from(format!("{event}\n"))).await;
            }
        }
    }
    Ok(result)
}

/// POST /api/euicc/profile/download - 下载 Profile 并流式返回 lpac 输出
pub async fn download_euicc_profile(
    Json(request): Json<EuiccDownloadRequest>,
) -> Response {
    let mut args = vec!["profile".to_string(), "download".to_string()];
    let activation_code = request
        .activation_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(activation_code) = activation_code {
        args.extend(["-a".to_string(), activation_code.to_string()]);
    } else {
        let smdp_address = request
            .smdp_address
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let matching_id = request
            .matching_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let (Some(smdp_address), Some(matching_id)) = (smdp_address, matching_id) else {
            return euicc_error(
                StatusCode::BAD_REQUEST,
                "Provide an activation code or both SM-DP+ address and matching ID",
            );
        };
        args.extend([
            "-s".to_string(),
            smdp_address.to_string(),
            "-m".to_string(),
            matching_id.to_string(),
        ]);
    }

    for (flag, value) in [("-c", request.confirmation_code), ("-i", request.imei)] {
        if let Some(value) = value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            args.extend([flag.to_string(), value]);
        }
    }

    let (sender, receiver) = mpsc::channel::<Bytes>(32);
    tokio::spawn(async move {
        with_euicc_write(async move {
            let spawn_result = lpac_command(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match spawn_result {
                Ok(child) => child,
                Err(error) => {
                    let event = json!({ "type": "complete", "success": false, "message": format!("Failed to start lpac: {error}") });
                    let mut line = event.to_string();
                    line.push('\n');
                    let _ = sender.send(Bytes::from(line)).await;
                    return;
                }
            };

            let stdout_task = tokio::spawn(forward_lpac_output(
                child.stdout.take().expect("stdout is piped"),
                sender.clone(),
                true,
            ));
            let stderr_task = tokio::spawn(forward_lpac_output(
                child.stderr.take().expect("stderr is piped"),
                sender.clone(),
                false,
            ));
            let status = child.wait().await;
            let stdout = stdout_task.await;
            let stderr = stderr_task.await;
            let streams_ok = matches!(&stdout, Ok(Ok(_))) && matches!(&stderr, Ok(Ok(_)));
            let result = stdout.ok().and_then(Result::ok).flatten();

            let success = matches!(status, Ok(status) if status.success())
                && streams_ok && result.as_ref().is_some_and(|payload| payload.code == 0);
            let message = match &result {
                Some(payload) if payload.code != 0 => lpac_failure(payload),
                _ if success => "eSIM 配置文件下载成功".to_string(),
                None => "LPAC 未返回最终下载结果".to_string(),
                _ => "LPAC 下载进程异常结束".to_string(),
            };
            let event = json!({ "type": "complete", "success": success, "message": message, "result": result });
            let mut line = event.to_string();
            line.push('\n');
            let _ = sender.send(Bytes::from(line)).await;
        })
        .await;
    });

    let body_stream = stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|chunk| (Ok::<Bytes, Infallible>(chunk), receiver))
    });
    let mut response = Response::new(Body::from_stream(body_stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
}

/// 处理 OPTIONS 请求（CORS 预检）
pub async fn options_handler() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

/// POST /api/at - 发送 AT 指令
///
/// # 请求体
/// ```json
/// {
///   "cmd": "AT+CGSN"
/// }
/// ```
pub async fn post_at_command(
    State(conn): State<Arc<Connection>>,
    Json(payload): Json<AtCommandRequest>,
) -> impl IntoResponse {
    let (status, body_text) = match send_at_command(&conn, &payload.cmd).await {
        Ok(result) => (StatusCode::OK, result),
        Err(e) => (StatusCode::OK, format!("Error: {}", e)),
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );

    (status, headers, body_text)
}

/// 获取主小区信息
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `cmd` - AT 指令
/// * `tech` - 网络制式
///
/// # Returns
/// 解析后的主小区信息
async fn fetch_primary_cell(
    conn: &Connection,
    cmd: &str,
    tech: &str,
) -> Result<CellInfo, String> {
    let response = send_at_read_command(conn, cmd)
        .await
        .map_err(|e| format!("Primary cell AT command failed: {}", e))?;

    let parsed = parse_at_response_to_2d_vec(&response);
    let cell = parse_primary_cell(tech, &parsed);

    Ok(cell)
}

/// 获取邻区信息列表
///
/// # Arguments
/// * `conn` - D-Bus 连接
/// * `cmd` - AT 指令
/// * `tech` - 网络制式
///
/// # Returns
/// 解析后的邻区信息列表
async fn fetch_neighbor_cells(
    conn: &Connection,
    cmd: &str,
    tech: &str,
) -> Result<Vec<CellInfo>, String> {
    let response = send_at_read_command(conn, cmd)
        .await
        .map_err(|e| format!("Neighbor cell AT command failed: {}", e))?;

    let parsed = parse_at_response_to_2d_vec(&response);
    let cells = parse_neighbor_cells(tech, &parsed);

    Ok(cells)
}

/// GET /api/cells - Get cell information
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "serving_cell": {
///       "tech": "nr",
///       "cell_id": 12345,
///       "tac": 100
///     },
///     "cells": [...]
///   }
/// }
/// ```
async fn read_cells_snapshot(conn: &Connection) -> zbus::Result<Arc<CellsResponse>> {
    crate::dbus::CELL_SNAPSHOT.get(
        &crate::dbus::snapshot_scope(conn), std::time::Duration::from_secs(3), async {
            let result = async {
                let serving_cell = get_serving_cell_info(conn)
                    .await
                    .map_err(|e| format!("Failed to get serving cell info: {}", e))?;
                let tech = serving_cell.tech.as_str();
                let cmd_config = get_cell_command_config(tech)
                    .ok_or_else(|| format!("Unsupported network type: {}", tech))?;

                // AT calls remain serial. Cache the complete result so readers
                // cannot mix independently cached primary/neighbor samples.
                let primary_cell = fetch_primary_cell(conn, cmd_config.primary, tech).await?;
                let neighbor_cells = fetch_neighbor_cells(conn, cmd_config.neighbor, tech).await?;
                let mut all_cells = vec![primary_cell];
                all_cells.extend(neighbor_cells);
                Ok::<_, String>(CellsResponse { serving_cell, cells: all_cells })
            }.await;
            result.map_err(zbus::Error::Failure)
    }).await
}

pub async fn get_cells(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    let result = read_cells_snapshot(&conn).await;
    match result {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data.as_ref().clone())),
        ),
        Err(msg) => (
            StatusCode::OK,
            Json(ApiResponse::<CellsResponse>::error(msg.to_string())),
        ),
    }
}

/// GET /api/device - 获取设备信息（来自 D-Bus Modem 接口）
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "imei": "123456789012345",
///     "manufacturer": "UNISOC",
///     "model": "UDX710",
///     "revision": "1.0.0",
///     "online": true,
///     "powered": true
///   }
/// }
/// ```
pub async fn get_device_info(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    match get_device_info_data(&conn).await {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<DeviceInfoResponse>::error(format!(
                "Failed to get device info: {}",
                e
            ))),
        ),
    }
}

/// POST /api/data - Set data connection status
///
/// # Request body
/// ```json
/// {
///   "active": true
/// }
/// ```
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Data connection updated successfully"
/// }
/// ```
///
/// # 说明
/// 每次切换数据连接状态时，会自动清空 iptables 规则（flush），
/// 以确保网络配置处于干净状态
pub async fn set_data_status(
    State(conn): State<Arc<Connection>>,
    Json(payload): Json<DataConnectionRequest>,
) -> impl IntoResponse {
    let _modem_change = crate::serial::modem_change();
    // 1. 先清空 iptables 规则
    if let Err(_e) = flush_iptables().await {
        // 清空规则失败不应阻止数据连接操作，静默处理
    }

    // 2. 设置数据连接状态
    match set_data_connection(&conn, payload.active).await {
        Ok(_) => {
            
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message(
                    "Data connection updated successfully",
                    DataConnectionResponse { active: payload.active },
                )),
            )
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<DataConnectionResponse>::error(format!(
                "Failed to set data connection: {}",
                e
            ))),
        ),
    }
}

/// GET /api/data - Get data connection status
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "active": true
///   }
/// }
/// ```
pub async fn get_data_status(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    match get_data_connection_status(&conn).await {
        Ok(active) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "Success",
                DataConnectionResponse { active },
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<DataConnectionResponse>::error(format!(
                "Failed to get data connection status: {}",
                e
            ))),
        ),
    }
}

/// GET /api/roaming - Get roaming status
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "roaming_allowed": true,
///     "is_roaming": false
///   }
/// }
/// ```
pub async fn get_roaming_status_handler(
    State(conn): State<Arc<Connection>>,
) -> impl IntoResponse {
    match get_roaming_status(&conn).await {
        Ok((roaming_allowed, is_roaming)) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "Success",
                RoamingResponse { roaming_allowed, is_roaming },
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<RoamingResponse>::error(format!(
                "Failed to get roaming status: {}",
                e
            ))),
        ),
    }
}

/// POST /api/roaming - Set roaming allowed
///
/// # Request body
/// ```json
/// {
///   "allowed": true
/// }
/// ```
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Roaming enabled successfully",
///   "data": {
///     "roaming_allowed": true,
///     "is_roaming": false
///   }
/// }
/// ```
pub async fn set_roaming_status_handler(
    State(conn): State<Arc<Connection>>,
    Json(payload): Json<RoamingRequest>,
) -> impl IntoResponse {
    let _modem_change = crate::serial::modem_change();
    match set_roaming_allowed(&conn, payload.allowed).await {
        Ok(_) => {
            // Read back the status to confirm
            match get_roaming_status(&conn).await {
                Ok((roaming_allowed, is_roaming)) => {
                    let msg = if payload.allowed {
                        "Roaming enabled successfully"
                    } else {
                        "Roaming disabled successfully"
                    };
                    (
                        StatusCode::OK,
                        Json(ApiResponse::success_with_message(
                            msg,
                            RoamingResponse { roaming_allowed, is_roaming },
                        )),
                    )
                }
                Err(e) => (
                    StatusCode::OK,
                    Json(ApiResponse::<RoamingResponse>::error(format!(
                        "Roaming setting updated, but failed to read status: {}",
                        e
                    ))),
                ),
            }
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<RoamingResponse>::error(format!(
                "Failed to set roaming: {}",
                e
            ))),
        ),
    }
}

/// POST /api/airplane-mode - Set airplane mode
///
/// # Request body
/// ```json
/// {
///   "enabled": true
/// }
/// ```
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Airplane mode enabled successfully",
///   "data": {
///     "enabled": true,
///     "powered": true,
///     "online": false
///   }
/// }
/// ```
pub async fn set_airplane_mode_handler(
    State(conn): State<Arc<Connection>>,
    Json(payload): Json<AirplaneModeRequest>,
) -> impl IntoResponse {
    let _modem_change = crate::serial::modem_change();
    match set_airplane_mode(&conn, payload.enabled).await {
        Ok(status) => {
            let msg = if payload.enabled {
                "Airplane mode enabled successfully"
            } else {
                "Airplane mode disabled successfully"
            };
            (StatusCode::OK, Json(ApiResponse::success_with_message(msg, status)))
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<AirplaneModeResponse>::error(format!(
                "Failed to set airplane mode: {}",
                e
            ))),
        ),
    }
}

/// GET /api/airplane-mode - Get airplane mode status
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "enabled": false,
///     "powered": true,
///     "online": true
///   }
/// }
/// ```
pub async fn get_airplane_mode_handler(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    match get_airplane_mode(&conn).await {
        Ok(status) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", status)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<AirplaneModeResponse>::error(format!(
                "Failed to get airplane mode status: {}",
                e
            ))),
        ),
    }
}

/// GET /api/health - Health check endpoint
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Service is running"
/// }
/// ```
pub async fn health_check() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "message": "Service is running",
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

/// GET /api/sim - Get SIM card information
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "present": true,
///     "pin_required": "none",
///     "service_center_address": "+8613800200569",
///     "subscriber_identity": "460123456789012"
///   }
/// }
/// ```
pub async fn get_sim_info(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    match get_sim_info_data(&conn).await {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<SimInfoResponse>::error(format!(
                "Failed to get SIM info: {}",
                e
            ))),
        ),
    }
}

/// GET /api/network - Get network information
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "operator_name": "CMCC",
///     "registration_status": "registered",
///     "technology_preference": "NR 5G/LTE auto",
///     "signal_strength": 85
///   }
/// }
/// ```
pub async fn get_network_info(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    match get_network_info_data(&conn).await {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<NetworkInfoResponse>::error(format!(
                "Failed to get network info: {}",
                e
            ))),
        ),
    }
}

/// GET /api/qos - Get QoS information
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "qci": 5,
///     "dl_speed": 30000,
///     "ul_speed": 30000
///   }
/// }
/// ```
pub async fn get_qos_info(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    match get_qos_info_data(&conn).await {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<QosInfoResponse>::error(format!(
                "Failed to get QoS info: {}",
                e
            ))),
        ),
    }
}

/// 读取温度传感器数据（内部工具函数）
fn read_temperature_sensors() -> Vec<ThermalZone> {
    use std::fs;
    use std::path::Path;

    let thermal_path = Path::new("/sys/class/thermal");
    let mut sensors = Vec::new();

    if let Ok(entries) = fs::read_dir(thermal_path) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();

            if name.starts_with("thermal_zone") {
                let zone_path = entry.path();
                
                let sensor_type = fs::read_to_string(zone_path.join("type"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

                let temperature = fs::read_to_string(zone_path.join("temp"))
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .map(|t| t as f64 / 1000.0)
                    .unwrap_or(0.0);

                sensors.push(ThermalZone {
                    zone: name.to_string(),
                    sensor_type,
                    temperature,
                });
            }
        }
    }

    sensors.sort_by(|a, b| a.zone.cmp(&b.zone));
    sensors
}

/// 获取USB模式名称
fn get_mode_name(mode: Option<u8>) -> String {
    match mode {
        Some(1) => "CDC-NCM".to_string(),
        Some(2) => "CDC-ECM".to_string(),
        Some(3) => "RNDIS".to_string(),
        _ => "Unknown".to_string(),
    }
}

/// GET /api/usb-mode - 查询USB模式配置
///
/// 返回当前硬件实际运行的模式、永久配置和临时配置
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "current_mode": 1,
///     "current_mode_name": "CDC-NCM",
///     "permanent_mode": 1,
///     "temporary_mode": null,
///     "needs_reboot": true,
///     "read_mode": "hardware"
///   }
/// }
/// ```
pub async fn get_usb_mode() -> impl IntoResponse {
    // 读取 USB 模式配置（包括硬件状态和配置文件）
    match usb_switch::get_usb_mode_config() {
        Ok(config) => {
            let response = UsbModeResponse {
                current_mode: config.current_mode,
                current_mode_name: get_mode_name(config.current_mode),
                permanent_mode: config.permanent_mode,
                temporary_mode: config.temporary_mode,
                needs_reboot: true, // 始终需要重启
                read_mode: "hardware".to_string(),
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message("Success", response)),
            )
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<UsbModeResponse>::error(format!(
                "Failed to get USB mode: {}",
                e
            ))),
        ),
    }
}

/// POST /api/usb-mode - 设置USB模式配置（写入配置文件，重启后生效）
///
/// # Request body
/// ```json
/// {
///   "mode": 1,
///   "permanent": true  // true=永久模式, false=临时模式
/// }
/// ```
///
/// # 支持的模式
/// - 1: CDC-NCM
/// - 2: CDC-ECM
/// - 3: RNDIS
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "USB mode configuration saved. Please reboot to apply changes."
/// }
/// ```
pub async fn set_usb_mode(Json(payload): Json<SetUsbModeRequest>) -> impl IntoResponse {
    // 验证模式值
    if !(1..=3).contains(&payload.mode) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error(
                "Invalid mode: must be 1 (CDC-NCM), 2 (CDC-ECM), or 3 (RNDIS)",
            )),
        );
    }
    
    // 写入配置文件
    match usb_switch::set_usb_mode_config(payload.mode, payload.permanent) {
        Ok(_) => {
            let mode_name = get_mode_name(Some(payload.mode));
            let mode_type = if payload.permanent { "永久" } else { "临时" };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message(
                    format!("USB 模式已设置为 {} ({})，请重启设备后生效", mode_name, mode_type),
                    (),
                )),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::error(format!("Failed to set USB mode: {}", e))),
        ),
    }
}

/// POST /api/usb-advance - 热切换USB模式（立即生效，无需重启）
///
/// 高级接口：直接操作 configfs 实现热切换
///
/// # Request body
/// ```json
/// {
///   "mode": 1
/// }
/// ```
///
/// # 支持的模式
/// - 1: CDC-NCM
/// - 2: CDC-ECM
/// - 3: RNDIS
///
/// # 注意事项
/// - 热切换会导致 USB 连接短暂断开（约 1-2 秒）
/// - macOS 可能需要更长时间识别新设备
/// - 模式 3 (RNDIS) 在 macOS/Linux 上可能需要额外驱动
/// - 建议使用模式 1 (NCM) 以获得最佳跨平台兼容性
pub async fn set_usb_mode_advanced(Json(payload): Json<SetUsbModeRequest>) -> impl IntoResponse {
    // 验证模式值
    if !(1..=3).contains(&payload.mode) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error(
                "Invalid mode: must be 1 (CDC-NCM), 2 (CDC-ECM), or 3 (RNDIS)",
            )),
        );
    }
    
    // 执行热切换
    match usb_switch::switch_usb_mode_advanced(payload.mode) {
        Ok(_) => {
            let mode_name = get_mode_name(Some(payload.mode));
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message(
                    format!("USB 模式已热切换为 {} (无需重启)", mode_name),
                    (),
                )),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::error(format!("USB 模式切换失败: {}", e))),
        ),
    }
}

/// GET /api/stats/cpu - 获取 CPU 信息
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "core_count": 2,
///     "cores": [
///       {
///         "processor": 0,
///         "bogomips": "52.00",
///         "features": ["fp", "asimd", "evtstrm", "aes"],
///         "implementer": "0x41",
///         "architecture": "8",
///         "variant": "0x1",
///         "part": "0xd05",
///         "revision": "0"
///       }
///     ],
///     "hardware": "Unisoc UDX710",
///     "serial": "0000000000000000",
///     "model_name": "ARM Cortex-A55"
///   }
/// }
/// ```
pub async fn get_cpu_info() -> impl IntoResponse {
    match read_cpu_info() {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data)),
        ),
        Err(msg) => (
            StatusCode::OK,
            Json(ApiResponse::<CpuInfo>::error(msg)),
        ),
    }
}

/// GET /api/stats/system - 获取综合系统状态（包括网速、内存、运行时间）
///
/// 一次性获取所有系统监控信息，适合仪表板使用
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "network_speed": { ... },
///     "memory": { ... },
///     "uptime": { ... }
///   }
/// }
/// ```
async fn sample_system_stats() -> Result<SystemStatsResponse, String> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;
    
    async {
        // 获取网速和 CPU 使用率（并行异步采样）
        let interfaces = get_active_interfaces()?;
        let mut first_samples = Vec::new();
        for interface in &interfaces {
            match read_interface_stats(interface) {
                Ok((rx, tx)) => first_samples.push((interface.clone(), rx, tx)),
                Err(_) => continue,
            }
        }
        
        // 同时开始 CPU 采样
        let cpu_usage_future = sample_cpu_usage();
        
        let start = Instant::now();
        // 等待 1 秒采样网速（CPU 采样只需 200ms，会先完成）
        let cpu_usage = cpu_usage_future.await.unwrap_or(0.0);
        
        // 补足剩余时间到 1 秒
        let elapsed_so_far = start.elapsed();
        if elapsed_so_far < Duration::from_secs(1) {
            sleep(Duration::from_secs(1) - elapsed_so_far).await;
        }
        let elapsed = start.elapsed().as_secs_f64();
        
        let mut speed_data = Vec::new();
        for (interface, rx1, tx1) in first_samples {
            if let Ok((rx2, tx2)) = read_interface_stats(&interface) {
                let rx_speed = ((rx2.saturating_sub(rx1)) as f64 / elapsed) as u64;
                let tx_speed = ((tx2.saturating_sub(tx1)) as f64 / elapsed) as u64;
                
                speed_data.push(NetworkSpeed {
                    interface,
                    rx_bytes_per_sec: rx_speed,
                    tx_bytes_per_sec: tx_speed,
                    total_rx_bytes: rx2,
                    total_tx_bytes: tx2,
                });
            }
        }
        
        // 获取内存信息
        let (total, available, cached, buffers) = read_memory_info()?;
        let used = total.saturating_sub(available);
        let used_percent = if total > 0 {
            (used as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        
        // 获取磁盘信息
        let disk = read_disk_info();
        
        // 获取 CPU 负载（使用之前采样的 CPU 使用率）
        let mut cpu_load = read_cpu_load_sync().unwrap_or_default();
        cpu_load.load_percent = cpu_usage;
        
        // 获取运行时间
        let (uptime, idle) = read_uptime()?;
        let formatted = format_uptime(uptime);
        
        // 获取系统信息（uname）
        let system_info = SYSTEM_INFO_SNAPSHOT.get_local(Duration::from_secs(300), async {
            read_system_info().map_err(zbus::Error::Failure)
        }).await.map_err(|error| error.to_string())?.as_ref().clone();
        
        // 获取温度
        let temperature = read_temperature_sensors();
        
        // 获取 USB 模式
        let usb_mode = match usb_switch::get_usb_mode_config() {
            Ok(config) => UsbModeResponse {
                current_mode: config.current_mode,
                current_mode_name: get_mode_name(config.current_mode),
                permanent_mode: config.permanent_mode,
                temporary_mode: config.temporary_mode,
                needs_reboot: true,
                read_mode: "hardware".to_string(),
            },
            Err(_) => UsbModeResponse::default(),
        };
        
        Ok(SystemStatsResponse {
            network_speed: NetworkSpeedResponse {
                interfaces: speed_data,
                interval_seconds: elapsed,
            },
            memory: MemoryInfo {
                total_bytes: total,
                available_bytes: available,
                used_bytes: used,
                used_percent,
                cached_bytes: cached,
                buffers_bytes: buffers,
            },
            disk,
            cpu_load,
            uptime: UptimeInfo {
                uptime_seconds: uptime,
                idle_seconds: idle,
                uptime_formatted: formatted,
            },
            system_info,
            temperature,
            usb_mode,
        })
    }
    .await
}

pub async fn get_system_stats() -> impl IntoResponse {
    let result = STATS_SNAPSHOT.get_local(std::time::Duration::from_secs(1), async {
        sample_system_stats().await.map_err(zbus::Error::Failure)
    }).await;
    
    match result {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data.as_ref().clone())),
        ),
        Err(msg) => (
            StatusCode::OK,
            Json(ApiResponse::<SystemStatsResponse>::error(msg.to_string())),
        ),
    }
}

/// GET /api/location/cell-info - 获取基站定位参数
/// 
/// 返回格式化的基站定位参数，可用于调用第三方定位API（如Google Geolocation、OpenCellID等）
async fn read_cell_location_info(conn: &Connection) -> zbus::Result<CellLocationResponse> {
    // 获取网络信息（MCC、MNC）
    let network_info = get_network_info_data(conn).await?;

    // 检查是否有 MCC 和 MNC
    let mcc = match network_info.mcc {
        Some(ref m) if !m.is_empty() => m.clone(),
        _ => {
            return Err(zbus::Error::Failure("ModemReadDeferred: waiting for registered MCC".into()));
        }
    };

    let mnc = match network_info.mnc {
        Some(ref m) if !m.is_empty() => m.clone(),
        _ => {
            return Err(zbus::Error::Failure("ModemReadDeferred: waiting for registered MNC".into()));
        }
    };

    // 获取服务小区信息（TAC、CID）
    let mut serving_cell = get_serving_cell_info(conn).await?;
    if serving_cell.cell_id == 0 {
        return Err(zbus::Error::Failure("ModemReadDeferred: waiting for serving cell identity".into()));
    }

    // 获取详细的小区信息（信号强度等）
    match get_cell_command_config(&serving_cell.tech) {
        Some(_) => {},
        None => {
            // 如果不支持当前网络制式，返回基本信息（不含信号强度）
            let cell_info = if serving_cell.cell_id > 0 {
                Some(CellLocationInfo {
                    mcc: mcc.clone(),
                    mnc: mnc.clone(),
                    lac: serving_cell.tac,
                    cid: serving_cell.cell_id,
                    signal_strength: -100, // 默认信号强度
                    radio_type: serving_cell.tech.clone(),
                    arfcn: None,
                    pci: None,
                    rsrq: None,
                    sinr: None,
                })
            } else {
                None
            };

            let usage_hint = format!(
                "Cell location data available (limited). Unsupported network type: {}.\n\
                Network: {} (MCC={}, MNC={}), Cell ID={}, TAC={}",
                serving_cell.tech,
                network_info.operator_name,
                mcc,
                mnc,
                serving_cell.cell_id,
                serving_cell.tac
            );

            return Ok(CellLocationResponse {
                available: cell_info.is_some(), cell_info, neighbor_cells: vec![], usage_hint,
            });
        }
    };

    // 获取主小区和邻区详细信息
    // Share the complete primary/neighbor sample with /api/cells. Never turn a
    // transient read failure into a successful empty/default location result.
    let snapshot = read_cells_snapshot(conn).await?;
    serving_cell = snapshot.serving_cell.clone();
    let serving_cell_detail = snapshot.cells.iter().find(|cell| cell.is_serving);
    let neighbor_cells: Vec<_> = snapshot.cells.iter().filter(|cell| !cell.is_serving).collect();

    // 构建主服务小区定位信息
    let cell_info = if serving_cell.cell_id > 0 {
        let signal_strength = if let Some(ref detail) = serving_cell_detail {
            // RSRP 是原始值×100，需要除以100
            detail.rsrp.parse::<i32>().unwrap_or(-140) / 100
        } else {
            -100 // 默认信号强度
        };

        let arfcn = serving_cell_detail.as_ref().and_then(|d| d.arfcn.parse::<u32>().ok());
        let pci = serving_cell_detail.as_ref().and_then(|d| d.pci.parse::<u32>().ok());
        let rsrq = serving_cell_detail.as_ref().and_then(|d| d.rsrq.parse::<i32>().ok().map(|v| v / 100));
        let sinr = serving_cell_detail.as_ref().and_then(|d| d.sinr.parse::<i32>().ok().map(|v| v / 100));

        Some(CellLocationInfo {
            mcc: mcc.clone(),
            mnc: mnc.clone(),
            lac: serving_cell.tac,
            cid: serving_cell.cell_id,
            signal_strength,
            radio_type: serving_cell.tech.clone(),
            arfcn,
            pci,
            rsrq,
            sinr,
        })
    } else {
        None
    };

    // 构建邻区定位信息列表
    let neighbor_location_cells: Vec<CellLocationInfo> = neighbor_cells
        .iter()
        .filter_map(|cell| {
            let signal_strength = cell.rsrp.parse::<i32>().unwrap_or(-140) / 100;
            let pci = cell.pci.parse::<u32>().ok()?;
            
            Some(CellLocationInfo {
                mcc: mcc.clone(),
                mnc: mnc.clone(),
                lac: serving_cell.tac, // 邻区通常与主小区在同一 TAC
                cid: 0, // 邻区可能没有完整的 CID，只有 PCI
                signal_strength,
                radio_type: cell.tech.clone(),
                arfcn: cell.arfcn.parse::<u32>().ok(),
                pci: Some(pci),
                rsrq: cell.rsrq.parse::<i32>().ok().map(|v| v / 100),
                sinr: cell.sinr.parse::<i32>().ok().map(|v| v / 100),
            })
        })
        .collect();

    // 构建使用建议
    let usage_hint = if cell_info.is_some() {
        format!(
            "Cell location data available. You can use this data with geolocation APIs:\n\
            - Google Geolocation API: https://developers.google.com/maps/documentation/geolocation/overview\n\
            - OpenCellID: https://opencellid.org/\n\
            - Unwired Labs: https://unwiredlabs.com/\n\
            Network: {} (MCC={}, MNC={}), Cell ID={}, TAC={}, Signal={}dBm",
            network_info.operator_name,
            mcc,
            mnc,
            serving_cell.cell_id,
            serving_cell.tac,
            cell_info.as_ref().map(|c| c.signal_strength).unwrap_or(-100)
        )
    } else {
        "Cell location unavailable: No serving cell found.".to_string()
    };

    let response = CellLocationResponse {
        available: cell_info.is_some(),
        cell_info,
        neighbor_cells: neighbor_location_cells,
        usage_hint,
    };

    if !response.available {
        return Err(zbus::Error::Failure("ModemReadDeferred: waiting for serving cell identity".into()));
    }
    Ok(response)
}

pub async fn get_cell_location_info(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    let result = crate::dbus::LOCATION_SNAPSHOT.get(
        &crate::dbus::snapshot_scope(&conn), std::time::Duration::from_secs(30),
        read_cell_location_info(&conn),
    ).await;
    let response = match result {
        Ok(data) => ApiResponse::success_with_message("Success", data.as_ref().clone()),
        Err(error) => ApiResponse::<CellLocationResponse>::error(format!("Failed to get cell location: {}", error)),
    };
    (StatusCode::OK, Json(response))
}

/// GET /api/network/interfaces - 获取所有网络接口详细信息
/// 
/// 返回所有网络接口的详细信息，包括：
/// - 接口名称、状态、MAC地址、MTU
/// - IPv4和IPv6地址列表
/// - 公网/内网地址分类
/// - 流量统计（接收/发送字节数、包数、错误数）
pub async fn get_network_interfaces_info() -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(|| {
        let interfaces = read_network_interfaces()?;
        let total_count = interfaces.len();
        
        Ok::<_, String>(NetworkInterfacesResponse {
            interfaces,
            total_count,
        })
    })
    .await;
    
    match result {
        Ok(Ok(data)) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data)),
        ),
        Ok(Err(msg)) => (
            StatusCode::OK,
            Json(ApiResponse::<NetworkInterfacesResponse>::error(msg)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<NetworkInterfacesResponse>::error(format!(
                "Task execution failed: {}",
                e
            ))),
        ),
    }
}

/// GET /api/radio-mode - 获取当前射频模式
///
/// # 返回
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "mode": "auto",
///     "technology_preference": "NR 5G/LTE auto"
///   }
/// }
/// ```
pub async fn get_radio_mode_handler(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    match get_radio_mode(&conn).await {
        Ok(data) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", data)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<RadioModeResponse>::error(format!(
                "Failed to get radio mode: {}",
                e
            ))),
        ),
    }
}

/// POST /api/radio-mode - 设置射频模式
///
/// # 请求体
/// ```json
/// {
///   "mode": "auto"  // auto | lte | nr
/// }
/// ```
///
/// # 说明
/// - auto: 4G/5G 自动切换
/// - lte: 仅 4G LTE
/// - nr: 仅 5G NR
pub async fn set_radio_mode_handler(
    State(conn): State<Arc<Connection>>,
    Json(payload): Json<RadioModeRequest>,
) -> impl IntoResponse {
    match set_radio_mode(&conn, payload.mode.clone()).await {
        Ok(_) => {
            let mode_str = match payload.mode {
                RadioMode::Auto => "4G/5G Auto",
                RadioMode::LteOnly => "4G LTE Only",
                RadioMode::NrOnly => "5G NR Only",
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message(
                    format!("Radio mode set to {}", mode_str),
                    json!({}),
                )),
            )
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<serde_json::Value>::error(format!(
                "Failed to set radio mode: {}",
                e
            ))),
        ),
    }
}

/// GET /api/band-lock - 获取当前频段锁定状态
///
/// # 返回
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "locked": true,
///     "lte_fdd_bands": [1, 3, 8],
///     "lte_tdd_bands": [38, 40, 41],
///     "nr_fdd_bands": [1, 28],
///     "nr_tdd_bands": [41, 77, 78, 79]
///   }
/// }
/// ```
async fn read_band_lock(conn: &Connection) -> zbus::Result<BandLockStatus> {
    // 读取 LTE 频段锁定状态
    let lte_result = read_lock_command(conn, "AT+SPLBAND=0", "+SPLBAND:").await;
    let (lte_fdd_mask, lte_tdd_mask, lte_raw) = match lte_result {
        Ok(response) => {
            let (fdd, tdd) = parse_splband_lte_response(&response);
            (fdd, tdd, Some(response))
        }
        Err(e) => return Err(e),
    };

    // 读取 NR 频段锁定状态
    let nr_result = read_lock_command(conn, "AT+SPLBAND=3", "+SPLBAND:").await;
    let (nr_fdd_mask, nr_tdd_mask, nr_raw) = match nr_result {
        Ok(response) => {
            let (fdd, tdd) = parse_splband_nr_response(&response);
            (fdd, tdd, Some(response))
        }
        Err(e) => return Err(e),
    };

    // UDX710 设备支持的全部频段掩码
    // LTE: FDD=149 (B1+B3+B5+B8), TDD=320 (B39+B41)
    // NR: FDD=517 (N1+N3+N28), TDD=912 (N41+N77+N78+N79)
    const LTE_FDD_ALL: u16 = 149;
    const LTE_TDD_ALL: u16 = 320;
    const NR_FDD_ALL: u16 = 517;
    const NR_TDD_ALL: u16 = 912;
    
    // 判断是否有频段锁定
    // 如果返回的频段等于设备支持的全部频段，则认为"未锁定"（全部可用）
    // 如果返回 0 或小于全部，则认为"已锁定"（限制了可用频段）
    let lte_is_all_or_zero = (lte_fdd_mask == LTE_FDD_ALL && lte_tdd_mask == LTE_TDD_ALL) 
                            || (lte_fdd_mask == 0 && lte_tdd_mask == 0);
    let nr_is_all_or_zero = (nr_fdd_mask == NR_FDD_ALL && nr_tdd_mask == NR_TDD_ALL)
                           || (nr_fdd_mask == 0 && nr_tdd_mask == 0);
    let locked = !(lte_is_all_or_zero && nr_is_all_or_zero);
    
    // 将位掩码转换为频段号列表
    // 未锁定时返回空数组（前端显示为"未锁定模式"）
    // 已锁定时返回具体频段列表（前端显示为"自定义锁定模式"）
    let (lte_fdd_bands, lte_tdd_bands, nr_fdd_bands, nr_tdd_bands) = if !locked {
        // 未锁定：返回空数组
        (vec![], vec![], vec![], vec![])
    } else {
        // 已锁定：返回具体频段
        (
            bitmask_to_bands(lte_fdd_mask, 1),    // LTE FDD: B1-B16
            bitmask_to_bands(lte_tdd_mask, 33),   // LTE TDD: B33-B48
            bitmask_to_bands(nr_fdd_mask, 100),   // NR FDD: 展锐特殊映射
            bitmask_to_bands(nr_tdd_mask, 41),    // NR TDD: 展锐特殊映射
        )
    };

    // 构建调试信息
    let raw_response = Some(format!(
        "LTE(fdd={},tdd={}): {}\nNR(fdd={},tdd={}): {}",
        lte_fdd_mask, lte_tdd_mask, lte_raw.unwrap_or_default().trim(),
        nr_fdd_mask, nr_tdd_mask, nr_raw.unwrap_or_default().trim()
    ));

    let status = BandLockStatus {
        locked,
        lte_fdd_bands,
        lte_tdd_bands,
        nr_fdd_bands,
        nr_tdd_bands,
        raw_response,
    };

    Ok(status)
}

pub async fn get_band_lock_handler(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    let response = match BAND_LOCK_SNAPSHOT.get(&crate::dbus::snapshot_scope(&conn),
        std::time::Duration::from_secs(30), read_band_lock(&conn)).await {
        Ok(status) => ApiResponse::success_with_message("Success", status.as_ref().clone()),
        Err(error) => ApiResponse::<BandLockStatus>::error(error.to_string()),
    };
    (StatusCode::OK, Json(response))
}

/// POST /api/system/reboot - 系统重启
///
/// # Request body（可选）
/// ```json
/// {
///   "delay_seconds": 3  // 延迟秒数，默认为3秒
/// }
/// ```
///
/// # Response example
/// ```json
/// {
///   "status": "ok",
///   "message": "System will reboot in 3 seconds"
/// }
/// ```
pub async fn system_reboot(
    Json(payload): Json<Option<SystemRebootRequest>>,
) -> impl IntoResponse {
    let delay = payload.map(|p| p.delay_seconds).unwrap_or(3);
    
    // 使用 tokio 异步执行重启命令
    tokio::spawn(async move {
        // 等待指定的延迟时间
        tokio::time::sleep(tokio::time::Duration::from_secs(delay as u64)).await;
        
        // 执行重启命令
        let _ = Command::new("reboot").output();
    });
    
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_message(
            format!("系统将在 {} 秒后重启", delay),
            json!({"delay_seconds": delay}),
        )),
    )
}

/// POST /api/band-lock - 设置频段锁定
///
/// # 请求体
/// ```json
/// {
///   "lte_fdd_bands": [1, 3, 8],
///   "lte_tdd_bands": [38, 40, 41],
///   "nr_fdd_bands": [1, 28],
///   "nr_tdd_bands": [41, 77, 78, 79]
/// }
/// ```
///
/// # 说明
/// - 传入空数组表示不锁定对应类型的频段
/// - 所有数组都为空时，表示解除所有频段锁定
/// - LTE FDD: B1-B16, TDD: B33-B48
/// - NR FDD: N1-N16, TDD: N41-N56 (实际支持 N41-N79)
pub async fn set_band_lock_handler(
    State(conn): State<Arc<Connection>>,
    Json(payload): Json<BandLockRequest>,
) -> impl IntoResponse {
    // LTE 频段锁定
    let lte_fdd_mask = bands_to_bitmask(&payload.lte_fdd_bands, 1);
    let lte_tdd_mask = bands_to_bitmask(&payload.lte_tdd_bands, 33);
    
    if lte_fdd_mask != 0 || lte_tdd_mask != 0 {
        let lte_cmd = build_splband_lte_command(lte_fdd_mask, lte_tdd_mask);
        if let Err(e) = send_at_command(&conn, &lte_cmd).await {
            return (
                StatusCode::OK,
                Json(ApiResponse::<serde_json::Value>::error(format!(
                    "Failed to set LTE band lock: {}",
                    e
                ))),
            );
        }
    }

    // NR 频段锁定
    let nr_fdd_mask = bands_to_bitmask(&payload.nr_fdd_bands, 100); // NR FDD: 展锐特殊映射
    let nr_tdd_mask = bands_to_bitmask(&payload.nr_tdd_bands, 41);  // NR TDD: 展锐特殊映射
    
    if nr_fdd_mask != 0 || nr_tdd_mask != 0 {
        let nr_cmd = build_splband_nr_command(nr_fdd_mask, nr_tdd_mask);
        if let Err(e) = send_at_command(&conn, &nr_cmd).await {
            return (
                StatusCode::OK,
                Json(ApiResponse::<serde_json::Value>::error(format!(
                    "Failed to set NR band lock: {}",
                    e
                ))),
            );
        }
    }

    // 如果所有频段都为空，则解除锁定
    if payload.lte_fdd_bands.is_empty()
        && payload.lte_tdd_bands.is_empty()
        && payload.nr_fdd_bands.is_empty()
        && payload.nr_tdd_bands.is_empty()
    {
        let mut lte_unlocked = false;
        let mut nr_unlocked = false;
        
        // 先读取当前 LTE 锁定状态
        let lte_result = send_at_command(&conn, "AT+SPLBAND=0").await;
        if let Ok(lte_response) = lte_result {
            let (lte_fdd_mask, lte_tdd_mask) = parse_splband_lte_response(&lte_response);
            
            // 只有当前有 LTE 锁定时才执行解锁
            if lte_fdd_mask != 0 || lte_tdd_mask != 0 {
                // 格式: AT+SPLBAND=1,0,<TDD>,0,<FDD>,0 (6 参数)
                if let Err(e) = send_at_command(&conn, "AT+SPLBAND=1,0,0,0,0,0").await {
                    return (
                        StatusCode::OK,
                        Json(ApiResponse::<serde_json::Value>::error(format!(
                            "Failed to unlock LTE bands: {}",
                            e
                        ))),
                    );
                }
                lte_unlocked = true;
            }
        }
        
        // 先读取当前 NR 锁定状态
        let nr_result = send_at_command(&conn, "AT+SPLBAND=3").await;
        if let Ok(nr_response) = nr_result {
            let (nr_fdd_mask, nr_tdd_mask) = parse_splband_nr_response(&nr_response);
            
            // 只有当前有 NR 锁定时才执行解锁
            if nr_fdd_mask != 0 || nr_tdd_mask != 0 {
                if let Err(e) = send_at_command(&conn, "AT+SPLBAND=2,0,0,0,0").await {
                    return (
                        StatusCode::OK,
                        Json(ApiResponse::<serde_json::Value>::error(format!(
                            "Failed to unlock NR bands: {}",
                            e
                        ))),
                    );
                }
                nr_unlocked = true;
            }
        }
        
        // 根据实际执行的解锁操作返回友好的提示信息
        let message = if lte_unlocked || nr_unlocked {
            if lte_unlocked && nr_unlocked {
                "已解除所有频段锁定（LTE + NR）"
            } else if lte_unlocked {
                "已解除 LTE 频段锁定（NR 未锁定）"
            } else {
                "已解除 NR 频段锁定（LTE 未锁定）"
            }
        } else {
            "当前没有锁定的频段，无需解锁"
        };
        
        return (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                message,
                json!({}),
            )),
        );
    }

    // 生成友好的提示信息
    let has_lte = lte_fdd_mask != 0 || lte_tdd_mask != 0;
    let has_nr = nr_fdd_mask != 0 || nr_tdd_mask != 0;
    let message = if has_lte && has_nr {
        "已同时锁定 LTE 和 NR 频段"
    } else if has_lte {
        "LTE 频段锁定已应用"
    } else {
        "NR 频段锁定已应用"
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_message(
            message,
            json!({}),
        )),
    )
}

// ============ 小区锁定 API ============
// 使用 AT+SPFORCEFRQ 指令实现小区锁定
// 发现来源：通过 dbus-monitor 监听实际锁频操作

use crate::models::{CellLockStatusResponse, CellLockRatStatus, CellLockRequest, CellUnlockRequest};

/// SPFORCEFRQ 网络类型常量
const FORCEFRQ_TYPE_LTE: u8 = 12;
const FORCEFRQ_TYPE_NR: u8 = 16;

/// 获取 RAT 类型名称
fn get_rat_name(rat: u8) -> String {
    match rat {
        12 => "LTE".to_string(),
        16 => "NR".to_string(),
        _ => format!("Unknown({})", rat),
    }
}

/// 解析 AT+SPFORCEFRQ 查询响应
/// 
/// 响应格式:
/// - 未锁定: +SPFORCEFRQ: 16,3
/// - 已锁定: +SPFORCEFRQ: 16,3,633984,597
fn parse_spforcefrq_query_response(response: &str, rat: u8) -> CellLockRatStatus {
    let prefix = format!("+SPFORCEFRQ: {},3", rat);
    
    if let Some(line) = response.lines().find(|l| l.starts_with(&prefix)) {
        let data = line.strip_prefix(&format!("+SPFORCEFRQ: {},3", rat)).unwrap_or("");
        let data = data.trim_start_matches(',');
        
        if data.is_empty() {
            // 未锁定
            CellLockRatStatus {
                rat,
                rat_name: get_rat_name(rat),
                enabled: false,
                lock_type: 0,
                pci: None,
                arfcn: None,
            }
        } else {
            // 已锁定，解析 arfcn,pci
            let parts: Vec<&str> = data.split(',').collect();
            let arfcn = parts.first().and_then(|s| s.trim().parse::<u32>().ok());
            let pci = parts.get(1).and_then(|s| s.trim().parse::<u16>().ok());
            
            CellLockRatStatus {
                rat,
                rat_name: get_rat_name(rat),
                enabled: arfcn.is_some() && pci.is_some(),
                lock_type: 3,
                pci,
                arfcn,
            }
        }
    } else {
        // 解析失败，返回未锁定状态
        CellLockRatStatus {
            rat,
            rat_name: get_rat_name(rat),
            enabled: false,
            lock_type: 0,
            pci: None,
            arfcn: None,
        }
    }
}

/// GET /api/cell-lock - 获取小区锁定状态
/// 
/// 使用 AT+SPFORCEFRQ=<type>,3 查询锁定状态
/// 
/// ## 响应示例
/// ```json
/// {
///   "status": "ok",
///   "message": "Success",
///   "data": {
///     "rat_status": [
///       { "rat": 16, "rat_name": "NR", "enabled": true, "lock_type": 3, "pci": 597, "arfcn": 633984 }
///     ],
///     "any_locked": true
///   }
/// }
/// ```
async fn read_lock_command(conn: &Connection, command: &str, prefix: &str) -> zbus::Result<String> {
    let response = send_at_read_command(conn, command).await?;
    if !response.lines().any(|line| line.trim() == "OK") || !response.contains(prefix) {
        return Err(zbus::Error::Failure(format!("{command} returned an invalid response: {}", response.trim())));
    }
    Ok(response)
}

async fn read_cell_lock(conn: &Connection) -> zbus::Result<CellLockStatusResponse> {
    let mut rat_status = Vec::new();
    let mut any_locked = false;
    
    // 查询 NR 锁定状态
    let nr_cmd = format!("AT+SPFORCEFRQ={},3", FORCEFRQ_TYPE_NR);
    {
        let response = read_lock_command(conn, &nr_cmd, "+SPFORCEFRQ:").await?;
        let status = parse_spforcefrq_query_response(&response, FORCEFRQ_TYPE_NR);
        if status.enabled {
            any_locked = true;
        }
        rat_status.push(status);
    }
    
    // 查询 LTE 锁定状态
    let lte_cmd = format!("AT+SPFORCEFRQ={},3", FORCEFRQ_TYPE_LTE);
    {
        let response = read_lock_command(conn, &lte_cmd, "+SPFORCEFRQ:").await?;
        let status = parse_spforcefrq_query_response(&response, FORCEFRQ_TYPE_LTE);
        if status.enabled {
            any_locked = true;
        }
        rat_status.push(status);
    }
    
    let response = CellLockStatusResponse {
        rat_status,
        any_locked,
    };
    
    Ok(response)
}

pub async fn get_cell_lock_handler(State(conn): State<Arc<Connection>>) -> impl IntoResponse {
    let response = match CELL_LOCK_SNAPSHOT.get(&crate::dbus::snapshot_scope(&conn),
        std::time::Duration::from_secs(30), read_cell_lock(&conn)).await {
        Ok(status) => ApiResponse::success_with_message("Success", status.as_ref().clone()),
        Err(error) => ApiResponse::<CellLockStatusResponse>::error(error.to_string()),
    };
    (StatusCode::OK, Json(response))
}

/// POST /api/cell-lock - 设置小区锁定
/// 
/// 使用 AT+SPFORCEFRQ 指令锁定到指定小区
/// 
/// ## 锁定流程
/// 1. AT+SFUN=5 - 进入工程模式
/// 2. AT+SPFORCEFRQ=16,0 - 清空 NR 锁定
/// 3. AT+SPFORCEFRQ=12,0 - 清空 LTE 锁定
/// 4. AT+SPFORCEFRQ=<type>,2,<arfcn>,<pci> - 设置锁定
/// 5. AT+SFUN=4 - 恢复正常模式
/// 
/// ## 请求示例
/// ```json
/// {
///   "rat": 16,        // 16=NR, 12=LTE
///   "enable": true,
///   "pci": 599,
///   "arfcn": 633984
/// }
/// ```
pub async fn set_cell_lock_handler(
    State(conn): State<Arc<Connection>>,
    Json(payload): Json<CellLockRequest>,
) -> impl IntoResponse {
    // 确定网络类型
    let forcefrq_type = if payload.rat == FORCEFRQ_TYPE_LTE || payload.rat == FORCEFRQ_TYPE_NR {
        payload.rat
    } else {
        // 根据 rat 值推断类型
        match payload.rat {
            1 | 2 => FORCEFRQ_TYPE_LTE,  // LTE FDD/TDD
            5 | 6 | 7 => FORCEFRQ_TYPE_NR, // NR SA/NSA
            _ => FORCEFRQ_TYPE_NR,
        }
    };
    
    if payload.enable {
        // 锁定小区需要 ARFCN 和 PCI
        let (arfcn, pci) = match (payload.arfcn, payload.pci) {
            (Some(a), Some(p)) => (a, p),
            _ => {
                return (
                    StatusCode::OK,
                    Json(ApiResponse::<serde_json::Value>::error(
                        "锁定小区需要同时提供 arfcn 和 pci 参数"
                    )),
                );
            }
        };
        
        // 执行锁定流程
        let steps = vec![
            ("AT+SFUN=5", "进入工程模式"),
            ("AT+SPFORCEFRQ=16,0", "清空 NR 锁定"),
            ("AT+SPFORCEFRQ=12,0", "清空 LTE 锁定"),
        ];
        
        for (cmd, desc) in &steps {
            if let Err(e) = send_at_command(&conn, cmd).await {
                // 恢复正常模式
                let _ = send_at_command(&conn, "AT+SFUN=4").await;
                return (
                    StatusCode::OK,
                    Json(ApiResponse::<serde_json::Value>::error(format!(
                        "{}失败: {}",
                        desc, e
                    ))),
                );
            }
        }
        
        // 设置锁定
        let lock_cmd = format!("AT+SPFORCEFRQ={},2,{},{}", forcefrq_type, arfcn, pci);
        if let Err(e) = send_at_command(&conn, &lock_cmd).await {
            // 恢复正常模式
            let _ = send_at_command(&conn, "AT+SFUN=4").await;
            return (
                StatusCode::OK,
                Json(ApiResponse::<serde_json::Value>::error(format!(
                    "设置锁定失败: {}",
                    e
                ))),
            );
        }
        
        // 恢复正常模式
        if let Err(e) = send_at_command(&conn, "AT+SFUN=4").await {
            return (
                StatusCode::OK,
                Json(ApiResponse::<serde_json::Value>::error(format!(
                    "恢复正常模式失败: {}",
                    e
                ))),
            );
        }
        
        (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                format!("{} 小区锁定已设置 (ARFCN={}, PCI={})", get_rat_name(forcefrq_type), arfcn, pci),
                json!({
                    "locked": true,
                    "tech": get_rat_name(forcefrq_type),
                    "arfcn": arfcn,
                    "pci": pci
                }),
            )),
        )
    } else {
        // 解锁：清空指定类型的锁定
        // 1. 进入工程模式
        if let Err(e) = send_at_command(&conn, "AT+SFUN=5").await {
            return (
                StatusCode::OK,
                Json(ApiResponse::<serde_json::Value>::error(format!(
                    "进入工程模式失败: {}",
                    e
                ))),
            );
        }
        
        // 2. 清空锁定
        let clear_cmd = format!("AT+SPFORCEFRQ={},0", forcefrq_type);
        if let Err(e) = send_at_command(&conn, &clear_cmd).await {
            // 恢复正常模式
            let _ = send_at_command(&conn, "AT+SFUN=4").await;
            return (
                StatusCode::OK,
                Json(ApiResponse::<serde_json::Value>::error(format!(
                    "清空锁定失败: {}",
                    e
                ))),
            );
        }
        
        // 3. 恢复正常模式
        if let Err(e) = send_at_command(&conn, "AT+SFUN=4").await {
            return (
                StatusCode::OK,
                Json(ApiResponse::<serde_json::Value>::error(format!(
                    "恢复正常模式失败: {}",
                    e
                ))),
            );
        }
        
        (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                format!("{} 小区锁定已解除", get_rat_name(forcefrq_type)),
                json!({
                    "locked": false,
                    "tech": get_rat_name(forcefrq_type)
                }),
            )),
        )
    }
}

/// POST /api/cell-lock/unlock-all - 解除所有小区锁定
/// 
/// 清除 NR 和 LTE 的小区锁定
pub async fn unlock_all_cells_handler(
    State(conn): State<Arc<Connection>>,
    Json(_payload): Json<CellUnlockRequest>,
) -> impl IntoResponse {
    // 完整的解锁流程
    let steps = vec![
        ("AT+SFUN=5", "进入工程模式"),
        ("AT+SPFORCEFRQ=16,0", "清空 NR 锁定"),
        ("AT+SPFORCEFRQ=12,0", "清空 LTE 锁定"),
        ("AT+SFUN=4", "恢复正常模式"),
    ];
    
    let mut success_steps = Vec::new();
    let mut errors = Vec::new();
    
    for (cmd, desc) in &steps {
        match send_at_command(&conn, cmd).await {
            Ok(_) => success_steps.push(*desc),
            Err(e) => {
                errors.push(format!("{}: {}", desc, e));
                // 尝试恢复正常模式
                if !cmd.contains("SFUN=4") {
                    let _ = send_at_command(&conn, "AT+SFUN=4").await;
                }
                break;
            }
        }
    }
    
    if errors.is_empty() {
        (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "已解除所有小区锁定 (NR + LTE)",
                json!({
                    "success": true,
                    "steps": success_steps
                }),
            )),
        )
    } else {
        (
            StatusCode::OK,
            Json(ApiResponse::<serde_json::Value>::error(format!(
                "解锁失败: {}",
                errors.join("; ")
            ))),
        )
    }
}

// ============ 电话相关 API ============

use crate::db::Database;

/// GET /api/calls - 获取当前通话列表
pub async fn get_calls_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<CallListResponse>>) {
    // 获取 VoiceCallManager 接口下的所有通话
    match crate::dbus::get_active_calls(&conn).await {
        Ok(calls) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", CallListResponse { calls })),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get calls: {}", e))),
        ),
    }
}


/// POST /api/call/dial - 拨打电话
pub async fn dial_call_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<MakeCallRequest>,
) -> (StatusCode, Json<ApiResponse<CallInfo>>) {
    match crate::dbus::dial_call(&conn, &req.phone_number).await {
        Ok(call_info) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Call initiated", call_info)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to dial: {}", e))),
        ),
    }
}

/// POST /api/call/hangup - 挂断电话
pub async fn hangup_call_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<HangupCallRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::hangup_call(&conn, &req.path).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Call ended", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to hangup: {}", e))),
        ),
    }
}

/// POST /api/call/hangup-all - 挂断所有电话
pub async fn hangup_all_calls_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::hangup_all_calls(&conn).await {
        Ok(count) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                format!("Ended {} call(s)", count),
                json!({ "count": count }),
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to hangup all: {}", e))),
        ),
    }
}

/// POST /api/call/answer - 接听来电
pub async fn answer_call_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<HangupCallRequest>, // 复用结构，只需要 path
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::answer_call(&conn, &req.path).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Call answered", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to answer: {}", e))),
        ),
    }
}

// ============ 短信相关 API ============

/// POST /api/sms/send - 发送短信
pub async fn send_sms_handler(
    State((conn, db)): State<(Arc<Connection>, Arc<Database>)>,
    Json(req): Json<SendSmsRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    // 发送短信
    match crate::dbus::send_sms(&conn, &req.phone_number, &req.content).await {
        Ok(message_path) => {
            // 存储到数据库
            match db.insert_sms("outgoing", &req.phone_number, &req.content, "sent", None) {
                Ok(id) => (
                    StatusCode::OK,
                    Json(ApiResponse::success_with_message(
                        "SMS sent successfully",
                        json!({
                            "message_path": message_path,
                            "db_id": id,
                        }),
                    )),
                ),
                Err(_e) => {
                    (
                        StatusCode::OK,
                        Json(ApiResponse::success_with_message(
                            "SMS sent but failed to save to database",
                            json!({ "message_path": message_path }),
                        )),
                    )
                }
            }
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to send SMS: {}", e))),
        ),
    }
}

/// GET /api/sms/list - 获取短信列表
pub async fn get_sms_list_handler(
    State(db): State<Arc<Database>>,
    axum::extract::Query(req): axum::extract::Query<SmsListRequest>,
) -> (StatusCode, Json<ApiResponse<Vec<crate::db::SmsMessage>>>) {
    match db.get_sms_messages(req.limit, req.offset) {
        Ok(messages) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                format!("Retrieved {} messages", messages.len()),
                messages,
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get messages: {}", e))),
        ),
    }
}

/// GET /api/sms/conversation - 获取与特定号码的对话历史
pub async fn get_sms_conversation_handler(
    State(db): State<Arc<Database>>,
    axum::extract::Query(req): axum::extract::Query<SmsConversationRequest>,
) -> (StatusCode, Json<ApiResponse<Vec<crate::db::SmsMessage>>>) {
    match db.get_sms_conversation(&req.phone_number, req.limit) {
        Ok(messages) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                format!("Retrieved {} messages", messages.len()),
                messages,
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get conversation: {}", e))),
        ),
    }
}

/// GET /api/sms/stats - 获取短信统计
pub async fn get_sms_stats_handler(
    State(db): State<Arc<Database>>,
) -> (StatusCode, Json<ApiResponse<crate::db::SmsStats>>) {
    match db.get_sms_stats() {
        Ok(stats) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", stats)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get stats: {}", e))),
        ),
    }
}

impl Default for crate::db::SmsStats {
    fn default() -> Self {
        Self {
            total: 0,
            incoming: 0,
            outgoing: 0,
        }
    }
}

/// DELETE /api/sms/clear - 清空所有短信
pub async fn clear_sms_handler(
    State(db): State<Arc<Database>>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match db.clear_all_sms() {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("All messages cleared", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to clear messages: {}", e))),
        ),
    }
}

// ============ 新增功能 API ============

/// GET /api/device/imeisv - 获取 IMEISV（软件版本号）
pub async fn get_imeisv_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::ImeisvResponse>>) {
    match crate::dbus::get_imeisv(&conn).await {
        Ok(imeisv) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", imeisv)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get IMEISV: {}", e))),
        ),
    }
}

/// GET /api/network/signal-strength - 获取信号强度详细信息
pub async fn get_signal_strength_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::SignalStrengthResponse>>) {
    match crate::dbus::get_signal_strength(&conn).await {
        Ok(signal) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", signal)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get signal strength: {}", e))),
        ),
    }
}

/// GET /api/network/nitz - 获取 NITZ 网络时间
pub async fn get_nitz_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::NitzTimeResponse>>) {
    match crate::dbus::get_nitz_time(&conn).await {
        Ok(nitz) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", nitz)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get NITZ time: {}", e))),
        ),
    }
}

/// GET /api/ims/status - 获取 IMS 状态
pub async fn get_ims_status_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::ImsStatusResponse>>) {
    match crate::dbus::get_ims_status(&conn).await {
        Ok(ims) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", ims)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get IMS status: {}", e))),
        ),
    }
}

/// GET /api/call/volume - 获取通话音量
pub async fn get_call_volume_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::CallVolumeResponse>>) {
    match crate::dbus::get_call_volume(&conn).await {
        Ok(volume) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", volume)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get call volume: {}", e))),
        ),
    }
}

/// POST /api/call/volume - 设置通话音量
pub async fn set_call_volume_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<crate::models::SetCallVolumeRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::set_call_volume(&conn, req.speaker_volume, req.microphone_volume, req.muted).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Call volume updated", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to set call volume: {}", e))),
        ),
    }
}

/// GET /api/voicemail/status - 获取语音留言状态
pub async fn get_voicemail_status_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::VoicemailStatusResponse>>) {
    match crate::dbus::get_voicemail_status(&conn).await {
        Ok(voicemail) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", voicemail)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get voicemail status: {}", e))),
        ),
    }
}

/// GET /api/network/operators - 获取运营商列表（快速）
pub async fn get_operators_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::OperatorListResponse>>) {
    match crate::dbus::get_operators(&conn).await {
        Ok(operators) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", operators)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get operators: {}", e))),
        ),
    }
}

/// GET /api/network/operators/scan - 扫描所有运营商（慢，120秒）
pub async fn scan_operators_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::OperatorListResponse>>) {
    match crate::dbus::scan_operators(&conn).await {
        Ok(operators) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Scan completed", operators)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to scan operators: {}", e))),
        ),
    }
}

/// POST /api/network/register-manual - 手动注册运营商
pub async fn register_operator_manual_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<crate::models::ManualRegisterRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::register_operator_manual(&conn, &req.mccmnc).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                format!("Registered to operator {}", req.mccmnc),
                json!({}),
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to register operator: {}", e))),
        ),
    }
}

/// POST /api/network/register-auto - 自动注册运营商
pub async fn register_operator_auto_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::register_operator_auto(&conn).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Automatic operator selection confirmed; registration may still be in progress", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to register automatically: {}", e))),
        ),
    }
}

/// GET /api/call/forwarding - 获取呼叫转移设置
pub async fn get_call_forwarding_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::CallForwardingResponse>>) {
    match crate::dbus::get_call_forwarding(&conn).await {
        Ok(forwarding) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", forwarding)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get call forwarding: {}", e))),
        ),
    }
}

/// POST /api/call/forwarding - 设置呼叫转移
pub async fn set_call_forwarding_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<crate::models::SetCallForwardingRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::set_call_forwarding(&conn, &req.forward_type, &req.number, req.timeout).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Call forwarding updated", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to set call forwarding: {}", e))),
        ),
    }
}

/// GET /api/call/settings - 获取通话设置
pub async fn get_call_settings_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::CallSettingsResponse>>) {
    match crate::dbus::get_call_settings(&conn).await {
        Ok(settings) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", settings)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get call settings: {}", e))),
        ),
    }
}

/// POST /api/call/settings - 设置通话设置
pub async fn set_call_settings_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<crate::models::SetCallSettingRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::set_call_setting(&conn, &req.property, &req.value).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Call settings updated", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to set call settings: {}", e))),
        ),
    }
}

// ============ SIM 卡槽功能 ============

/// GET /api/sim/slot - 获取 SIM 卡槽信息
pub async fn get_sim_slot_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<crate::models::SimSlotResponse>>) {
    match crate::dbus::get_sim_slot(&conn).await {
        Ok(slot_info) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", slot_info)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get SIM slot info: {}", e))),
        ),
    }
}

/// POST /api/sim/slot/switch - 切换 SIM 卡槽
pub async fn switch_sim_slot_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<crate::models::SwitchSimSlotRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match crate::dbus::switch_sim_slot(&conn, req.slot).await {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                format!("Switched to SIM slot {}", req.slot),
                json!({"response": response}),
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to switch SIM slot: {}", e))),
        ),
    }
}

// ============ APN 管理功能 ============

/// GET /api/apn - 获取 APN 列表
///
/// 返回所有 internet 类型的 APN context 配置
pub async fn get_apn_list_handler(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<ApnListResponse>>) {
    match get_all_apn_contexts(&conn).await {
        Ok(contexts) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "Success",
                ApnListResponse { contexts },
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get APN list: {}", e))),
        ),
    }
}

/// POST /api/apn - 设置 APN 配置
///
/// # 请求体
/// ```json
/// {
///   "context_path": "/ril_0/context2",
///   "apn": "cbnet",
///   "protocol": "dual",
///   "username": "",
///   "password": "",
///   "auth_method": "chap"
/// }
/// ```
pub async fn set_apn_handler(
    State(conn): State<Arc<Connection>>,
    Json(req): Json<SetApnRequest>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    // 验证 context_path
    if req.context_path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::error("context_path is required")),
        );
    }
    
    // 调用 D-Bus 设置 APN 属性
    match set_apn_properties(
        &conn,
        &req.context_path,
        req.apn.as_deref(),
        req.protocol.as_deref(),
        req.username.as_deref(),
        req.password.as_deref(),
        req.auth_method.as_deref(),
    ).await {
        Ok(_) => {
            // 获取更新后的 APN 配置
            match get_all_apn_contexts(&conn).await {
                Ok(contexts) => {
                    // 找到刚刚修改的 context
                    let updated_context = contexts
                        .iter()
                        .find(|c| c.path == req.context_path)
                        .cloned();
                    
                    (
                        StatusCode::OK,
                        Json(ApiResponse::success_with_message(
                            "APN configuration updated successfully",
                            json!({
                                "updated_context": updated_context,
                            }),
                        )),
                    )
                }
                Err(_) => (
                    StatusCode::OK,
                    Json(ApiResponse::success_with_message(
                        "APN configuration updated successfully",
                        json!({}),
                    )),
                ),
            }
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to set APN: {}", e))),
        ),
    }
}

/// GET /api/connectivity - 联网检测
///
/// 通过 ping 检测 IPv4 和 IPv6 连通性
pub async fn get_connectivity_check(
    State(conn): State<Arc<Connection>>,
) -> (StatusCode, Json<ApiResponse<ConnectivityCheckResponse>>) {
    let result = crate::dbus::CONNECTIVITY_SNAPSHOT.get(&crate::dbus::snapshot_scope(&conn),
        std::time::Duration::from_secs(5), async {
            let (ipv4_target, ipv6_target) = connectivity_targets(&conn).await;
            let (ipv4, ipv6) = tokio::join!(ping_host(ipv4_target, false), ping_host(ipv6_target, true));
            Ok(ConnectivityCheckResponse { ipv4, ipv6 })
        }).await;
    let response = match result {
        Ok(data) => ApiResponse::success_with_message("Connectivity check completed", data.as_ref().clone()),
        Err(error) => ApiResponse::error(error.to_string()),
    };
    (StatusCode::OK, Json(response))
}

/// 中国大陆（MCC 460）用阿里 DNS，其他地区用 Cloudflare anycast；无法获取 MCC 时沿用原有目标
async fn connectivity_targets(conn: &Connection) -> (&'static str, &'static str) {
    const MAINLAND: (&str, &str) = ("223.5.5.5", "2400:3200::1");
    const GLOBAL: (&str, &str) = ("1.1.1.1", "2606:4700:4700::1111");

    let mcc = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::dbus::get_registered_mcc(conn),
    )
    .await
    .ok()
    .flatten();

    match mcc.as_deref() {
        Some("460") | None => MAINLAND,
        Some(_) => GLOBAL,
    }
}

/// 执行 ping 检测
async fn ping_host(target: &str, is_ipv6: bool) -> PingResult {
    let cmd = if is_ipv6 { "ping6" } else { "ping" };
    
    // 使用 -c 1 只发一个包，-W 2 设置超时 2 秒
    let output = TokioCommand::new(cmd)
        .args(["-c", "1", "-W", "2", target])
        .kill_on_drop(true)
        .output()
        .await;
    
    match output {
        Ok(result) => {
            if result.status.success() {
                // 解析延迟时间
                let stdout = String::from_utf8_lossy(&result.stdout);
                let latency = parse_ping_latency(&stdout);
                
                PingResult {
                    success: true,
                    latency_ms: latency,
                    target: target.to_string(),
                    error: None,
                }
            } else {
                let stderr = String::from_utf8_lossy(&result.stderr);
                PingResult {
                    success: false,
                    latency_ms: None,
                    target: target.to_string(),
                    error: Some(if stderr.is_empty() {
                        "Host unreachable".to_string()
                    } else {
                        stderr.trim().to_string()
                    }),
                }
            }
        }
        Err(e) => PingResult {
            success: false,
            latency_ms: None,
            target: target.to_string(),
            error: Some(format!("Failed to execute ping: {}", e)),
        },
    }
}

/// 从 ping 输出中解析延迟时间
fn parse_ping_latency(output: &str) -> Option<f64> {
    // 匹配 "time=XX.XX ms" 或 "time=XX ms"
    for line in output.lines() {
        if let Some(time_pos) = line.find("time=") {
            let after_time = &line[time_pos + 5..];
            // 找到数字部分
            let num_str: String = after_time
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(latency) = num_str.parse::<f64>() {
                return Some(latency);
            }
        }
    }
    None
}

// ============ 通话记录 API ============

use crate::sms_push::SmsPushSender;
use crate::webhook::WebhookSender;

/// GET /api/call/history - 获取通话记录
pub async fn get_call_history_handler(
    State(db): State<Arc<Database>>,
    Query(params): Query<crate::models::CallHistoryRequest>,
) -> (StatusCode, Json<ApiResponse<crate::models::CallHistoryResponse>>) {
    let limit = if params.limit > 0 { params.limit } else { 50 };
    let offset = if params.offset >= 0 { params.offset } else { 0 };
    
    match db.get_call_history(limit, offset) {
        Ok(records) => {
            let stats = db.get_call_stats().unwrap_or_default();
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message(
                    "Success",
                    crate::models::CallHistoryResponse { records, stats },
                )),
            )
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to get call history: {}", e))),
        ),
    }
}

/// DELETE /api/call/history/{id} - 删除单条通话记录
pub async fn delete_call_history_handler(
    State(db): State<Arc<Database>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match db.delete_call(id) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Call record deleted", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to delete call record: {}", e))),
        ),
    }
}

/// POST /api/call/history/clear - 清空所有通话记录
pub async fn clear_call_history_handler(
    State(db): State<Arc<Database>>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match db.clear_all_calls() {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("All call records cleared", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to clear call history: {}", e))),
        ),
    }
}

// ============ init.sh 管理 API ============

/// GET /api/init-script - Get init.sh content and loader hook status.
pub async fn get_init_script_handler(
) -> (StatusCode, Json<ApiResponse<crate::models::InitScriptResponse>>) {
    match crate::config::get_init_script() {
        Ok(script) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Success", script)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<crate::models::InitScriptResponse>::error(format!(
                "Failed to get init script: {}",
                e
            ))),
        ),
    }
}

/// POST /api/init-script - Save init.sh content and ensure loader.sh calls it.
pub async fn set_init_script_handler(
    Json(req): Json<crate::models::SetInitScriptRequest>,
) -> (StatusCode, Json<ApiResponse<crate::models::InitScriptResponse>>) {
    match crate::config::set_init_script(req.script) {
        Ok(script) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("init.sh updated", script)),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<crate::models::InitScriptResponse>::error(format!(
                "Failed to update init script: {}",
                e
            ))),
        ),
    }
}

// ============ Webhook 配置 API ============

/// GET /api/webhook/config - 获取 Webhook 配置
pub async fn get_webhook_config_handler(
    State(config_manager): State<Arc<ConfigManager>>,
) -> (StatusCode, Json<ApiResponse<crate::config::WebhookConfig>>) {
    let config = config_manager.get_webhook();
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_message("Success", config)),
    )
}

/// POST /api/webhook/config - 设置 Webhook 配置
pub async fn set_webhook_config_handler(
    State(config_manager): State<Arc<ConfigManager>>,
    Json(webhook_config): Json<crate::config::WebhookConfig>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match config_manager.set_webhook(webhook_config) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Webhook config updated", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to update webhook config: {}", e))),
        ),
    }
}

/// POST /api/webhook/test - 测试 Webhook 连接
pub async fn test_webhook_handler(
    State(webhook_sender): State<Arc<WebhookSender>>,
) -> (StatusCode, Json<ApiResponse<crate::models::WebhookTestResponse>>) {
    match webhook_sender.test_webhook().await {
        Ok(message) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "Webhook test successful",
                crate::models::WebhookTestResponse {
                    success: true,
                    message,
                },
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "Webhook test failed",
                crate::models::WebhookTestResponse {
                    success: false,
                    message: e,
                },
            )),
        ),
    }
}

/// GET /api/sms-push/config - 获取短信推送配置
pub async fn get_sms_push_config_handler(
    State(config_manager): State<Arc<ConfigManager>>,
) -> (StatusCode, Json<ApiResponse<crate::config::SmsPushConfig>>) {
    let config = config_manager.get_sms_push();
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_message("Success", config)),
    )
}

/// POST /api/sms-push/config - 设置短信推送配置
pub async fn set_sms_push_config_handler(
    State(config_manager): State<Arc<ConfigManager>>,
    Json(sms_push_config): Json<crate::config::SmsPushConfig>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    match config_manager.set_sms_push(sms_push_config) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("SMS push config updated", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!("Failed to update SMS push config: {}", e))),
        ),
    }
}

/// POST /api/sms-push/test - 测试短信推送连接
pub async fn test_sms_push_handler(
    State(sms_push_sender): State<Arc<SmsPushSender>>,
) -> (StatusCode, Json<ApiResponse<crate::models::WebhookTestResponse>>) {
    match sms_push_sender.test_sms_push().await {
        Ok(message) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "SMS push test successful",
                crate::models::WebhookTestResponse {
                    success: true,
                    message,
                },
            )),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(
                "SMS push test failed",
                crate::models::WebhookTestResponse {
                    success: false,
                    message: e,
                },
            )),
        ),
    }
}

// ============ OTA 更新功能 ============

/// GET /api/ota/status - 获取 OTA 更新状态
pub async fn get_refresh_config_handler(
    State(config_manager): State<Arc<ConfigManager>>,
    State(frontend_runtime): State<Arc<FrontendRuntime>>,
) -> (StatusCode, Json<ApiResponse<crate::models::RefreshConfigResponse>>) {
    let refresh = config_manager.get_refresh();
    let response = crate::models::RefreshConfigResponse {
        interval_ms: refresh.interval_ms,
        watchdog_active_interval_ms: refresh.active_watchdog_interval_ms(),
        watchdog_idle_interval_ms: refresh.idle_watchdog_interval_ms(),
        heartbeat_timeout_ms: refresh.heartbeat_timeout_ms(),
        frontend_connected: frontend_runtime
            .is_recent(std::time::Duration::from_millis(refresh.heartbeat_timeout_ms())),
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_message("Success", response)),
    )
}

pub async fn set_refresh_config_handler(
    State(config_manager): State<Arc<ConfigManager>>,
    State(frontend_runtime): State<Arc<FrontendRuntime>>,
    Json(refresh): Json<RefreshConfig>,
) -> (StatusCode, Json<ApiResponse<crate::models::RefreshConfigResponse>>) {
    let refresh = refresh.sanitize();

    match config_manager.set_refresh(refresh.clone()) {
        Ok(_) => {
            let response = crate::models::RefreshConfigResponse {
                interval_ms: refresh.interval_ms,
                watchdog_active_interval_ms: refresh.active_watchdog_interval_ms(),
                watchdog_idle_interval_ms: refresh.idle_watchdog_interval_ms(),
                heartbeat_timeout_ms: refresh.heartbeat_timeout_ms(),
                frontend_connected: frontend_runtime
                    .is_recent(std::time::Duration::from_millis(refresh.heartbeat_timeout_ms())),
            };

            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message("Refresh config updated", response)),
            )
        }
        Err(error) => (
            StatusCode::OK,
            Json(ApiResponse::error(format!(
                "Failed to update refresh config: {}",
                error
            ))),
        ),
    }
}

pub async fn frontend_refresh_heartbeat_handler(
    State(config_manager): State<Arc<ConfigManager>>,
    State(frontend_runtime): State<Arc<FrontendRuntime>>,
) -> (StatusCode, Json<ApiResponse<crate::models::RefreshConfigResponse>>) {
    frontend_runtime.mark_seen();
    let refresh = config_manager.get_refresh();
    let response = crate::models::RefreshConfigResponse {
        interval_ms: refresh.interval_ms,
        watchdog_active_interval_ms: refresh.active_watchdog_interval_ms(),
        watchdog_idle_interval_ms: refresh.idle_watchdog_interval_ms(),
        heartbeat_timeout_ms: refresh.heartbeat_timeout_ms(),
        frontend_connected: true,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_message("Refresh heartbeat accepted", response)),
    )
}

pub async fn get_ota_status_handler() -> impl IntoResponse {
    let status = crate::ota::get_ota_status();
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_message("Success", status)),
    )
}

/// POST /api/ota/upload - 上传 OTA 更新包
pub async fn upload_ota_handler(
    body: axum::body::Bytes,
) -> impl IntoResponse {
    match crate::ota::handle_ota_upload(&body) {
        Ok(response) => {
            let message = if response.validation.valid {
                "OTA package uploaded and validated"
            } else {
                "OTA package uploaded but validation failed"
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_message(message, response)),
            )
        }
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<crate::models::OtaUploadResponse>::error(format!(
                "Failed to process OTA package: {}",
                e
            ))),
        ),
    }
}

/// POST /api/ota/apply - 应用 OTA 更新
pub async fn apply_ota_handler(
    Json(req): Json<crate::models::OtaApplyRequest>,
) -> impl IntoResponse {
    match crate::ota::apply_ota_update(req.restart_now) {
        Ok(message) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message(&message, json!({ "applied": true }))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<serde_json::Value>::error(format!(
                "Failed to apply OTA update: {}",
                e
            ))),
        ),
    }
}

/// POST /api/ota/cancel - 取消待安装的更新
pub async fn cancel_ota_handler() -> impl IntoResponse {
    match crate::ota::cancel_pending_update() {
        Ok(()) => (
            StatusCode::OK,
            Json(ApiResponse::success_with_message("Pending update cancelled", json!({}))),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(ApiResponse::<serde_json::Value>::error(format!(
                "Failed to cancel update: {}",
                e
            ))),
        ),
    }
}

