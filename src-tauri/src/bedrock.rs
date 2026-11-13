// bedrock.rs — 基岩版（MineBBS 清单 + 123 云盘分享解析下载 + 本地启动）
// 清单：MineBBS 平台 platform=3 / environment=1（Windows 基岩版）
// 下载：先带 Referer 解析 MineBBS 跳转拿到 123 分享页，再按 123 网页协议本地解析。
// 登录态：用户在客户端内登录一次 123 云盘，authorization 只存在本地，不经过任何服务器。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::api::ApiResult;
use crate::storage;
use crate::utils;

const MINEBBS_API_BASE: &str =
    "https://api.mc.minebbs.com/api/v1/versions/?pageSize=10&platform=3&environment=1";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const CHAR_MAP: [char; 26] = [
    'a', 'd', 'e', 'f', 'g', 'h', 'l', 'm', 'y', 'i', 'j', 'n', 'o', 'p', 'k', 'q', 'r', 's',
    't', 'u', 'b', 'c', 'v', 'w', 's', 'z',
];

// ==================== 小工具 ====================

fn data_dir() -> PathBuf {
    storage::resolve_data_dir()
}

fn token_path() -> PathBuf {
    data_dir().join("123pan-token.txt")
}

fn read_token() -> String {
    std::fs::read_to_string(token_path())
        .map(|s| s.trim().trim_start_matches("Bearer ").to_string())
        .unwrap_or_default()
}

fn save_token(token: &str) -> bool {
    let _ = std::fs::create_dir_all(data_dir());
    match std::fs::write(token_path(), token.trim()) {
        Ok(_) => {
            login_log(&format!("token saved len={}", token.len()));
            true
        }
        Err(e) => {
            login_log(&format!("token save failed: {e}"));
            false
        }
    }
}

/// 登录流程诊断日志：写入 data/logs/bedrock-login.log
/// 用于排查「登录窗口一闪就关」「token 没落盘」这类只在用户机器上复现的问题
fn login_log(msg: &str) {
    let dir = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    use std::io::Write;
    let ts = chrono::Local::now().format("%H:%M:%S%.3f").to_string();
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("bedrock-login.log"))
        .and_then(|mut f| writeln!(f, "[{}] {}", ts, msg));
}

fn versions_dir() -> PathBuf {
    data_dir().join("versions")
}

fn marker_path(version_id: &str) -> PathBuf {
    versions_dir()
        .join(storage::sanitize_version_id(version_id))
        .join("bedrock.json")
}

pub fn is_bedrock(version_id: &str) -> bool {
    marker_path(version_id).exists()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn http_client(timeout_secs: u64, redirect: bool) -> reqwest::blocking::Client {
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(UA);
    if !redirect {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }
    builder.build().unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// CRC32（与 123 网页 JS 的 crc32 一致，返回十进制）
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// 123 接口签名：?{authK}={authV}
fn pan_sign(path: &str) -> String {
    use chrono::{FixedOffset, Utc};
    let offset = FixedOffset::east_opt(8 * 3600).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    let now = Utc::now().with_timezone(&offset);
    let ts = now.timestamp();
    let time_str = now.format("%Y%m%d%H%M").to_string();
    let mapped: String = time_str
        .chars()
        .map(|c| {
            let digit = c.to_digit(10).unwrap_or(0) as usize;
            CHAR_MAP[digit]
        })
        .collect();
    let auth_k = crc32(mapped.as_bytes());
    let random_part = (rand::random::<f64>() * 10_000_000.0).round() as u64;
    let raw = format!("{}|{}|{}|web|3|{}", ts, random_part, path, auth_k);
    let hash = crc32(raw.as_bytes());
    format!("?{}={}-{}-{}", auth_k, ts, random_part, hash)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(char::from_u32((h * 16 + l) as u32).unwrap_or('%'));
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(' ');
        } else {
            out.push(bytes[i] as char);
        }
        i += 1;
    }
    out
}

fn str_field(v: &Value, names: &[&str]) -> String {
    for name in names {
        if let Some(s) = v.get(*name).and_then(|x| x.as_str())
            && !s.is_empty()
        {
            return s.to_string();
        }
        if let Some(n) = v.get(*name).and_then(|x| x.as_i64()) {
            return n.to_string();
        }
    }
    String::new()
}

/// 清洗 Windows 非法文件名字符
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        "bedrock".to_string()
    } else {
        trimmed.to_string()
    }
}

// ==================== 123 登录态（本地 WebView 登录） ====================

#[derive(Clone, Default)]
struct LoginState {
    status: String, // idle / pending / success / failed
    message: String,
}

fn login_state() -> &'static Mutex<LoginState> {
    static STATE: OnceLock<Mutex<LoginState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(LoginState::default()))
}

fn set_login(status: &str, message: &str) {
    if let Ok(mut s) = login_state().lock() {
        s.status = status.to_string();
        s.message = message.to_string();
    }
}

fn close_login_window(app: &AppHandle) {
    // 登录窗口由 tauri.conf.json 预创建，这里只隐藏不销毁，下次复用同一个 WebView
    // （运行时用 builder 创建窗口在这个构建里拿不到 HWND，只能走配置声明）
    match app.get_webview_window("bedrock-123-login") {
        Some(win) => {
            login_log("hide login window");
            let _ = win.hide();
        }
        None => login_log("hide login window: not found"),
    }
}

/// 枚举本进程所有顶层窗口（标题/可见性/矩形）写进日志，
/// 并强制把登录窗口摆到屏幕中央、真正显示出来。
/// 不依赖 WebviewWindow::hwnd()（该接口在此构建里恒返回不可用）。
/// 只能在**非主线程**调用。
#[cfg(target_os = "windows")]
fn dump_and_force_login_window(width: i32, height: i32) -> String {
    use std::ffi::c_void;
    use std::sync::{Mutex, OnceLock};

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(
            cb: unsafe extern "system" fn(*mut c_void, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, pid_out: *mut u32) -> u32;
        fn GetWindowTextW(hwnd: *mut c_void, buf: *mut u16, max: i32) -> i32;
        fn IsWindowVisible(hwnd: *mut c_void) -> i32;
        fn GetWindowRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
        fn SetWindowPos(
            hwnd: *mut c_void,
            insert_after: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
        fn ShowWindow(hwnd: *mut c_void, cmd: i32) -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
    }

    static FOUND: OnceLock<Mutex<Vec<(isize, String, i32)>>> = OnceLock::new();

    unsafe extern "system" fn collect_cb(hwnd: *mut c_void, lparam: isize) -> i32 {
        let pid = lparam as u32;
        let mut wpid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut wpid);
            if wpid == pid {
                let mut buf = [0u16; 260];
                let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), 260);
                let title = if n > 0 {
                    String::from_utf16_lossy(&buf[..n as usize])
                } else {
                    String::new()
                };
                let visible = IsWindowVisible(hwnd);
                if let Some(list) = FOUND.get() {
                    list.lock().unwrap().push((hwnd as isize, title, visible));
                }
            }
        }
        1
    }

    let list = FOUND.get_or_init(|| Mutex::new(Vec::new()));
    list.lock().unwrap().clear();
    unsafe {
        EnumWindows(collect_cb, std::process::id() as isize);
    }
    let items: Vec<(isize, String, i32)> = list.lock().unwrap().clone();

    let sw = unsafe { GetSystemMetrics(0) };
    let sh = unsafe { GetSystemMetrics(1) };
    let x = ((sw - width) / 2).max(0);
    let y = ((sh - height) / 2).max(0);
    let mut out = String::new();
    for (h, title, visible) in items {
        let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
        unsafe { GetWindowRect(h as *mut c_void, &mut r) };
        out.push_str(&format!(
            "[{title:?} vis={visible} {}x{}@({},{})] ",
            r.right - r.left,
            r.bottom - r.top,
            r.left,
            r.top
        ));
        // 登录窗口判定：标题含 123；或标题为空但可见（主窗口标题固定、输入法窗口不可见）
        let is_login = title.contains("123") || (title.is_empty() && visible == 1);
        if is_login {
            unsafe {
                // HWND_TOPMOST(-1)：不置顶的话，尺寸更大的主窗口会把登录窗口整个盖住
                SetWindowPos(
                    h as *mut c_void,
                    (-1isize) as *mut c_void,
                    x,
                    y,
                    width,
                    height,
                    0x0040, // SWP_SHOWWINDOW
                );
                ShowWindow(h as *mut c_void, 5);
                let mut after = Rect { left: 0, top: 0, right: 0, bottom: 0 };
                GetWindowRect(h as *mut c_void, &mut after);
                out.push_str(&format!(
                    "<forced {h} after={}x{}@({},{})>",
                    after.right - after.left,
                    after.bottom - after.top,
                    after.left,
                    after.top
                ));
            }
        }
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn dump_and_force_login_window(_width: i32, _height: i32) -> String {
    String::from("unsupported")
}

fn handle_login_request(app: AppHandle, request_line: &str) {
    // 形如：GET /collect?t=xxx HTTP/1.1
    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    login_log(&format!("collect hit, path_len={}", path.len()));
    if let Some(pos) = path.find("t=") {
        let raw = &path[pos + 2..];
        let token = percent_decode(raw.split('&').next().unwrap_or(raw))
            .trim()
            .trim_start_matches("Bearer ")
            .to_string();
        login_log(&format!("parsed token len={}", token.len()));
        if token.len() > 8 {
            save_token(&token);
            set_login("success", "登录成功");
            login_log("closing login window after success");
            close_login_window(&app);
            return;
        }
    }
    login_log("no usable token in request");
    set_login("failed", "没有读到登录态，请重新登录");
}

fn start_login_browser(app: &AppHandle) -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("无法创建本地接收端口：{}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("无法获取本地端口：{}", e))?
        .port();
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("本地端口初始化失败：{}", e))?;

    set_login("pending", "请在弹出的窗口里登录 123 云盘");

    let poll_js = format!(
        r#"(function(){{try{{if(window.__verse123)return;window.__verse123=setInterval(function(){{try{{var t=localStorage.getItem('authorization')||localStorage.getItem('token')||sessionStorage.getItem('authorization');if(!t){{var m=document.cookie.match(/authorization=([^;]+)/);if(m)t=decodeURIComponent(m[1]);}}if(t&&t.length>10){{fetch('http://127.0.0.1:{}/collect?t='+encodeURIComponent(t),{{mode:'no-cors'}});}}}}catch(e){{}}}},1500);}}catch(e){{}}}})()"#,
        port
    );

    let app_accept = app.clone();
    let js_for_thread = poll_js.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut last_eval = Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(300) {
                set_login("failed", "登录超时，请重试");
                close_login_window(&app_accept);
                break;
            }
            // 登录页会跳转，跳转后 JS 定时器会丢，所以定期重新注入
            if last_eval.elapsed() >= Duration::from_millis(1500) {
                last_eval = Instant::now();
                if let Some(win) = app_accept.get_webview_window("bedrock-123-login") {
                    let _ = win.eval(&js_for_thread);
                }
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    use std::io::{Read, Write};
                    let mut buf = [0u8; 2048];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
                    handle_login_request(app_accept.clone(), &line);
                    break;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(120));
                }
                Err(_) => break,
            }
        }
    });

    let app_show = app.clone();
    let js_for_window = poll_js.clone();
    login_log(&format!("login port={port}, showing window"));

    // 登录窗口已在 tauri.conf.json 里预创建（和主窗口走同一条创建路径，HWND 正常）。
    // 运行时只 show + 注入脚本；这些调用必须放在非主线程，主线程里调会等待自己而死锁。
    std::thread::spawn(move || match app_show.get_webview_window("bedrock-123-login") {
        Some(win) => {
            login_log("worker: found pre-created login window");
            match win.set_size(tauri::LogicalSize::new(1080.0, 760.0)) {
                Ok(_) => login_log("worker: set_size ok"),
                Err(e) => login_log(&format!("worker: set_size err {e}")),
            }
            match win.center() {
                Ok(_) => login_log("worker: center ok"),
                Err(e) => login_log(&format!("worker: center err {e}")),
            }
            match win.show() {
                Ok(_) => login_log("worker: show ok"),
                Err(e) => login_log(&format!("worker: show err {e}")),
            }
            match win.set_focus() {
                Ok(_) => login_log("worker: focus ok"),
                Err(e) => login_log(&format!("worker: focus err {e}")),
            }
            match win.eval(&js_for_window) {
                Ok(_) => login_log("worker: eval ok"),
                Err(e) => login_log(&format!("worker: eval err {e}")),
            }
            // 不依赖 hwnd()，直接枚举本进程窗口并强制显示登录窗口
            let info = dump_and_force_login_window(1080, 760);
            login_log(&format!("worker: windows {info}"));
        }
        None => {
            login_log("worker: login window missing");
            set_login("failed", "登录窗口未初始化，请重启程序");
        }
    });

    Ok(())
}

// ==================== 基岩版清单（mcappx JSON，回退 MineBBS） ====================

/// mcappx 官方维护的版本数据源：一次请求拿全量，比 MineBBS 翻页快得多
const MCAPPX_JSON: &str = "https://data.mcappx.com/v2/bedrock.json";

fn cache_path() -> PathBuf {
    data_dir().join("cache").join("bedrock-versions-mcappx.json")
}

fn load_cache() -> Option<Vec<Value>> {
    let meta = std::fs::metadata(cache_path()).ok()?;
    let modified = meta.modified().ok()?;
    // 缓存有效期 24 小时：接口每页有 250KB、要翻 6 页，重复拉取代价太高
    if modified.elapsed().ok()?.as_secs() > 24 * 3600 {
        return None;
    }
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()?
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
}

/// 拉取一页清单，返回 (该页数据, 总页数)
fn fetch_page(page: u32) -> Result<(Vec<Value>, u32), String> {
    let client = http_client(15, false);
    let url = format!("{}&page={}", MINEBBS_API_BASE, page);
    let resp = client
        .get(&url)
        .header("Referer", "https://mc.minebbs.com/")
        .send()
        .map_err(|e| format!("无法连接 MineBBS：{}", e))?;
    if !resp.status().is_success() {
        return Err(format!("MineBBS 返回 HTTP {}", resp.status()));
    }
    let text = resp.text().map_err(|e| format!("读取清单失败：{}", e))?;
    let root: Value = serde_json::from_str(&text).map_err(|e| format!("清单格式异常：{}", e))?;
    let list = root
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let total = root
        .get("totalPages")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as u32;
    Ok((list, total))
}

fn fetch_manifest(force: bool) -> Result<Vec<Value>, String> {
    if !force && let Some(list) = load_cache() {
        return Ok(list);
    }

    // 优先 mcappx：单次请求返回全部版本；不可用时回退到 MineBBS 分页
    let all = match fetch_mcappx_manifest() {
        Ok(list) => list,
        Err(e) => {
            login_log(&format!("mcappx 清单不可用，回退 MineBBS：{e}"));
            fetch_minebbs_manifest()?
        }
    };

    let _ = std::fs::create_dir_all(cache_path().parent().unwrap_or(Path::new(".")));
    let _ = std::fs::write(cache_path(), json!({ "data": all.clone() }).to_string());
    Ok(all)
}

/// 解析 mcappx 的 JSON：键是版本号，值里有类型/日期/架构变体
fn fetch_mcappx_manifest() -> Result<Vec<Value>, String> {
    let client = http_client(20, false);
    let resp = client
        .get(MCAPPX_JSON)
        .send()
        .map_err(|e| format!("无法连接 mcappx 数据源：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("mcappx 数据源返回 HTTP {}", resp.status()));
    }
    let root: Value = resp.json().map_err(|e| format!("mcappx 数据格式异常：{e}"))?;
    let map = root
        .get("From_mcappx.com")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "mcappx 数据里没有版本表".to_string())?;

    let mut out = Vec::new();
    for (version, info) in map {
        let release_type = utils::get_str(info, "Type");
        let build_type = utils::get_str(info, "BuildType");
        let id = utils::get_str(info, "ID");
        let date = utils::get_str(info, "Date");
        let variations = info
            .get("Variations")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        // 只取 x64，且 ArchivalStatus >= 2（1 = 未发布、0 = 已丢失）
        let x64 = variations.iter().find(|v| {
            utils::get_str(v, "Arch").eq_ignore_ascii_case("x64")
                && v.get("ArchivalStatus").and_then(|s| s.as_u64()).unwrap_or(0) >= 2
        });
        let Some(var) = x64 else { continue };
        let os_build = utils::get_str(var, "OSbuild");
        let md5 = utils::get_str(var, "MD5");
        let mut abstract_text = format!("{release_type} · {build_type} · x64");
        if !os_build.is_empty() {
            abstract_text.push_str(&format!(" · Win{os_build}+"));
        }
        out.push(json!({
            "version": version,
            "id": id,
            "time": date,
            "size": 0,
            "downloadlinks": [],
            "abstract": abstract_text,
            "buildType": build_type,
            "releaseType": release_type,
            "md5": md5,
        }));
    }
    if out.is_empty() {
        return Err("mcappx 没有返回可用版本".to_string());
    }
    // Date 是 ISO 日期字符串，直接按字符串倒序就是最新在前
    out.sort_by(|a, b| utils::get_str(b, "time").cmp(&utils::get_str(a, "time")));
    Ok(out)
}

fn fetch_minebbs_manifest() -> Result<Vec<Value>, String> {
    // 先拉第一页，顺便确定总页数
    let (mut all, total_pages) = fetch_page(1)?;
    let last = total_pages.min(12);
    if last > 1 {
        // 其余页并发拉取：接口 pageSize 最大只能 10，6 页串行在网络差时要几十秒，
        // 并发后总耗时约等于最慢的一页
        let pages: Vec<u32> = (2..=last).collect();
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = pages
                .into_iter()
                .map(|p| scope.spawn(move || fetch_page(p)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err("清单线程异常".to_string())))
                .collect::<Vec<_>>()
        });
        for r in results {
            let (list, _) = r?;
            all.extend(list);
        }
    }

    if all.is_empty() {
        return Err("MineBBS 没有返回版本数据".to_string());
    }
    Ok(all)
}

fn installed_versions() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(versions_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("bedrock.json").exists()
                && let Some(name) = entry.file_name().to_str()
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// 清单版本与已装目录名的对应关系是松散的：
/// 清单里可能叫 1.21.111，而安装目录是 1.21.11101.0，所以按数字串前缀比较。
/// 返回实际已装目录名，这样前端点启动时能找到对应的 bedrock.json
fn match_installed(version: &str, id: &str, installed: &[String]) -> Option<String> {
    if let Some(hit) = installed.iter().find(|i| i.as_str() == version) {
        return Some(hit.clone());
    }
    let digits = |s: &str| -> String { s.chars().filter(|c| c.is_ascii_digit()).collect() };
    let want = {
        let d = digits(id);
        if d.is_empty() { digits(version) } else { d }
    };
    if want.len() < 6 {
        return None;
    }
    installed
        .iter()
        .find(|i| {
            let d = digits(i);
            d.len() >= 6 && (d.starts_with(&want) || want.starts_with(&d))
        })
        .cloned()
}

// ==================== 123 分享解析 ====================

/// 按 mcappx 的站内路由拼版本详情页：
///   1.21.111   → /bedrock/1.21/1.21.111/
///   26.70.24   → /bedrock/26.70/26.70.24/
///   26.62      → /bedrock/26.60/26.62/
/// 26.x 起站方按十位建目录（26.62 归到 26.60），1.x 时代是逐个 minor 建目录，不能取整。
fn mcappx_version_url(version: &str) -> String {
    let mut parts = version.split('.');
    let major = parts.next().unwrap_or("");
    let minor = parts.next().unwrap_or("");
    let dir = match minor.parse::<u32>() {
        Ok(n) if major != "1" && n >= 10 => format!("{}.{}", major, (n / 10) * 10),
        _ => format!("{}.{}", major, minor),
    };
    format!("https://www.mcappx.com/bedrock/{dir}/{version}/")
}

fn resolve_share_url(link: &str) -> Result<String, String> {
    let url = if link.starts_with("//") { format!("https:{}", link) } else { link.to_string() };
    let client = http_client(20, false);
    let resp = client
        .get(&url)
        .header("Referer", "https://mc.minebbs.com/")
        .send()
        .map_err(|e| format!("请求 MineBBS 下载跳转失败：{}", e))?;
    if resp.status().is_redirection() {
        if let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok()) {
            return Ok(loc.to_string());
        }
    }
    // 有些版本会直接给 123 地址
    let text = resp.text().unwrap_or_default();
    if text.contains("123pan.com") || text.contains("123912.com") {
        return Ok(text.trim().to_string());
    }
    Err("MineBBS 没有跳转到 123 云盘（分享链接可能已失效）".to_string())
}

fn parse_share(url: &str) -> Result<(String, String, String, String), String> {
    // 返回 (origin, shareKey, pwd, sharePageUrl)
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("分享地址异常：{}", e))?;
    let origin = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or("www.123pan.com"));
    let path = parsed.path().trim_end_matches('/');
    let key = path
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(".html")
        .to_string();
    if key.is_empty() {
        return Err("无法从链接里识别分享编号".to_string());
    }
    let pwd = parsed
        .query_pairs()
        .find(|(k, _)| k.as_ref() == "pwd" || k.as_ref() == "password")
        .map(|(_, v)| v.to_string())
        .unwrap_or_default();
    Ok((origin, key, pwd, url.to_string()))
}

struct ShareItem {
    file_id: Value,
    name: String,
    size: u64,
    s3_key_flag: String,
    etag: String,
    is_folder: bool,
}

fn list_share_files(origin: &str, key: &str, pwd: &str, referer: &str, token: &str) -> Result<Vec<ShareItem>, String> {
    let api = format!(
        "{}/a/api/share/get?limit=100&next=1&orderBy=file_name&orderDirection=asc&shareKey={}&SharePwd={}&ParentFileId=0&Page=1&event=homeListFile&operateType=1",
        origin, urlencoding::encode(key), urlencoding::encode(pwd)
    );
    let client = http_client(30, true);
    let mut req = client
        .get(&api)
        .header("Platform", "web")
        .header("App-Version", "3")
        .header("Referer", referer)
        .header("Origin", origin);
    if !token.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", token));
    }
    let resp = req.send().map_err(|e| format!("读取分享文件列表失败：{}", e))?;
    let text = resp.text().map_err(|e| format!("读取分享文件列表失败：{}", e))?;
    let root: Value = serde_json::from_str(&text).map_err(|e| format!("分享返回格式异常：{}", e))?;
    if api_code(&root) != 0 {
        let msg = root.get("message").and_then(|v| v.as_str()).unwrap_or("分享解析失败");
        return Err(msg.to_string());
    }
    let data = root.get("data").cloned().unwrap_or(Value::Null);
    let arr = data
        .get("Info")
        .or_else(|| data.get("info"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut items = Vec::new();
    for item in arr {
        let file_id = item
            .get("FileId")
            .or_else(|| item.get("fileId"))
            .cloned()
            .unwrap_or(Value::Null);
        let file_type = item
            .get("Type")
            .or_else(|| item.get("type"))
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
            })
            .unwrap_or(0);
        if file_id.is_null() {
            continue;
        }
        items.push(ShareItem {
            file_id,
            name: str_field(&item, &["FileName", "fileName", "name"]),
            size: item.get("Size").or_else(|| item.get("size")).and_then(|v| v.as_u64()).unwrap_or(0),
            s3_key_flag: str_field(&item, &["S3KeyFlag", "s3KeyFlag"]),
            etag: str_field(&item, &["Etag", "etag"]),
            is_folder: file_type == 1,
        });
    }
    if items.is_empty() {
        return Err("分享里没有找到可下载的文件".to_string());
    }
    Ok(items)
}

fn decode_download_url(raw: &str) -> Result<String, String> {
    use base64::Engine;
    let parsed = reqwest::Url::parse(raw).map_err(|e| format!("下载地址异常：{}", e))?;
    let params = match parsed.query_pairs().find(|(k, _)| k.as_ref() == "params") {
        Some((_, v)) => v.to_string(),
        None => return Ok(raw.to_string()),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(params.as_bytes())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(params.as_bytes()))
        .map_err(|e| format!("下载参数解码失败：{}", e))?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        for key in ["url", "downloadUrl", "redirect_url"] {
            if let Some(url) = v.get(key).and_then(|u| u.as_str()) {
                return Ok(url.to_string());
            }
        }
        if let Some(obj) = v.as_object() {
            for value in obj.values() {
                if let Some(url) = value.as_str()
                    && url.starts_with("http")
                {
                    return Ok(url.to_string());
                }
            }
        }
    }
    if text.starts_with("http") {
        return Ok(text);
    }
    Err("下载参数里没有找到地址".to_string())
}

fn fetch_final_url(app: &AppHandle, session_id: &str, url: &str, referer: &str) -> Result<String, String> {
    let client = http_client(60, true);
    let resp = client
        .get(url)
        .header("Referer", referer)
        .header("User-Agent", UA)
        .send()
        .map_err(|e| format!("获取下载地址失败：{}", e))?;
    if !resp.status().is_success() {
        return Err(format!("获取下载地址失败，HTTP {}", resp.status()));
    }
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let text = resp.text().map_err(|e| format!("读取下载信息失败：{}", e))?;
    if !content_type.contains("json") {
        return Ok(final_url);
    }
    let root: Value = serde_json::from_str(&text).map_err(|_| "123 返回的下载信息不是 JSON（可能需要登录或付费）".to_string())?;
    if api_code(&root) != 0 {
        let msg = root
            .get("message")
            .or_else(|| root.get("msg"))
            .and_then(|v| v.as_str())
            .unwrap_or("下载被拒绝");
        update_session(app, session_id, |s| {
            s.message = format!("123 云盘：{}", msg);
        });
        return Err(msg.to_string());
    }
    root.get("data")
        .and_then(|d| d.get("redirect_url").or_else(|| d.get("redirectUrl")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "下载信息里没有 redirect_url（可能需要登录或付费）".to_string())
}

fn request_download_url(
    origin: &str, key: &str, item: &ShareItem, referer: &str, token: &str,
) -> Result<String, String> {
    let client = http_client(30, true);
    let (api, body) = if item.is_folder {
        (
            format!("{}/b/api/file/batch_download_share_info{}", origin, pan_sign("/b/api/file/batch_download_share_info")),
            json!({ "ShareKey": key, "fileIdList": [{ "fileId": item.file_id }] }),
        )
    } else {
        (
            format!("{}/a/api/share/download/info{}", origin, pan_sign("/a/api/share/download/info")),
            json!({
                "ShareKey": key,
                "FileID": item.file_id,
                "S3keyFlag": item.s3_key_flag,
                "Size": item.size,
                "Etag": item.etag,
            }),
        )
    };
    let mut req = client
        .post(&api)
        .header("Platform", "web")
        .header("App-Version", "3")
        .header("Referer", referer)
        .header("Origin", origin)
        .json(&body);
    if !token.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", token));
    }
    let resp = req.send().map_err(|e| format!("请求下载地址失败：{}", e))?;
    let text = resp.text().map_err(|e| format!("读取下载地址失败：{}", e))?;
    let root: Value = serde_json::from_str(&text).map_err(|e| format!("下载接口返回格式异常：{}", e))?;
    if api_code(&root) != 0 {
        let msg = root
            .get("message")
            .or_else(|| root.get("msg"))
            .and_then(|v| v.as_str())
            .unwrap_or("123 云盘拒绝了下载请求");
        return Err(msg.to_string());
    }
    let data = root.get("data").cloned().unwrap_or(Value::Null);
    let raw = data
        .get("DownloadUrl")
        .or_else(|| data.get("DownloadURL"))
        .or_else(|| data.get("downloadUrl"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "下载接口没有返回地址".to_string())?;
    decode_download_url(raw)
}

// ==================== 安装会话 ====================

#[derive(Default)]
struct BedrockSession {
    status: String,
    progress: u32,
    message: String,
    downloaded: u64,
    total: u64,
    speed: u64,
    version: String,
    error: String,
    cancel: Option<Arc<AtomicBool>>,
}

static SESSIONS: OnceLock<Mutex<HashMap<String, BedrockSession>>> = OnceLock::new();

fn sessions() -> std::sync::MutexGuard<'static, HashMap<String, BedrockSession>> {
    SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
}

fn update_session<F: FnOnce(&mut BedrockSession)>(_app: &AppHandle, id: &str, f: F) {
    let mut map = sessions();
    if let Some(s) = map.get_mut(id) {
        f(s);
    }
}

/// 不依赖 AppHandle 的会话状态更新（本地导入任务用）
fn set_session(id: &str, status: &str, progress: u32, message: &str) {
    let mut map = sessions();
    if let Some(s) = map.get_mut(id) {
        s.status = status.into();
        s.progress = progress;
        s.message = message.into();
    }
}

fn set_session_error(id: &str, message: &str) {
    let mut map = sessions();
    if let Some(s) = map.get_mut(id) {
        s.status = "failed".into();
        s.progress = 0;
        s.message = message.into();
        s.error = message.into();
    }
}

fn api_code(v: &Value) -> i64 {
    v.get("code")
        .and_then(|c| {
            c.as_i64()
                .or_else(|| c.as_str().and_then(|s| s.parse::<i64>().ok()))
        })
        .unwrap_or(-1)
}

fn session_json(id: &str) -> Value {
    let map = sessions();
    match map.get(id) {
        Some(s) => json!({
            "sessionId": id,
            "status": s.status,
            "progress": s.progress,
            "message": s.message,
            "downloadedBytes": s.downloaded,
            "totalBytes": s.total,
            "speed": s.speed,
            "version": s.version,
            "error": s.error,
        }),
        None => json!({
            "sessionId": id,
            "status": "completed",
            "progress": 100,
            "message": "会话已结束",
        }),
    }
}

fn find_download_item(items: &[ShareItem], version: &str) -> Option<usize> {
    let matches_version = |item: &ShareItem| {
        let parts: Vec<&str> = version.split('.').collect();
        let name_parts: Vec<&str> = item
            .name
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .collect();
        if name_parts.len() >= 2
            && parts.first().is_some_and(|p| *p == name_parts[0])
            && parts.get(1).is_some_and(|p| *p == name_parts[1])
        {
            return true;
        }
        item.name.contains(version) || version.contains(item.name.as_str())
    };
    items
        .iter()
        .position(|i| i.is_folder && matches_version(i))
        .or_else(|| items.iter().position(|i| i.is_folder))
        .or_else(|| items.iter().position(|i| matches_version(i)))
        .or_else(|| items.iter().position(|_| true))
}

fn common_top_dir(names: &[String]) -> Option<String> {
    let first = names.iter().find(|n| !n.is_empty())?;
    let prefix = first.split('/').next().unwrap_or("");
    if prefix.is_empty() || !first.contains('/') {
        return None;
    }
    let dir_prefix = format!("{}/", prefix);
    for name in names {
        if name.is_empty() {
            continue;
        }
        let value = name.as_str();
        if value == prefix
            || value == dir_prefix.as_str()
            || value.starts_with(dir_prefix.as_str())
        {
            continue;
        }
        return None;
    }
    Some(prefix.to_string())
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<PathBuf, String> {
    use std::io::{Read, Write};
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开压缩包失败：{}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("压缩包无效：{}", e))?;

    let mut names: Vec<String> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| format!("读取压缩包失败：{}", e))?;
        names.push(entry.name().replace('\\', "/"));
    }
    let prefix = common_top_dir(&names);

    std::fs::create_dir_all(dest).map_err(|e| format!("创建版本目录失败：{}", e))?;
    for (i, raw) in names.iter().enumerate() {
        if raw.starts_with('/') || raw.contains("..") || raw.contains(':') {
            continue;
        }
        let mut entry = archive.by_index(i).map_err(|e| format!("读取压缩包失败：{}", e))?;
        let relative = match &prefix {
            Some(p) => {
                let strip_prefix = format!("{}/", p);
                raw.strip_prefix(strip_prefix.as_str()).unwrap_or(raw.as_str())
            }
            None => raw.as_str(),
        };
        if relative.is_empty() {
            continue;
        }
        let target = dest.join(relative);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if entry.is_dir() {
            let _ = std::fs::create_dir_all(&target);
            continue;
        }
        let mut out = std::fs::File::create(&target).map_err(|e| format!("写入文件失败：{}", e))?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = entry.read(&mut buf).map_err(|e| format!("解压失败：{}", e))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| format!("写入文件失败：{}", e))?;
        }
    }

    // 找启动 exe：优先客户端，其次服务端，最后任意 exe
    let candidates = walk_exe(dest);
    let pick = candidates
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()).unwrap_or("").eq_ignore_ascii_case("Minecraft.Windows.exe"))
        .or_else(|| candidates.iter().find(|p| p.file_name().and_then(|n| n.to_str()).unwrap_or("").eq_ignore_ascii_case("bedrock_server.exe")))
        .or_else(|| candidates.first())
        .cloned();
    pick.ok_or_else(|| "压缩包里没有可执行文件（既没有 Minecraft.Windows.exe，也没有 appx/msix 包）".to_string())
}

fn walk_exe(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_exe(&path));
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
            out.push(path);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn install_task(
    app: AppHandle, session_id: String, version: String, link: String, cancel: Arc<AtomicBool>,
) {
    let stage = |p: u32, msg: &str| {
        update_session(&app, &session_id, |s| {
            s.progress = p;
            s.message = msg.to_string();
        });
    };

    if cancel.load(Ordering::Relaxed) {
        update_session(&app, &session_id, |s| { s.status = "cancelled".into(); s.message = "已取消".into(); });
        return;
    }

    stage(3, "正在解析下载链接...");
    let share_url = match resolve_share_url(&link) {
        Ok(v) => v,
        Err(e) => return fail(&app, &session_id, &e),
    };
    let (origin, key, pwd, referer) = match parse_share(&share_url) {
        Ok(v) => v,
        Err(e) => return fail(&app, &session_id, &e),
    };

    stage(8, "正在读取分享文件列表...");
    let token = read_token();
    let items = match list_share_files(&origin, &key, &pwd, &referer, &token) {
        Ok(v) => v,
        Err(e) => return fail(&app, &session_id, &e),
    };
    let index = match find_download_item(&items, &version) {
        Some(i) => i,
        None => return fail(&app, &session_id, "分享里没有匹配的版本文件"),
    };
    let item = &items[index];

    if token.is_empty() {
        return fail(&app, &session_id, "请先登录 123 云盘再下载（客户端不会上传你的账号信息）");
    }

    stage(12, "正在申请下载地址...");
    let raw_url = match request_download_url(&origin, &key, item, &referer, &token) {
        Ok(v) => v,
        Err(e) => return fail(&app, &session_id, &e),
    };
    let final_url = match fetch_final_url(&app, &session_id, &raw_url, &referer) {
        Ok(v) => v,
        Err(e) => return fail(&app, &session_id, &e),
    };

    let tmp_dir = data_dir().join("cache").join("bedrock-download");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let zip_path = tmp_dir.join(format!("{}.zip", safe_name(&version)));
    stage(15, "开始下载...");
    let client = http_client(3600, true);
    let mut resp = match client
        .get(&final_url)
        .header("Referer", &referer)
        .header("User-Agent", UA)
        .send()
    {
        Ok(r) => r,
        Err(e) => return fail(&app, &session_id, &format!("下载失败：{}", e)),
    };
    if !resp.status().is_success() {
        return fail(&app, &session_id, &format!("下载失败，HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    update_session(&app, &session_id, |s| s.total = total);
    let mut out = match std::fs::File::create(&zip_path) {
        Ok(f) => f,
        Err(e) => return fail(&app, &session_id, &format!("创建临时文件失败：{}", e)),
    };
    use std::io::{Read, Write};
    let mut received: u64 = 0;
    let started = Instant::now();
    let mut last_report = Instant::now();
    let mut buf = [0u8; 256 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(&zip_path);
            update_session(&app, &session_id, |s| { s.status = "cancelled".into(); s.message = "已取消".into(); });
            return;
        }
        let n = match resp.read(&mut buf) {
            Ok(n) => n,
            Err(e) => return fail(&app, &session_id, &format!("下载中断：{}", e)),
        };
        if n == 0 {
            break;
        }
        if out.write_all(&buf[..n]).is_err() {
            return fail(&app, &session_id, "写入下载文件失败");
        }
        received += n as u64;
        if last_report.elapsed() >= Duration::from_millis(600) {
            last_report = Instant::now();
            let speed = if started.elapsed().as_secs_f64() > 0.1 {
                (received as f64 / started.elapsed().as_secs_f64()) as u64
            } else {
                0
            };
            let pct = if total > 0 {
                (15 + received.min(total) * 80 / total) as u32
            } else {
                50
            };
            update_session(&app, &session_id, |s| {
                s.downloaded = received;
                s.speed = speed;
                s.progress = pct;
                s.message = if total > 0 {
                    format!("下载中 {:.1}MB / {:.1}MB", received as f64 / 1048576.0, total as f64 / 1048576.0)
                } else {
                    format!("下载中 {:.1}MB", received as f64 / 1048576.0)
                };
            });
        }
    }
    let _ = out.flush();
    drop(out);

    stage(96, "正在解压安装...");
    let version_dir = versions_dir().join(storage::sanitize_version_id(&version));
    let exe = match extract_zip(&zip_path, &version_dir) {
        Ok(exe) => exe,
        Err(e) => return fail(&app, &session_id, &e),
    };
    let relative_exe = exe
        .strip_prefix(&version_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| exe.to_string_lossy().to_string());

    let manifest = json!({
        "version": version,
        "exe": relative_exe,
        "installedAt": now_ms(),
        "source": share_url,
    });
    if let Err(e) = std::fs::write(version_dir.join("bedrock.json"), serde_json::to_string_pretty(&manifest).unwrap_or_default()) {
        return fail(&app, &session_id, &format!("写入安装标记失败：{}", e));
    }
    let _ = std::fs::remove_file(&zip_path);

    update_session(&app, &session_id, |s| {
        s.status = "completed".into();
        s.progress = 100;
        s.message = "安装完成".into();
        s.downloaded = received;
    });
}

/// 从本地安装包安装：zip 解压进版本目录；appx/msix 交给系统部署（UWP 包不能解压直接跑）
fn install_local_task(session_id: &str, version: &str, package_path: &str) -> ApiResult {
    let pkg = Path::new(package_path);
    if !pkg.exists() {
        set_session_error(session_id, "找不到这个文件");
        return ApiResult::err(400, "找不到这个文件");
    }
    let ext = pkg
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "zip" => install_zip_task(session_id, version, pkg),
        "appx" | "appxbundle" | "msix" | "msixbundle" => {
            install_appx_task(session_id, version, package_path)
        }
        _ => {
            let msg = "只支持 zip / appx / msix / msixbundle 安装包";
            set_session_error(session_id, msg);
            ApiResult::err(400, msg)
        }
    }
}

/// 解压式安装（免安装整合包）
fn install_zip_task(session_id: &str, version: &str, zip: &Path) -> ApiResult {
    set_session(session_id, "running", 8, "正在解压安装包");
    let version_dir = versions_dir().join(storage::sanitize_version_id(version));
    match extract_zip(zip, &version_dir) {
        Ok(exe) => {
            let relative_exe = exe
                .strip_prefix(&version_dir)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| exe.to_string_lossy().to_string());
            let manifest = json!({
                "version": version,
                "exe": relative_exe,
                "installedAt": now_ms(),
                "source": "local-zip",
            });
            if let Err(e) = std::fs::write(
                version_dir.join("bedrock.json"),
                serde_json::to_string_pretty(&manifest).unwrap_or_default(),
            ) {
                set_session_error(session_id, &format!("写入安装标记失败：{e}"));
                return ApiResult::err(500, &format!("写入安装标记失败：{e}"));
            }
            login_log(&format!("install-local ok: {version} -> {relative_exe}"));
            set_session(session_id, "completed", 100, "安装完成");
            ApiResult::ok(json!({
                "success": true,
                "version": version,
                "exe": relative_exe,
            }))
        }
        Err(e) => {
            // 有些资源把 .appx/.msix 再套一层 zip 发布：解压后没有 exe，就把包找出来交给系统部署
            if let Some(pkg) = find_appx_pkg(&version_dir) {
                let pkg_str = pkg.to_string_lossy().to_string();
                login_log(&format!("install-local: zip 里是 appx，转系统部署 {pkg_str}"));
                return install_appx_task(session_id, version, &pkg_str);
            }
            login_log(&format!("install-local failed: {e}"));
            set_session_error(session_id, &e);
            ApiResult::err(500, &e)
        }
    }
}

/// 在解压出来的目录里找 appx/msix 安装包
fn find_appx_pkg(dir: &Path) -> Option<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_appx_pkg(&path) {
                return Some(found);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "appx" | "appxbundle" | "msix" | "msixbundle"
            )
        }) {
            return Some(path);
        }
    }
    None
}

/// 通过 Add-AppxPackage 把 appx/msix 注册进系统。
/// UWP 应用必须先完成包注册才能运行，官方包解压出来直接跑是没有意义的。
fn install_appx_task(session_id: &str, version: &str, package_path: &str) -> ApiResult {
    login_log(&format!("install-appx start: {version} <- {package_path}"));
    set_session(session_id, "running", 20, "正在部署到系统（大包要几分钟，别关启动器）");

    // PowerShell 单引号字符串里，单引号要写成两个
    let quoted = package_path.replace('\'', "''");
    let script = format!(
        "$OutputEncoding=[Console]::OutputEncoding=[Text.Encoding]::UTF8;\
$ErrorActionPreference='Stop';\
try {{ Add-AppxPackage -Path '{quoted}' -ErrorAction Stop }} \
catch {{ Write-Output ('ERR:' + $_.Exception.Message); exit 1 }};\
Get-AppxPackage -Name Microsoft.MinecraftUWP | Select-Object -First 1 -Property PackageFamilyName,InstallLocation | ConvertTo-Json -Compress"
    );

    let (ok, raw) = match run_powershell(&script) {
        Ok(v) => v,
        Err(e) => {
            login_log(&format!("install-appx spawn failed: {e}"));
            set_session_error(session_id, &e);
            return ApiResult::err(500, &e);
        }
    };

    if !ok {
        // 部署失败但系统里本来就有基岩版（比如同版本重复导入），按已安装登记，至少能启动
        if let Some(pfn) = query_installed_pfn() {
            login_log(&format!("install-appx failed but package present: {pfn} | {raw}"));
            return finish_appx(session_id, version, &pfn, None, "系统里已有基岩版，已登记为已安装");
        }
        let hint = appx_error_hint(&raw);
        login_log(&format!("install-appx failed: {raw}"));
        set_session_error(session_id, &hint);
        return ApiResult::err(500, &hint);
    }

    let info: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    let pfn = utils::get_str(&info, "PackageFamilyName");
    let location = utils::get_str(&info, "InstallLocation");
    if pfn.is_empty() {
        let msg = "包装上去了，但没找到 Minecraft 的包标识，无法启动；请确认这是基岩版的安装包";
        login_log("install-appx: deployed but PackageFamilyName missing");
        set_session_error(session_id, msg);
        return ApiResult::err(500, msg);
    }

    let loc = if location.is_empty() { None } else { Some(location.as_str()) };
    finish_appx(session_id, version, &pfn, loc, "安装完成")
}

/// 写入 appx 的安装标记（bedrock.json），让列表和主页能识别
fn finish_appx(
    session_id: &str,
    version: &str,
    pfn: &str,
    location: Option<&str>,
    message: &str,
) -> ApiResult {
    let aumid = format!("{pfn}!App");
    let version_dir = versions_dir().join(storage::sanitize_version_id(version));
    let _ = std::fs::create_dir_all(&version_dir);
    let manifest = json!({
        "version": version,
        "type": "appx",
        "aumid": aumid,
        "packageFamily": pfn,
        "installLocation": location.unwrap_or(""),
        "installedAt": now_ms(),
        "source": "local-appx",
    });
    if let Err(e) = std::fs::write(
        version_dir.join("bedrock.json"),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    ) {
        let msg = format!("写入安装标记失败：{e}");
        set_session_error(session_id, &msg);
        return ApiResult::err(500, &msg);
    }
    login_log(&format!("install-appx ok: {version} -> {aumid}"));
    set_session(session_id, "completed", 100, message);
    ApiResult::ok(json!({
        "success": true,
        "version": version,
        "type": "appx",
        "aumid": aumid,
    }))
}

/// 跑一次 PowerShell，返回 (是否成功, 输出文本)
fn run_powershell(script: &str) -> Result<(bool, String), String> {
    let mut cmd = std::process::Command::new("powershell");
    cmd.arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script);
    let output = cmd.output().map_err(|e| format!("调用系统部署工具失败：{e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let ok = output.status.success() && !stdout.contains("ERR:");
    let raw = if stdout.contains("ERR:") || ok {
        stdout
    } else {
        stderr
    };
    Ok((ok, raw))
}

/// 查系统里已注册的基岩版包标识
fn query_installed_pfn() -> Option<String> {
    let script = "Get-AppxPackage -Name Microsoft.MinecraftUWP | Select-Object -First 1 -ExpandProperty PackageFamilyName";
    let (ok, raw) = run_powershell(script).ok()?;
    if !ok {
        return None;
    }
    let pfn = raw.trim().to_string();
    if pfn.is_empty() {
        None
    } else {
        Some(pfn)
    }
}

/// 查系统里已注册的基岩版：返回 (包族名, 版本, 安装路径)
fn query_installed_package() -> Option<(String, String, String)> {
    let script = "Get-AppxPackage -Name Microsoft.MinecraftUWP | Select-Object -First 1 -Property PackageFamilyName,Version,InstallLocation | ConvertTo-Json -Compress";
    let (ok, raw) = run_powershell(script).ok()?;
    if !ok {
        return None;
    }
    let info: Value = serde_json::from_str(raw.trim()).ok()?;
    let pfn = info.get("PackageFamilyName").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if pfn.is_empty() {
        return None;
    }
    let ver = info
        .get("Version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let loc = info
        .get("InstallLocation")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((pfn, ver, loc))
}

/// 把 Add-AppxPackage 的失败信息翻成用户能看懂的话
fn appx_error_hint(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let hints: [(&str, &str); 7] = [
        (
            "0x80073cff",
            "系统没开旁加载：设置 → 系统 → 开发者选项 → 打开「开发人员模式」",
        ),
        (
            "0x80073cf3",
            "缺少依赖包（一般是 VCLibs 或 UI.Xaml），先装依赖再重新导入",
        ),
        (
            "0x800b0109",
            "安装包签名不受信任：把包里的 .cer 证书导入「受信任的根证书颁发机构」后重试",
        ),
        ("0x800b010a", "证书链不完整，同上处理"),
        (
            "0x80073cf9",
            "系统策略禁止安装非商店应用，可能被组策略或安全软件拦截",
        ),
        (
            "0x80073d28",
            "系统里已经装了更高版本，先在「设置 → 应用」里卸载旧版",
        ),
        ("0x80070005", "权限不足，请以管理员身份运行启动器"),
    ];
    for (code, hint) in hints {
        if lower.contains(code) {
            return format!("{hint}（{raw}）");
        }
    }
    format!("导入失败：{raw}")
}

fn fail(app: &AppHandle, session_id: &str, msg: &str) {
    update_session(app, session_id, |s| {
        s.status = "failed".into();
        s.error = msg.to_string();
        s.message = msg.to_string();
    });
}

// ==================== 启动 ====================

pub fn launch(version_id: &str) -> Result<String, String> {
    let marker = marker_path(version_id);
    let text = std::fs::read_to_string(&marker).map_err(|e| format!("读取基岩版安装信息失败：{}", e))?;
    // 有些工具（比如 PowerShell 的 Set-Content）会写进 BOM，serde 会直接报 column 1
    let text = text.trim_start_matches('\u{feff}');
    let info: Value = serde_json::from_str(text).map_err(|e| format!("基岩版安装信息损坏：{}", e))?;

    // appx 导入的版本已经在系统里注册，按 AUMID 拉起，不走 exe 路径
    if utils::get_str(&info, "type") == "appx" {
        let aumid = utils::get_str(&info, "aumid");
        if aumid.is_empty() {
            return Err("安装信息里没有应用标识，请重新导入安装包".to_string());
        }
        std::process::Command::new("explorer.exe")
            .arg(format!("shell:appsFolder\\{aumid}"))
            .spawn()
            .map_err(|e| format!("启动基岩版失败：{}", e))?;
        return Ok("基岩版已启动".to_string());
    }

    let exe_rel = utils::get_str(&info, "exe");
    if exe_rel.is_empty() {
        return Err("安装信息里没有启动程序".to_string());
    }
    let version_dir = marker.parent().unwrap_or(Path::new(".")).to_path_buf();
    let exe = version_dir.join(&exe_rel);
    if !exe.exists() {
        return Err(format!("启动程序不存在：{}", exe.display()));
    }
    std::process::Command::new(&exe)
        .current_dir(&version_dir)
        .spawn()
        .map_err(|e| format!("启动基岩版失败：{}", e))?;
    Ok("基岩版已启动".to_string())
}

// ==================== 路由 ====================

pub async fn handle(
    app: &AppHandle,
    method: &str,
    path: &str,
    params: &Option<Value>,
    body: &Option<Value>,
) -> Option<ApiResult> {
    let key = format!("{} {}", method.to_uppercase(), path);
    match key.as_str() {
        "GET /api/bedrock/versions" => {
            let force = params
                .as_ref()
                .and_then(|p| p.get("refresh"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| s == "true");
            let installed = installed_versions();
            let result = tokio::task::spawn_blocking(move || fetch_manifest(force)).await;
            match result {
                Ok(Ok(list)) => {
                    login_log(&format!("versions: ok, {} items", list.len()));
                    let versions: Vec<Value> = list
                        .into_iter()
                        .map(|v| {
                            let version = utils::get_str(&v, "version");
                            let id = utils::get_str(&v, "id");
                            let links = v.get("downloadlinks").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                            // mcappx 给的是 ISO 日期字符串，MineBBS 给的是秒级时间戳
                            let time = match v.get("time") {
                                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                                Some(Value::Number(n)) => utils::ts_to_iso(n.as_u64().unwrap_or(0)),
                                _ => String::new(),
                            };
                            let matched = match_installed(&version, &id, &installed);
                            let installed_flag = matched.is_some();
                            // 已装时用真实目录名做 id，启动时才能找到 bedrock.json
                            let display_version = matched.clone().unwrap_or_else(|| version.clone());
                            json!({
                                "id": id,
                                "version": display_version,
                                "manifestVersion": version,
                                "size": v.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
                                "time": time,
                                "abstract": utils::get_str(&v, "abstract"),
                                "wiki": utils::get_str(&v, "wikiLinks"),
                                "links": links,
                                "installed": installed_flag,
                                "isBedrock": true,
                            })
                        })
                        .collect();
                    Some(ApiResult::ok(json!({ "versions": versions })))
                }
                Ok(Err(e)) => {
                    login_log(&format!("versions: fetch failed: {e}"));
                    Some(ApiResult::err(502, &e))
                }
                Err(e) => {
                    login_log(&format!("versions: join error: {e}"));
                    Some(ApiResult::err(500, "清单任务异常"))
                }
            }
        }

        // 在系统浏览器打开下载页：mcappx 源没有 123 短链，直接打开版本库官网
        "POST /api/bedrock/open-share" => {
            let data = body.as_ref().or(params.as_ref()).cloned().unwrap_or(Value::Null);
            let link = utils::get_str(&data, "link");
            let version = utils::get_str(&data, "version");
            if link.is_empty() {
                // mcappx 清单没有 123 短链，直接打开该版本的详情页
                let url = mcappx_version_url(&version);
                if let Err(e) = open::that(&url) {
                    return Some(ApiResult::err(500, &format!("无法打开浏览器：{e}")));
                }
                login_log(&format!("open-share: {version} -> {url} (mcappx)"));
                return Some(ApiResult::ok(json!({
                    "success": true,
                    "url": url,
                    "version": version,
                    "site": "mcappx",
                })));
            }
            match resolve_share_url(&link) {
                Ok(url) => {
                    if let Err(e) = open::that(&url) {
                        return Some(ApiResult::err(500, &format!("无法打开浏览器：{e}")));
                    }
                    login_log(&format!("open-share: {version} -> {url}"));
                    Some(ApiResult::ok(json!({
                        "success": true,
                        "url": url,
                        "version": version,
                    })))
                }
                Err(e) => Some(ApiResult::err(502, &e)),
            }
        }

        // 从本地安装包导入：浏览器下载完的包，由用户在这里选中导入
        // 部署 appx 可能要好几分钟，所以做成异步会话，前端轮询进度而不是干等 HTTP
        "POST /api/bedrock/install-local" => {
            let data = body.as_ref().or(params.as_ref()).cloned().unwrap_or(Value::Null);
            let version = utils::get_str(&data, "version");
            let package_path = utils::get_str(&data, "zipPath");
            if version.is_empty() || package_path.is_empty() {
                return Some(ApiResult::err(400, "缺少版本号或文件路径"));
            }
            let session_id = uuid::Uuid::new_v4().to_string();
            {
                let mut map = sessions();
                map.insert(
                    session_id.clone(),
                    BedrockSession {
                        status: "running".into(),
                        progress: 0,
                        message: "准备导入安装包".into(),
                        version: version.clone(),
                        ..Default::default()
                    },
                );
            }
            let sid = session_id.clone();
            tokio::task::spawn_blocking(move || {
                let _ = install_local_task(&sid, &version, &package_path);
            });
            Some(ApiResult::ok(json!({ "success": true, "sessionId": session_id })))
        }

        // 系统里已经装好基岩版（商店装过、或者被别的方式侧载过），直接认领进版本列表
        "POST /api/bedrock/adopt-installed" => match query_installed_package() {
            Some((pfn, ver, loc)) => {
                let version = if ver.is_empty() { "system".to_string() } else { ver };
                let loc_ref = if loc.is_empty() { None } else { Some(loc.as_str()) };
                Some(finish_appx("-", &version, &pfn, loc_ref, "已登记系统里的基岩版"))
            }
            None => Some(ApiResult::err(
                404,
                "系统里没找到基岩版，请先用「从本地包导入」安装",
            )),
        },

        "POST /api/bedrock/install" => {
            let data = body.as_ref().or(params.as_ref()).cloned().unwrap_or(Value::Null);
            let version = utils::get_str(&data, "version");
            let link = utils::get_str(&data, "link");
            if version.is_empty() || link.is_empty() {
                return Some(ApiResult::err(400, "缺少 version 或 link"));
            }
            if marker_path(&version).exists() {
                return Some(ApiResult::ok(json!({ "success": false, "alreadyInstalled": true, "message": "该基岩版已安装" })));
            }
            let session_id = uuid::Uuid::new_v4().to_string();
            let cancel = Arc::new(AtomicBool::new(false));
            {
                let mut map = sessions();
                map.insert(session_id.clone(), BedrockSession {
                    status: "running".into(),
                    progress: 0,
                    message: "准备下载".into(),
                    version: version.clone(),
                    cancel: Some(cancel.clone()),
                    ..Default::default()
                });
            }
            let app_clone = app.clone();
            let sid = session_id.clone();
            tokio::task::spawn_blocking(move || {
                install_task(app_clone, sid, version, link, cancel);
            });
            Some(ApiResult::ok(json!({ "success": true, "sessionId": session_id })))
        }

        "GET /api/bedrock/install-progress" => {
            let sid = params
                .as_ref()
                .and_then(|p| p.get("sessionId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if sid.is_empty() {
                return Some(ApiResult::err(400, "缺少 sessionId"));
            }
            Some(ApiResult::ok(session_json(sid)))
        }

        "POST /api/bedrock/install-cancel" => {
            let data = body.as_ref().or(params.as_ref()).cloned().unwrap_or(Value::Null);
            let sid = utils::get_str(&data, "sessionId");
            let mut map = sessions();
            if let Some(s) = map.get_mut(&sid)
                && let Some(flag) = &s.cancel
            {
                flag.store(true, Ordering::Relaxed);
            }
            Some(ApiResult::ok(json!({ "success": true })))
        }

        "GET /api/bedrock/login-status" => {
            let state = login_state()
                .lock()
                .map(|s| (*s).clone())
                .unwrap_or_default();
            let has_token = !read_token().is_empty();
            Some(ApiResult::ok(json!({
                "status": if has_token && state.status.is_empty() { "success" } else { state.status.as_str() },
                "message": if has_token && state.message.is_empty() { "已登录" } else { state.message.as_str() },
                "loggedIn": has_token,
            })))
        }

        "POST /api/bedrock/login-start" => {
            if !read_token().is_empty() {
                return Some(ApiResult::ok(json!({ "success": true, "loggedIn": true })));
            }
            match start_login_browser(app) {
                Ok(_) => Some(ApiResult::ok(json!({ "success": true, "loggedIn": false }))),
                Err(e) => Some(ApiResult::err(500, &e)),
            }
        }

        "POST /api/bedrock/logout" => {
            let _ = std::fs::remove_file(token_path());
            set_login("idle", "");
            Some(ApiResult::ok(json!({ "success": true })))
        }

        _ => None,
    }
}
