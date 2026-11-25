#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(unsafe_op_in_unsafe_fn)]

//! Один процесс: иконка в трее + окно на нативном Win32, которое создаётся по требованию
//! и уничтожается при закрытии. Тема окна (светлая/тёмная) берётся из настроек Windows.
//!
//! Zapret и tgwsproxy скачиваются из GitHub Releases (latest) — ссылки на версии нигде не зашиты.

use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::env;
use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicI32, AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawFocusRect, DrawTextW, FillRect, FrameRect, GetDC,
    GetDeviceCaps, HBRUSH, HDC, HFONT, InvalidateRect, LineTo, MoveToEx, ReleaseDC, SelectObject, SetBkColor,
    SetBkMode, SetTextColor, UpdateWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RegGetValueW};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, SetWindowTheme};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW,
    GetClientRect, GetMessageW, GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW,
    LoadCursorW, LoadIconW, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageW, SetCursor,
    SetForegroundWindow, SetWindowTextW, ShowWindow, TranslateMessage, WNDCLASSEXW,
};

const TELEGRAM_CHANNEL_URL: &str = "https://t.me/fan4ez231";
const CONFIG_FILE_NAME: &str = "tgwszapret_config.json";
const TASK_NAME: &str = "TGWSZapretLauncher";
const APP_MUTEX: &str = r"Local\TGWSZapretLauncherApp";
const GUI_CLASS: &str = "TGWSZapretLauncherWnd";
const CTL_CLASS: &str = "TGWSZapretLauncherCtl";
const USER_AGENT: &str = "tgwszapret-launcher";
const CREATE_NO_WINDOW: u32 = 0x08000000;
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3600);

// ───────────────────────── утилиты ─────────────────────────

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hide_console_window(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

fn exe_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// true — мы первые; false — такой экземпляр уже запущен. Handle живёт до конца процесса.
fn acquire_single_instance(name: &str) -> bool {
    let w = wide(name);
    unsafe {
        let handle = CreateMutexW(null(), 0, w.as_ptr());
        if handle.is_null() {
            return true;
        }
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

fn norm_path(s: &str) -> String {
    s.to_lowercase().replace('/', "\\")
}

fn path_under(path: &str, dir: &Path) -> bool {
    let d = norm_path(&dir.display().to_string());
    let prefix = format!("{}\\", d.trim_end_matches('\\'));
    norm_path(path).starts_with(&prefix)
}

fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

// ───────────────────────── автозапуск ─────────────────────────

fn run_schtasks(args: &[&str]) -> io::Result<std::process::Output> {
    let mut cmd = Command::new("schtasks");
    cmd.args(args);
    hide_console_window(&mut cmd);
    cmd.output()
}

fn autostart_task_exists() -> bool {
    run_schtasks(&["/Query", "/TN", TASK_NAME])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn build_task_xml(exe: &str, dir: &str, user: &str) -> String {
    let command = xml_escape(&format!("\"{exe}\""));
    let dir = xml_escape(dir);
    let user = xml_escape(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>TGWS + Zapret Launcher autostart</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
      <Delay>PT10S</Delay>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>--autostart</Arguments>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn set_autostart(enabled: bool) -> Result<(), String> {
    if !enabled {
        let _ = run_schtasks(&["/Delete", "/TN", TASK_NAME, "/F"]);
        return Ok(());
    }

    let exe_path = env::current_exe().map_err(|e| format!("путь exe: {e}"))?;
    let exe_str = exe_path.display().to_string();
    let dir_str = exe_dir().display().to_string();

    let username = env::var("USERNAME").unwrap_or_default();
    let domain = env::var("USERDOMAIN").unwrap_or_default();
    let user = if domain.is_empty() {
        username
    } else {
        format!("{domain}\\{username}")
    };

    let xml = build_task_xml(&exe_str, &dir_str, &user);

    let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }

    let xml_path = env::temp_dir().join("tgwszapret_task.xml");
    fs::write(&xml_path, &bytes).map_err(|e| format!("временный xml: {e}"))?;

    let xml_path_str = xml_path.display().to_string();
    let result = run_schtasks(&["/Create", "/TN", TASK_NAME, "/XML", &xml_path_str, "/F"]);
    let _ = fs::remove_file(&xml_path);

    match result {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("ошибка автозапуска: {}", err.trim()))
        }
        Err(e) => Err(format!("schtasks: {e}")),
    }
}

// ───────────────────────── процессы ─────────────────────────

fn taskkill_by_name(exe_name: &str) -> bool {
    if exe_name.trim().is_empty() {
        return false;
    }
    let mut cmd = Command::new("taskkill");
    cmd.args(["/IM", exe_name, "/F", "/T"]);
    hide_console_window(&mut cmd);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn is_process_running(exe_name: &str) -> bool {
    if exe_name.trim().is_empty() {
        return false;
    }
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", &format!("IMAGENAME eq {exe_name}"), "/NH", "/FO", "CSV"]);
    hide_console_window(&mut cmd);
    cmd.output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains(&exe_name.to_lowercase())
        })
        .unwrap_or(false)
}

fn stop_windivert_service(service_name: &str) {
    let mut cmd = Command::new("sc");
    cmd.args(["stop", service_name]);
    hide_console_window(&mut cmd);
    let _ = cmd.output();
}

// ───────────────────────── конфиг ─────────────────────────

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone)]
struct SavedConfig {
    default_dir: String,
    zapret_bat_path: String,
    tgws_file_path: String,
    use_zapret: bool,
    use_tgws: bool,
    #[serde(default)]
    autostart: bool,
    #[serde(default = "default_true")]
    auto_update: bool,
    #[serde(default)]
    zapret_version: String,
    #[serde(default)]
    tgws_version: String,
}

impl Default for SavedConfig {
    fn default() -> Self {
        Self {
            default_dir: r"C:\tgwszapret".to_string(),
            zapret_bat_path: String::new(),
            tgws_file_path: String::new(),
            use_zapret: true,
            use_tgws: true,
            autostart: false,
            auto_update: true,
            zapret_version: String::new(),
            tgws_version: String::new(),
        }
    }
}

fn load_config() -> SavedConfig {
    let exe_cfg = exe_dir().join(CONFIG_FILE_NAME);
    if let Ok(text) = fs::read_to_string(&exe_cfg) {
        if let Ok(cfg) = serde_json::from_str::<SavedConfig>(&text) {
            return cfg;
        }
    }

    let fallback_path = PathBuf::from(CONFIG_FILE_NAME);
    if let Ok(text) = fs::read_to_string(&fallback_path) {
        if let Ok(cfg) = serde_json::from_str::<SavedConfig>(&text) {
            return cfg;
        }
    }

    let default_cfg = SavedConfig::default();
    let dir_cfg_path = Path::new(&default_cfg.default_dir).join(CONFIG_FILE_NAME);
    if let Ok(text) = fs::read_to_string(&dir_cfg_path) {
        if let Ok(cfg) = serde_json::from_str::<SavedConfig>(&text) {
            return cfg;
        }
    }

    default_cfg
}

fn save_config(cfg: &SavedConfig) {
    let Ok(json) = serde_json::to_string_pretty(cfg) else {
        return;
    };

    let exe_cfg = exe_dir().join(CONFIG_FILE_NAME);
    let _ = fs::write(&exe_cfg, &json);

    let dir = Path::new(&cfg.default_dir);
    if dir.exists() || fs::create_dir_all(dir).is_ok() {
        let dir_cfg = dir.join(CONFIG_FILE_NAME);
        if dir_cfg != exe_cfg {
            let _ = fs::write(&dir_cfg, &json);
        }
    }
}

fn build_tray_icon_image() -> Icon {
    let width = 16u32;
    let height = 16u32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);

    for top_y in 0..height {
        for x in 0..width {
            let is_border = x == 0 || top_y == 0 || x == width - 1 || top_y == height - 1;
            let vert_bar = (4..=5).contains(&x) && (3..=12).contains(&top_y);
            let top_bar = (4..=11).contains(&x) && (3..=4).contains(&top_y);
            let mid_bar = (4..=10).contains(&x) && (7..=8).contains(&top_y);
            let bot_bar = (4..=11).contains(&x) && (11..=12).contains(&top_y);

            let is_black = vert_bar || top_bar || mid_bar || bot_bar;
            if is_black {
                rgba.extend_from_slice(&[10, 10, 10, 255]);
            } else if is_border {
                rgba.extend_from_slice(&[210, 210, 210, 255]);
            } else {
                rgba.extend_from_slice(&[255, 255, 255, 255]);
            }
        }
    }

    Icon::from_rgba(rgba, width, height).expect("failed to create tray icon")
}

// ───────────────────────── компоненты: запуск / остановка ─────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Component {
    Zapret,
    Tgws,
}

impl Component {
    fn label(self) -> &'static str {
        match self {
            Component::Zapret => "zapret",
            Component::Tgws => "tgwsproxy",
        }
    }
    fn dir_name(self) -> &'static str {
        self.label()
    }
    fn repo(self) -> &'static str {
        match self {
            Component::Zapret => "Flowseal/zapret-discord-youtube",
            Component::Tgws => "Flowseal/tg-ws-proxy",
        }
    }
}

fn work_dir_for(default_dir: &str, target_path: &str) -> PathBuf {
    Path::new(target_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default_dir))
}

fn launch_component(comp: Component, default_dir: &str, path: &str) -> Result<(), String> {
    let work_dir = work_dir_for(default_dir, path);
    match comp {
        Component::Zapret => {
            // raw_arg + двойные кавычки: иначе cmd ломается на путях вида "general (ALT).bat"
            let mut cmd = Command::new("cmd");
            cmd.raw_arg(format!("/D /C \"\"{path}\"\"")).current_dir(work_dir);
            hide_console_window(&mut cmd);
            cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
        }
        Component::Tgws => {
            let mut cmd = Command::new(path);
            cmd.current_dir(work_dir);
            hide_console_window(&mut cmd);
            cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
        }
    }
}

fn component_running(comp: Component, tgws_name: &str) -> bool {
    match comp {
        Component::Zapret => is_process_running("winws.exe") || is_process_running("winws64.exe"),
        Component::Tgws => is_process_running(tgws_name),
    }
}

fn stop_component(comp: Component, tgws_name: &str) -> bool {
    match comp {
        Component::Zapret => {
            let a = taskkill_by_name("winws.exe");
            let b = taskkill_by_name("winws64.exe");
            stop_windivert_service("windivert");
            stop_windivert_service("windivert14");
            a || b
        }
        Component::Tgws => {
            let a = taskkill_by_name(tgws_name);
            let b = taskkill_by_name("TgWsProxy_windows.exe");
            a || b
        }
    }
}

fn start_selected(cfg: &SavedConfig) -> String {
    if !cfg.use_zapret && !cfg.use_tgws {
        return "Ничего не выбрано: отметь Zapret и/или tgwsproxy.".to_string();
    }

    if cfg.use_zapret {
        if cfg.zapret_bat_path.trim().is_empty() {
            return "Укажи путь к .bat файлу zapret (или нажми «Скачать / обновить zapret»).".to_string();
        }
        if !Path::new(&cfg.zapret_bat_path).is_file() {
            return format!("Не найден батник zapret: {}", cfg.zapret_bat_path);
        }
    }

    if cfg.use_tgws {
        if cfg.tgws_file_path.trim().is_empty() {
            return "Укажи путь к файлу tgwsproxy (или нажми «Скачать / обновить tgwsproxy»).".to_string();
        }
        if !Path::new(&cfg.tgws_file_path).is_file() {
            return format!("Не найден файл tgwsproxy: {}", cfg.tgws_file_path);
        }
    }

    let mut started: Vec<&str> = Vec::new();
    let mut already: Vec<&str> = Vec::new();

    if cfg.use_zapret {
        if component_running(Component::Zapret, "") {
            already.push("zapret");
        } else if let Err(e) = launch_component(Component::Zapret, &cfg.default_dir, &cfg.zapret_bat_path) {
            return format!("Ошибка запуска zapret: {e}");
        } else {
            started.push("zapret");
        }
    }

    if cfg.use_tgws {
        let name = file_name_of(&cfg.tgws_file_path);
        if component_running(Component::Tgws, &name) {
            already.push("tgwsproxy");
        } else if let Err(e) = launch_component(Component::Tgws, &cfg.default_dir, &cfg.tgws_file_path) {
            return format!("Ошибка запуска tgwsproxy: {e}");
        } else {
            started.push("tgwsproxy");
        }
    }

    match (started.is_empty(), already.is_empty()) {
        (false, true) => format!("Запущено: {}", started.join(" + ")),
        (false, false) => format!("Запущено: {}. Уже работало: {}", started.join(" + "), already.join(" + ")),
        (true, false) => format!("Уже запущено: {}", already.join(" + ")),
        (true, true) => "Нечего запускать.".to_string(),
    }
}

fn stop_all(cfg: &SavedConfig) -> String {
    let name = file_name_of(&cfg.tgws_file_path);
    stop_component(Component::Tgws, &name);
    let killed = stop_component(Component::Zapret, "");
    if killed {
        "Остановка выполнена (zapret и tgwsproxy остановлены).".to_string()
    } else {
        "Остановка выполнена.".to_string()
    }
}

// ───────────────────────── обновления с GitHub ─────────────────────────

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

struct LatestRelease {
    tag: String,
    asset_name: String,
    url: String,
}

struct UpdateResult {
    changed: bool,
    path: String,
    version: String,
    message: String,
}

/// `overall` — жёсткий предел на весь запрос (включая скачивание тела), чтобы ничего не висело вечно.
fn http_agent(overall: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .timeout(overall)
        .build()
}

fn pick_asset(comp: Component, assets: &[GhAsset]) -> Option<&GhAsset> {
    match comp {
        Component::Zapret => assets.iter().find(|a| a.name.to_lowercase().ends_with(".zip")),
        Component::Tgws => {
            let exes: Vec<&GhAsset> = assets
                .iter()
                .filter(|a| a.name.to_lowercase().ends_with(".exe"))
                .collect();
            exes.iter()
                .copied()
                .find(|a| a.name.eq_ignore_ascii_case("TgWsProxy_windows.exe"))
                .or_else(|| {
                    exes.iter().copied().find(|a| {
                        let n = a.name.to_lowercase();
                        n.contains("windows") && !n.contains('7')
                    })
                })
                .or_else(|| exes.first().copied())
        }
    }
}

fn fetch_latest_release(comp: Component) -> Result<LatestRelease, String> {
    let api = format!("https://api.github.com/repos/{}/releases/latest", comp.repo());
    let text = http_agent(Duration::from_secs(30))
        .get(&api)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("GitHub API: {e}"))?
        .into_string()
        .map_err(|e| format!("ответ GitHub: {e}"))?;

    let rel: GhRelease =
        serde_json::from_str(&text).map_err(|e| format!("разбор ответа GitHub: {e}"))?;

    let asset = pick_asset(comp, &rel.assets)
        .ok_or_else(|| format!("в релизе {} нет подходящего файла", rel.tag_name))?;

    Ok(LatestRelease {
        tag: rel.tag_name.clone(),
        asset_name: asset.name.clone(),
        url: asset.browser_download_url.clone(),
    })
}

fn norm_version(v: &str) -> String {
    v.trim().trim_start_matches(|c| c == 'v' || c == 'V').to_string()
}

fn same_version(a: &str, b: &str) -> bool {
    !a.trim().is_empty() && norm_version(a) == norm_version(b)
}

/// Качает на диск потоком и сообщает прогресс (МБ), чтобы в окне было видно, что происходит.
fn download_to_file(url: &str, dest: &Path, label: &str, progress: &dyn Fn(String)) -> Result<(), String> {
    progress(format!("Скачиваю {label}..."));

    let resp = http_agent(Duration::from_secs(1800))
        .get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("сеть: {e}"))?;

    let total: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);

    let mut reader = resp.into_reader();
    let mut file = File::create(dest).map_err(|e| format!("временный файл: {e}"))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_report: Option<Instant> = None;

    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("загрузка прервана: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("запись: {e}"))?;
        done += n as u64;

        let due = last_report
            .map(|t| t.elapsed() > Duration::from_millis(300))
            .unwrap_or(true);
        if due {
            last_report = Some(Instant::now());
            let done_mb = done as f64 / 1_048_576.0;
            if total > 0 {
                progress(format!(
                    "Скачиваю {label}: {done_mb:.1} / {:.1} МБ",
                    total as f64 / 1_048_576.0
                ));
            } else {
                progress(format!("Скачиваю {label}: {done_mb:.1} МБ"));
            }
        }
    }
    Ok(())
}

/// Распаковка zip. Общая корневая папка архива срезается, чтобы структура не менялась между версиями.
/// Существующие пользовательские списки (*-user.txt) не перезаписываются.
fn extract_zip(zip_path: &Path, out_dir: &Path) -> Result<(), String> {
    let file = File::open(zip_path).map_err(|e| format!("открытие архива: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(io::BufReader::new(file)).map_err(|e| format!("архив ZIP: {e}"))?;

    let mut names: Vec<(PathBuf, bool)> = Vec::new();
    for i in 0..archive.len() {
        let f = archive.by_index(i).map_err(|e| format!("элемент ZIP: {e}"))?;
        if let Some(n) = f.enclosed_name() {
            names.push((n.to_path_buf(), f.is_dir()));
        }
    }

    let root = names
        .first()
        .and_then(|(n, _)| n.components().next())
        .map(|c| c.as_os_str().to_os_string());
    let strip = match &root {
        Some(r) => names.iter().all(|(n, is_dir)| {
            n.components()
                .next()
                .map(|c| c.as_os_str() == r.as_os_str())
                .unwrap_or(false)
                && (n.components().count() >= 2 || *is_dir)
        }),
        None => false,
    };

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| format!("элемент ZIP: {e}"))?;
        let Some(name) = file.enclosed_name().map(|n| n.to_path_buf()) else {
            continue;
        };
        let rel: PathBuf = if strip {
            name.components().skip(1).collect()
        } else {
            name
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out_path = out_dir.join(&rel);

        if file.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| format!("папка: {e}"))?;
            continue;
        }

        let lower_name = rel
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        if lower_name.ends_with("-user.txt") && out_path.exists() {
            continue;
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("родительская папка: {e}"))?;
        }
        let mut outfile = File::create(&out_path).map_err(|e| format!("создание {}: {e}", rel.display()))?;
        io::copy(&mut file, &mut outfile).map_err(|e| format!("запись {}: {e}", rel.display()))?;
    }
    Ok(())
}

fn replace_file(src: &Path, dest: &Path) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..6 {
        let _ = fs::remove_file(dest);
        match fs::rename(src, dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                thread::sleep(Duration::from_millis(500));
            }
        }
    }
    Err(format!("замена файла: {last}"))
}

fn collect_files_with_ext(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if path
                .extension()
                .and_then(|s| s.to_str())
                .map(|e| e.eq_ignore_ascii_case(ext))
                .unwrap_or(false)
            {
                out.push(path);
            }
        } else if path.is_dir() {
            collect_files_with_ext(&path, ext, out);
        }
    }
}

fn lower_file_name(p: &Path) -> String {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase()
}

/// Оставляет текущий .bat, если он на месте, иначе ищет general.bat (потом любой не service*.bat).
fn pick_zapret_bat(managed: &Path, current: &str) -> Result<String, String> {
    if !current.trim().is_empty() && Path::new(current).is_file() && path_under(current, managed) {
        return Ok(current.to_string());
    }
    let mut all: Vec<PathBuf> = Vec::new();
    collect_files_with_ext(managed, "bat", &mut all);
    all.sort();

    let best = all
        .iter()
        .find(|p| lower_file_name(p) == "general.bat")
        .or_else(|| all.iter().find(|p| !lower_file_name(p).starts_with("service")))
        .or_else(|| all.first());

    best.map(|p| p.display().to_string())
        .ok_or_else(|| format!("в {} не найден .bat — выбери файл вручную", managed.display()))
}

/// Проверяет последний релиз и при необходимости ставит его.
/// `install_unmanaged = true` — ставим в общую папку, даже если путь был задан вручную (нажата кнопка).
/// `false` — трогаем только установку, лежащую в общей папке.
fn update_component(
    comp: Component,
    cfg: &SavedConfig,
    install_unmanaged: bool,
    progress: &dyn Fn(String),
) -> Result<UpdateResult, String> {
    let (cur_path, cur_ver) = match comp {
        Component::Zapret => (cfg.zapret_bat_path.clone(), cfg.zapret_version.clone()),
        Component::Tgws => (cfg.tgws_file_path.clone(), cfg.tgws_version.clone()),
    };
    let managed = Path::new(&cfg.default_dir).join(comp.dir_name());
    let cur_exists = !cur_path.trim().is_empty() && Path::new(&cur_path).is_file();
    let cur_managed = cur_exists && path_under(&cur_path, &managed);

    let unchanged = |message: String| UpdateResult {
        changed: false,
        path: cur_path.clone(),
        version: cur_ver.clone(),
        message,
    };

    if !install_unmanaged && !cur_managed {
        return Ok(unchanged(format!("{}: автообновление пропущено", comp.label())));
    }

    let latest = fetch_latest_release(comp)?;

    if cur_managed && same_version(&cur_ver, &latest.tag) {
        return Ok(unchanged(format!(
            "{}: уже последняя версия ({})",
            comp.label(),
            latest.tag
        )));
    }

    fs::create_dir_all(&managed).map_err(|e| format!("создание папки {}: {e}", managed.display()))?;
    let tmp = managed.join(".download.tmp");
    let label = format!("{} {}", comp.label(), latest.tag);
    if let Err(e) = download_to_file(&latest.url, &tmp, &label, progress) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    let old_tgws_name = file_name_of(&cur_path);
    let stop_name = if old_tgws_name.is_empty() {
        latest.asset_name.clone()
    } else {
        old_tgws_name
    };

    progress(format!("Устанавливаю {label}..."));
    let was_running = component_running(comp, &stop_name);
    if was_running {
        stop_component(comp, &stop_name);
        thread::sleep(Duration::from_millis(1500));
    }

    let install_res: Result<String, String> = match comp {
        Component::Zapret => extract_zip(&tmp, &managed).and_then(|_| pick_zapret_bat(&managed, &cur_path)),
        Component::Tgws => {
            let dest = managed.join(&latest.asset_name);
            replace_file(&tmp, &dest).map(|_| dest.display().to_string())
        }
    };
    let _ = fs::remove_file(&tmp);

    let final_path = match &install_res {
        Ok(p) => p.clone(),
        Err(_) => cur_path.clone(),
    };
    if was_running && Path::new(&final_path).is_file() {
        let _ = launch_component(comp, &cfg.default_dir, &final_path);
    }

    let new_path = install_res?;

    // Если у tgwsproxy поменялось имя файла — убираем старый exe из общей папки
    if comp == Component::Tgws && cur_managed && norm_path(&cur_path) != norm_path(&new_path) {
        let _ = fs::remove_file(&cur_path);
    }

    let message = if cur_ver.trim().is_empty() {
        format!("{} установлен: {}", comp.label(), latest.tag)
    } else {
        format!("{} обновлён: {} → {}", comp.label(), cur_ver, latest.tag)
    };

    Ok(UpdateResult {
        changed: true,
        path: new_path,
        version: latest.tag,
        message,
    })
}

fn apply_result(cfg: &mut SavedConfig, comp: Component, r: &UpdateResult) {
    match comp {
        Component::Zapret => {
            cfg.zapret_bat_path = r.path.clone();
            cfg.zapret_version = r.version.clone();
        }
        Component::Tgws => {
            cfg.tgws_file_path = r.path.clone();
            cfg.tgws_version = r.version.clone();
        }
    }
}

// ───────────────────────── общее состояние приложения ─────────────────────────

struct AppState {
    cfg: SavedConfig,
    status: String,
    busy: bool,
}

static STATE: OnceLock<Mutex<AppState>> = OnceLock::new();

fn gui() -> MutexGuard<'static, AppState> {
    STATE
        .get_or_init(|| {
            Mutex::new(AppState {
                cfg: load_config(),
                status: "Готов к запуску.".to_string(),
                busy: false,
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn persist_gui() {
    let cfg = gui().cfg.clone();
    save_config(&cfg);
}

fn set_status(msg: impl Into<String>) {
    gui().status = msg.into();
}

// Сообщения скрытого окна-контроллера (живёт всё время работы процесса, принимает команды от потоков)
const WM_APP_REFRESH: u32 = 0x8000 + 1;
const WM_APP_OPEN: u32 = 0x8000 + 2;
const WM_APP_QUIT: u32 = 0x8000 + 3;

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static GUI_HWND: AtomicIsize = AtomicIsize::new(0);

fn post_to_main(msg: u32) {
    let h = MAIN_HWND.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, msg, 0, 0);
        }
    }
}

fn post_refresh() {
    post_to_main(WM_APP_REFRESH);
}

struct UpdateMode {
    install_unmanaged: bool,
    respect_flags: bool,
    verbose: bool,
}

/// Выполняется в фоновом потоке. Статус и прогресс видны в окне (если оно открыто).
fn run_updates(comps: &[Component], mode: UpdateMode) {
    {
        let mut g = gui();
        if g.busy {
            if mode.verbose {
                g.status = "Загрузка уже выполняется, подожди немного.".to_string();
            }
            drop(g);
            post_refresh();
            return;
        }
        g.busy = true;
    }
    post_refresh();

    for &comp in comps {
        let cfg = gui().cfg.clone();
        if mode.respect_flags {
            if !cfg.auto_update {
                break;
            }
            let used = match comp {
                Component::Zapret => cfg.use_zapret,
                Component::Tgws => cfg.use_tgws,
            };
            if !used {
                continue;
            }
        }

        if mode.verbose {
            set_status(format!("Проверяю последний релиз {}...", comp.label()));
            post_refresh();
        }

        let progress = |s: String| {
            set_status(s);
            post_refresh();
        };

        match update_component(comp, &cfg, mode.install_unmanaged, &progress) {
            Ok(r) => {
                {
                    let mut g = gui();
                    if r.changed {
                        apply_result(&mut g.cfg, comp, &r);
                    }
                    if mode.verbose || r.changed {
                        g.status = r.message.clone();
                    }
                }
                if r.changed {
                    persist_gui();
                }
            }
            Err(e) => set_status(format!("Не удалось обновить {}: {e}", comp.label())),
        }
        post_refresh();
    }

    gui().busy = false;
    post_refresh();
}

fn do_start() {
    let cfg = gui().cfg.clone();
    let msg = start_selected(&cfg);
    set_status(msg);
    post_refresh();
}

fn do_stop() {
    let cfg = gui().cfg.clone();
    let msg = stop_all(&cfg);
    set_status(msg);
    post_refresh();
}

// ───────────────────────── тема (светлая / тёмная) ─────────────────────────

fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

fn system_uses_dark() -> bool {
    unsafe {
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let val = wide("AppsUseLightTheme");
        let mut data: u32 = 1;
        let mut size: u32 = 4;
        let rc = RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            0x10, // RRF_RT_REG_DWORD
            null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        );
        rc == 0 && data == 0
    }
}

#[derive(Clone, Copy)]
struct Theme {
    dark: bool,
    bg: u32,
    text: u32,
    btn_bg: u32,
    btn_border: u32,
    btn_pressed: u32,
    disabled: u32,
    link: u32,
    accent: u32,
    box_bg: u32,
    box_border: u32,
    bg_brush: isize,
    edit_brush: isize,
}

unsafe fn make_theme(dark: bool) -> Theme {
    if dark {
        let bg = rgb(32, 32, 32);
        let edit_bg = rgb(45, 45, 45);
        Theme {
            dark,
            bg,
            text: rgb(240, 240, 240),
            btn_bg: rgb(55, 55, 55),
            btn_border: rgb(95, 95, 95),
            btn_pressed: rgb(75, 75, 75),
            disabled: rgb(120, 120, 120),
            link: rgb(90, 175, 255),
            accent: rgb(0, 120, 215),
            box_bg: edit_bg,
            box_border: rgb(150, 150, 150),
            bg_brush: CreateSolidBrush(bg) as isize,
            edit_brush: CreateSolidBrush(edit_bg) as isize,
        }
    } else {
        let bg = rgb(240, 240, 240);
        let edit_bg = rgb(255, 255, 255);
        Theme {
            dark,
            bg,
            text: rgb(0, 0, 0),
            btn_bg: rgb(225, 225, 225),
            btn_border: rgb(173, 173, 173),
            btn_pressed: rgb(204, 228, 247),
            disabled: rgb(150, 150, 150),
            link: rgb(0, 120, 215),
            accent: rgb(0, 120, 215),
            box_bg: edit_bg,
            box_border: rgb(80, 80, 80),
            bg_brush: CreateSolidBrush(bg) as isize,
            edit_brush: CreateSolidBrush(edit_bg) as isize,
        }
    }
}

thread_local! {
    static THEME: Cell<Option<Theme>> = Cell::new(None);
}

// ───────────────────────── окно (Win32) ─────────────────────────

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
const MAIN_STYLE: u32 = 0x00C0_0000 | 0x0008_0000 | 0x0002_0000; // CAPTION | SYSMENU | MINIMIZEBOX
const BS_OWNERDRAW: u32 = 0x0B;
const ES_AUTOHSCROLL: u32 = 0x80;
const SS_RIGHT: u32 = 0x02;
const SS_NOTIFY: u32 = 0x100;

const WM_CREATE: u32 = 0x0001;
const WM_DESTROY: u32 = 0x0002;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_CLOSE: u32 = 0x0010;
const WM_SETCURSOR: u32 = 0x0020;
const WM_SETFONT: u32 = 0x0030;
const WM_GETFONT: u32 = 0x0031;
const WM_DRAWITEM: u32 = 0x002B;
const WM_COMMAND: u32 = 0x0111;
const WM_CTLCOLOREDIT: u32 = 0x0133;
const WM_CTLCOLORSTATIC: u32 = 0x0138;
const BN_CLICKED: u32 = 0;
const EN_KILLFOCUS: u32 = 0x0200;

const ID_EDIT_DIR: i32 = 101;
const ID_BTN_DIR: i32 = 102;
const ID_BTN_UPD_ZAPRET: i32 = 103;
const ID_BTN_UPD_TGWS: i32 = 104;
const ID_LINK_TG: i32 = 105;
const ID_CHK_ZAPRET: i32 = 106;
const ID_CHK_TGWS: i32 = 107;
const ID_CHK_AUTOSTART: i32 = 108;
const ID_CHK_AUTOUPD: i32 = 109;
const ID_EDIT_BAT: i32 = 110;
const ID_BTN_BAT: i32 = 111;
const ID_EDIT_TGWS: i32 = 112;
const ID_BTN_TGWS: i32 = 113;
const ID_BTN_START: i32 = 114;
const ID_BTN_STOP: i32 = 115;

const CLIENT_W: i32 = 560;
const CLIENT_H: i32 = 452;

static DPI: AtomicI32 = AtomicI32::new(96);

fn sc(v: i32) -> i32 {
    (v as i64 * DPI.load(Ordering::Relaxed) as i64 / 96) as i32
}

fn init_dpi() {
    unsafe {
        let hdc = GetDC(null_mut());
        if !hdc.is_null() {
            let dpi = GetDeviceCaps(hdc, 88); // LOGPIXELSX
            if dpi > 0 {
                DPI.store(dpi, Ordering::Relaxed);
            }
            ReleaseDC(null_mut(), hdc);
        }
    }
}

#[derive(Clone, Copy)]
struct Ui {
    edit_dir: isize,
    edit_bat: isize,
    edit_tgws: isize,
    chk_zapret: isize,
    chk_tgws: isize,
    chk_autostart: isize,
    chk_autoupd: isize,
    btn_upd_zapret: isize,
    btn_upd_tgws: isize,
    status: isize,
    versions: isize,
    link: isize,
    font: isize,
    font_link: isize,
}

thread_local! {
    static UI: Cell<Option<Ui>> = Cell::new(None);
}

unsafe fn mk(
    parent: HWND,
    font: HFONT,
    class: &str,
    text: &str,
    style: u32,
    ex_style: u32,
    (x, y, w, h): (i32, i32, i32, i32),
    id: i32,
) -> isize {
    let hinst = GetModuleHandleW(null());
    let cls = wide(class);
    let txt = wide(text);
    let hwnd = CreateWindowExW(
        ex_style,
        cls.as_ptr(),
        txt.as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        sc(x),
        sc(y),
        sc(w),
        sc(h),
        parent,
        id as usize as *mut c_void,
        hinst,
        null(),
    );
    SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
    hwnd as isize
}

unsafe fn label(parent: HWND, font: HFONT, text: &str, rect: (i32, i32, i32, i32)) -> isize {
    mk(parent, font, "STATIC", text, 0, 0, rect, 0)
}

unsafe fn edit(parent: HWND, font: HFONT, rect: (i32, i32, i32, i32), id: i32) -> isize {
    let h = mk(parent, font, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL, WS_EX_CLIENTEDGE, rect, id);
    if THEME.get().map(|t| t.dark).unwrap_or(false) {
        SetWindowTheme(h as HWND, wide("DarkMode_CFD").as_ptr(), null());
    }
    h
}

unsafe fn button(parent: HWND, font: HFONT, text: &str, rect: (i32, i32, i32, i32), id: i32) -> isize {
    mk(parent, font, "BUTTON", text, WS_TABSTOP | BS_OWNERDRAW, 0, rect, id)
}

unsafe fn checkbox(parent: HWND, font: HFONT, text: &str, rect: (i32, i32, i32, i32), id: i32) -> isize {
    mk(parent, font, "BUTTON", text, WS_TABSTOP | BS_OWNERDRAW, 0, rect, id)
}

unsafe fn make_font(underline: bool) -> HFONT {
    CreateFontW(
        -(9 * DPI.load(Ordering::Relaxed) / 72),
        0,
        0,
        0,
        400,
        0,
        if underline { 1 } else { 0 },
        0,
        1,
        0,
        0,
        5,
        0,
        wide("Segoe UI").as_ptr(),
    )
}

unsafe fn create_controls(hwnd: HWND) {
    let font = make_font(false);
    let font_link = make_font(true);

    label(hwnd, font, "Общая папка (куда качать и складывать):", (12, 12, 536, 18));
    let edit_dir = edit(hwnd, font, (12, 32, 440, 24), ID_EDIT_DIR);
    button(hwnd, font, "Папка...", (460, 32, 88, 24), ID_BTN_DIR);

    let btn_upd_zapret = button(hwnd, font, "Скачать / обновить zapret", (12, 66, 200, 28), ID_BTN_UPD_ZAPRET);
    let btn_upd_tgws = button(hwnd, font, "Скачать / обновить tgwsproxy", (220, 66, 220, 28), ID_BTN_UPD_TGWS);

    label(hwnd, font, "Что запустить:", (12, 106, 536, 18));
    let chk_zapret = checkbox(hwnd, font, "Zapret (.bat)", (12, 126, 200, 20), ID_CHK_ZAPRET);
    let chk_tgws = checkbox(hwnd, font, "tgwsproxy", (220, 126, 200, 20), ID_CHK_TGWS);
    let chk_autostart = checkbox(
        hwnd,
        font,
        "Автозапуск с Windows (запуск программы в трей)",
        (12, 150, 536, 20),
        ID_CHK_AUTOSTART,
    );
    let chk_autoupd = checkbox(
        hwnd,
        font,
        "Автообновление zapret и tgwsproxy (последний релиз с GitHub)",
        (12, 174, 536, 20),
        ID_CHK_AUTOUPD,
    );

    label(hwnd, font, "Путь к .bat файлу zapret:", (12, 206, 536, 18));
    let edit_bat = edit(hwnd, font, (12, 226, 440, 24), ID_EDIT_BAT);
    button(hwnd, font, "Выбрать .bat...", (460, 226, 88, 24), ID_BTN_BAT);

    label(hwnd, font, "Путь к файлу tgwsproxy:", (12, 260, 536, 18));
    let edit_tgws = edit(hwnd, font, (12, 280, 440, 24), ID_EDIT_TGWS);
    button(hwnd, font, "Выбрать файл...", (460, 280, 88, 24), ID_BTN_TGWS);

    button(hwnd, font, "Запустить выбранное", (12, 318, 170, 30), ID_BTN_START);
    button(hwnd, font, "Остановить всё", (190, 318, 150, 30), ID_BTN_STOP);

    let status = label(hwnd, font, "", (12, 360, 536, 36));
    let versions = label(hwnd, font, "", (12, 402, 536, 18));

    label(hwnd, font, "Закрытие окна не останавливает zapret и tgwsproxy.", (12, 424, 360, 18));
    // Ссылка в правом нижнем углу: просто голубой подчёркнутый текст
    let link = mk(
        hwnd,
        font_link,
        "STATIC",
        "Telegram: @fan4ez231",
        SS_RIGHT | SS_NOTIFY,
        0,
        (376, 424, 172, 18),
        ID_LINK_TG,
    );

    UI.set(Some(Ui {
        edit_dir,
        edit_bat,
        edit_tgws,
        chk_zapret,
        chk_tgws,
        chk_autostart,
        chk_autoupd,
        btn_upd_zapret,
        btn_upd_tgws,
        status,
        versions,
        link,
        font: font as isize,
        font_link: font_link as isize,
    }));
}

unsafe fn get_text(h: isize) -> String {
    let hwnd = h as HWND;
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn set_text_if_changed(h: isize, s: &str) {
    if get_text(h) != s {
        SetWindowTextW(h as HWND, wide(s).as_ptr());
    }
}

unsafe fn invalidate(h: isize) {
    InvalidateRect(h as HWND, null(), 1);
}

fn check_state(id: i32) -> bool {
    let g = gui();
    match id {
        ID_CHK_ZAPRET => g.cfg.use_zapret,
        ID_CHK_TGWS => g.cfg.use_tgws,
        ID_CHK_AUTOSTART => g.cfg.autostart,
        ID_CHK_AUTOUPD => g.cfg.auto_update,
        _ => false,
    }
}

unsafe fn refresh_ui() {
    let Some(ui) = UI.get() else {
        return;
    };
    let (cfg, status, busy) = {
        let g = gui();
        (g.cfg.clone(), g.status.clone(), g.busy)
    };

    set_text_if_changed(ui.edit_dir, &cfg.default_dir);
    set_text_if_changed(ui.edit_bat, &cfg.zapret_bat_path);
    set_text_if_changed(ui.edit_tgws, &cfg.tgws_file_path);
    invalidate(ui.chk_autostart);

    set_text_if_changed(ui.status, &format!("Статус: {status}"));

    let zv = if cfg.zapret_version.is_empty() { "—" } else { cfg.zapret_version.as_str() };
    let tv = if cfg.tgws_version.is_empty() { "—" } else { cfg.tgws_version.as_str() };
    set_text_if_changed(ui.versions, &format!("Установлено: zapret {zv}, tgwsproxy {tv}"));

    let enable = if busy { 0 } else { 1 };
    EnableWindow(ui.btn_upd_zapret as HWND, enable);
    EnableWindow(ui.btn_upd_tgws as HWND, enable);
}

unsafe fn sync_from_controls() {
    let Some(ui) = UI.get() else {
        return;
    };
    let d = get_text(ui.edit_dir);
    let b = get_text(ui.edit_bat);
    let t = get_text(ui.edit_tgws);
    {
        let mut g = gui();
        g.cfg.default_dir = d.trim().trim_matches('"').to_string();
        g.cfg.zapret_bat_path = b.trim().trim_matches('"').to_string();
        g.cfg.tgws_file_path = t.trim().trim_matches('"').to_string();
    }
    persist_gui();
}

fn pick_folder_dialog(current: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new();
    let start = PathBuf::from(current);
    if start.exists() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder().map(|p| p.display().to_string())
}

fn pick_file_dialog(current: &str, fallback_dir: &str, filter_name: &str, exts: &[&str]) -> Option<String> {
    let mut dialog = rfd::FileDialog::new().add_filter(filter_name, exts);
    let start_dir: PathBuf = Path::new(current)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback_dir));
    if start_dir.exists() {
        dialog = dialog.set_directory(start_dir);
    }
    dialog.pick_file().map(|p| p.display().to_string())
}

fn request_update(comp: Component) {
    thread::spawn(move || {
        run_updates(
            &[comp],
            UpdateMode {
                install_unmanaged: true,
                respect_flags: false,
                verbose: true,
            },
        );
    });
}

unsafe fn toggle_flag(id: i32, ctrl: isize) {
    {
        let mut g = gui();
        match id {
            ID_CHK_ZAPRET => g.cfg.use_zapret = !g.cfg.use_zapret,
            ID_CHK_TGWS => g.cfg.use_tgws = !g.cfg.use_tgws,
            ID_CHK_AUTOUPD => g.cfg.auto_update = !g.cfg.auto_update,
            _ => {}
        }
    }
    persist_gui();
    invalidate(ctrl);
}

unsafe fn on_command(wparam: WPARAM) {
    let id = (wparam & 0xFFFF) as i32;
    let code = ((wparam >> 16) & 0xFFFF) as u32;
    let Some(ui) = UI.get() else {
        return;
    };

    match id {
        ID_EDIT_DIR | ID_EDIT_BAT | ID_EDIT_TGWS => {
            if code == EN_KILLFOCUS {
                sync_from_controls();
            }
        }
        ID_BTN_DIR if code == BN_CLICKED => {
            let cur = gui().cfg.default_dir.clone();
            if let Some(folder) = pick_folder_dialog(&cur) {
                {
                    let mut g = gui();
                    g.cfg.default_dir = folder.clone();
                    g.status = format!("Общая папка: {folder}");
                }
                persist_gui();
                refresh_ui();
            }
        }
        ID_BTN_UPD_ZAPRET if code == BN_CLICKED => {
            sync_from_controls();
            request_update(Component::Zapret);
        }
        ID_BTN_UPD_TGWS if code == BN_CLICKED => {
            sync_from_controls();
            request_update(Component::Tgws);
        }
        ID_LINK_TG if code == BN_CLICKED => {
            ShellExecuteW(
                null_mut(),
                wide("open").as_ptr(),
                wide(TELEGRAM_CHANNEL_URL).as_ptr(),
                null(),
                null(),
                1,
            );
        }
        ID_CHK_ZAPRET if code == BN_CLICKED => toggle_flag(id, ui.chk_zapret),
        ID_CHK_TGWS if code == BN_CLICKED => toggle_flag(id, ui.chk_tgws),
        ID_CHK_AUTOUPD if code == BN_CLICKED => toggle_flag(id, ui.chk_autoupd),
        ID_CHK_AUTOSTART if code == BN_CLICKED => {
            let want = !check_state(ID_CHK_AUTOSTART);
            match set_autostart(want) {
                Ok(()) => {
                    {
                        let mut g = gui();
                        g.cfg.autostart = want;
                        g.status = if want {
                            "Автозапуск включён.".to_string()
                        } else {
                            "Автозапуск выключен.".to_string()
                        };
                    }
                    persist_gui();
                }
                Err(e) => set_status(e),
            }
            invalidate(ui.chk_autostart);
            refresh_ui();
        }
        ID_BTN_BAT if code == BN_CLICKED => {
            let (cur, def) = {
                let g = gui();
                (g.cfg.zapret_bat_path.clone(), g.cfg.default_dir.clone())
            };
            if let Some(p) = pick_file_dialog(&cur, &def, "Batch script", &["bat", "cmd"]) {
                {
                    let mut g = gui();
                    g.cfg.zapret_bat_path = p.clone();
                    g.status = format!("Выбран файл: {p}");
                }
                persist_gui();
                refresh_ui();
            }
        }
        ID_BTN_TGWS if code == BN_CLICKED => {
            let (cur, def) = {
                let g = gui();
                (g.cfg.tgws_file_path.clone(), g.cfg.default_dir.clone())
            };
            if let Some(p) = pick_file_dialog(&cur, &def, "Executable / Script", &["exe", "bat", "cmd"]) {
                {
                    let mut g = gui();
                    g.cfg.tgws_file_path = p.clone();
                    g.status = format!("Выбран файл: {p}");
                }
                persist_gui();
                refresh_ui();
            }
        }
        ID_BTN_START if code == BN_CLICKED => {
            sync_from_controls();
            set_status("Запускаю...");
            refresh_ui();
            thread::spawn(do_start);
        }
        ID_BTN_STOP if code == BN_CLICKED => {
            sync_from_controls();
            set_status("Останавливаю...");
            refresh_ui();
            thread::spawn(do_stop);
        }
        _ => {}
    }
}

unsafe fn fill_rect(hdc: HDC, rc: &RECT, color: u32) {
    let b = CreateSolidBrush(color);
    FillRect(hdc, rc, b);
    DeleteObject(b);
}

unsafe fn frame_rect(hdc: HDC, rc: &RECT, color: u32) {
    let b = CreateSolidBrush(color);
    FrameRect(hdc, rc, b);
    DeleteObject(b);
}

/// Отрисовка кнопок и чекбоксов (owner-draw) в цветах текущей темы.
unsafe fn draw_item(lparam: LPARAM) {
    let Some(t) = THEME.get() else {
        return;
    };
    let di = &*(lparam as *const DRAWITEMSTRUCT);
    let hdc = di.hDC;
    let rc = di.rcItem;
    let id = di.CtlID as i32;
    let pressed = di.itemState & 0x1 != 0;
    let disabled = di.itemState & 0x4 != 0;
    let focused = di.itemState & 0x10 != 0;
    let text = get_text(di.hwndItem as isize);

    let font = SendMessageW(di.hwndItem, WM_GETFONT, 0, 0);
    let old_font = SelectObject(hdc, font as *mut c_void);
    SetBkMode(hdc, 1); // TRANSPARENT
    SetTextColor(hdc, if disabled { t.disabled } else { t.text });

    let is_check = matches!(id, ID_CHK_ZAPRET | ID_CHK_TGWS | ID_CHK_AUTOSTART | ID_CHK_AUTOUPD);

    if is_check {
        fill_rect(hdc, &rc, t.bg);

        let size = sc(14);
        let left = rc.left + sc(1);
        let top = rc.top + (rc.bottom - rc.top - size) / 2;
        let box_rc = RECT {
            left,
            top,
            right: left + size,
            bottom: top + size,
        };

        let checked = check_state(id);
        if checked {
            let c = if disabled { t.disabled } else { t.accent };
            fill_rect(hdc, &box_rc, c);
            frame_rect(hdc, &box_rc, c);

            let pen = CreatePen(0, sc(2).max(2), rgb(255, 255, 255));
            let old_pen = SelectObject(hdc, pen);
            MoveToEx(hdc, left + sc(3), top + sc(7), null_mut());
            LineTo(hdc, left + sc(6), top + sc(10));
            LineTo(hdc, left + sc(11), top + sc(4));
            SelectObject(hdc, old_pen);
            DeleteObject(pen);
        } else {
            fill_rect(hdc, &box_rc, t.box_bg);
            frame_rect(hdc, &box_rc, t.box_border);
        }

        let mut text_rc = RECT {
            left: box_rc.right + sc(8),
            top: rc.top,
            right: rc.right,
            bottom: rc.bottom,
        };
        DrawTextW(hdc, wide(&text).as_ptr(), -1, &mut text_rc, 0x4 | 0x20); // VCENTER | SINGLELINE

        if focused {
            let f = RECT {
                left: text_rc.left - sc(2),
                top: rc.top + sc(1),
                right: rc.right,
                bottom: rc.bottom - sc(1),
            };
            DrawFocusRect(hdc, &f);
        }
    } else {
        let bg = if pressed { t.btn_pressed } else { t.btn_bg };
        fill_rect(hdc, &rc, bg);
        frame_rect(hdc, &rc, t.btn_border);

        let mut text_rc = rc;
        DrawTextW(hdc, wide(&text).as_ptr(), -1, &mut text_rc, 0x1 | 0x4 | 0x20); // CENTER | VCENTER | SINGLELINE

        if focused {
            let f = RECT {
                left: rc.left + sc(3),
                top: rc.top + sc(3),
                right: rc.right - sc(3),
                bottom: rc.bottom - sc(3),
            };
            DrawFocusRect(hdc, &f);
        }
    }

    SelectObject(hdc, old_font);
}

unsafe extern "system" fn gui_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            GUI_HWND.store(hwnd as isize, Ordering::Relaxed);
            create_controls(hwnd);
            refresh_ui();
            0
        }
        WM_ERASEBKGND => {
            if let Some(t) = THEME.get() {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                FillRect(wparam as HDC, &rc, t.bg_brush as HBRUSH);
                1
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_CTLCOLORSTATIC => {
            if let Some(t) = THEME.get() {
                let is_link = UI.get().map(|u| u.link == lparam as isize).unwrap_or(false);
                SetTextColor(wparam as HDC, if is_link { t.link } else { t.text });
                SetBkColor(wparam as HDC, t.bg);
                t.bg_brush as LRESULT
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_CTLCOLOREDIT => {
            if let Some(t) = THEME.get() {
                SetTextColor(wparam as HDC, t.text);
                SetBkColor(wparam as HDC, if t.dark { rgb(45, 45, 45) } else { rgb(255, 255, 255) });
                t.edit_brush as LRESULT
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_DRAWITEM => {
            draw_item(lparam);
            1
        }
        WM_SETCURSOR => {
            let over_link = UI.get().map(|u| u.link == wparam as isize).unwrap_or(false);
            if over_link && (lparam & 0xFFFF) == 1 {
                SetCursor(LoadCursorW(null_mut(), 32649usize as *const u16)); // IDC_HAND
                1
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_COMMAND => {
            on_command(wparam);
            0
        }
        WM_CLOSE => {
            sync_from_controls();
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            if let Some(ui) = UI.get() {
                DeleteObject(ui.font as *mut c_void);
                DeleteObject(ui.font_link as *mut c_void);
            }
            if let Some(t) = THEME.get() {
                DeleteObject(t.bg_brush as *mut c_void);
                DeleteObject(t.edit_brush as *mut c_void);
            }
            UI.set(None);
            THEME.set(None);
            GUI_HWND.store(0, Ordering::Relaxed);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Создаёт окно (или выводит уже открытое на передний план). Вызывать только из главного потока.
unsafe fn open_gui_window() {
    let existing = GUI_HWND.load(Ordering::Relaxed);
    if existing != 0 {
        ShowWindow(existing as HWND, 9); // SW_RESTORE
        SetForegroundWindow(existing as HWND);
        return;
    }

    let dark = system_uses_dark();
    THEME.set(Some(make_theme(dark)));

    let hinst = GetModuleHandleW(null());
    let class = wide(GUI_CLASS);
    let icon = LoadIconW(hinst, 1usize as *const u16);

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: 0x0003, // CS_HREDRAW | CS_VREDRAW
        lpfnWndProc: Some(gui_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: icon,
        hCursor: LoadCursorW(null_mut(), 32512usize as *const u16), // IDC_ARROW
        hbrBackground: null_mut(), // фон рисуем сами (WM_ERASEBKGND) в цвете темы
        lpszMenuName: null(),
        lpszClassName: class.as_ptr(),
        hIconSm: icon,
    };
    RegisterClassExW(&wc); // повторная регистрация вернёт 0 — это нормально

    let mut rc = RECT {
        left: 0,
        top: 0,
        right: sc(CLIENT_W),
        bottom: sc(CLIENT_H),
    };
    AdjustWindowRectEx(&mut rc, MAIN_STYLE, 0, 0);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;
    let x = ((GetSystemMetrics(0) - w) / 2).max(0);
    let y = ((GetSystemMetrics(1) - h) / 2).max(0);

    let title = wide("TGWS + Zapret Launcher");
    let hwnd = CreateWindowExW(
        0,
        class.as_ptr(),
        title.as_ptr(),
        MAIN_STYLE,
        x,
        y,
        w,
        h,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if hwnd.is_null() {
        if let Some(t) = THEME.get() {
            DeleteObject(t.bg_brush as *mut c_void);
            DeleteObject(t.edit_brush as *mut c_void);
        }
        THEME.set(None);
        return;
    }

    // Тёмный заголовок окна (20 — Win10 20H1+/Win11, 19 — более ранние сборки Win10)
    let flag: i32 = if dark { 1 } else { 0 };
    DwmSetWindowAttribute(hwnd, 20, &flag as *const i32 as *const c_void, 4);
    DwmSetWindowAttribute(hwnd, 19, &flag as *const i32 as *const c_void, 4);

    ShowWindow(hwnd, 5); // SW_SHOW
    UpdateWindow(hwnd);
    SetForegroundWindow(hwnd);
}

/// Невидимое окно-контроллер: принимает команды из потоков и от второго запуска программы.
unsafe extern "system" fn ctl_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_OPEN => {
            open_gui_window();
            0
        }
        WM_APP_REFRESH => {
            refresh_ui();
            0
        }
        WM_APP_QUIT => {
            let g = GUI_HWND.load(Ordering::Relaxed);
            if g != 0 {
                sync_from_controls();
                DestroyWindow(g as HWND);
            }
            persist_gui();
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn create_controller_window() -> HWND {
    let hinst = GetModuleHandleW(null());
    let class = wide(CTL_CLASS);
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(ctl_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: class.as_ptr(),
        hIconSm: null_mut(),
    };
    RegisterClassExW(&wc);
    // HWND_MESSAGE (-3): окно без интерфейса, только для сообщений
    CreateWindowExW(
        0,
        class.as_ptr(),
        null(),
        0,
        0,
        0,
        0,
        0,
        (-3isize) as HWND,
        null_mut(),
        hinst,
        null(),
    )
}

/// Второй запуск программы просит уже работающий экземпляр открыть окно.
fn signal_existing_instance() {
    unsafe {
        let class = wide(CTL_CLASS);
        let h = FindWindowExW((-3isize) as HWND, null_mut(), class.as_ptr(), null());
        if !h.is_null() {
            PostMessageW(h, WM_APP_OPEN, 0, 0);
        }
    }
}

fn run_message_loop() {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            let g = GUI_HWND.load(Ordering::Relaxed);
            if g != 0 && IsDialogMessageW(g as HWND, &msg) != 0 {
                continue; // Tab / стрелки / пробел для элементов окна
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ───────────────────────── запуск приложения ─────────────────────────

fn run_app(autostart_launch: bool) {
    init_dpi();

    if !acquire_single_instance(APP_MUTEX) {
        // Уже работает в трее. При ручном запуске просто открываем его окно.
        if !autostart_launch {
            signal_existing_instance();
        }
        return;
    }

    unsafe {
        let ctl = create_controller_window();
        MAIN_HWND.store(ctl as isize, Ordering::Relaxed);
    }

    // Галочка автозапуска должна отражать реальное состояние задачи; заодно обновляем путь в задаче
    thread::spawn(move || {
        let real = autostart_task_exists();
        let changed = {
            let mut g = gui();
            if g.cfg.autostart != real {
                g.cfg.autostart = real;
                true
            } else {
                false
            }
        };
        if changed {
            persist_gui();
        }
        if real && !autostart_launch {
            let _ = set_autostart(true);
        }
        post_refresh();
    });

    // Фоновое автообновление: через 25 с после старта, затем раз в 6 часов
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(25));
        loop {
            run_updates(
                &[Component::Zapret, Component::Tgws],
                UpdateMode {
                    install_unmanaged: false,
                    respect_flags: true,
                    verbose: false,
                },
            );
            thread::sleep(UPDATE_CHECK_INTERVAL);
        }
    });

    let tray_menu = Menu::new();
    let item_show = MenuItem::new("Открыть окно", true, None);
    let item_start = MenuItem::new("Запустить выбранное", true, None);
    let item_stop = MenuItem::new("Остановить всё", true, None);
    let item_update = MenuItem::new("Проверить обновления", true, None);
    let item_exit = MenuItem::new("Выход", true, None);

    let id_show = item_show.id().clone();
    let id_start = item_start.id().clone();
    let id_stop = item_stop.id().clone();
    let id_update = item_update.id().clone();
    let id_exit = item_exit.id().clone();

    let _ = tray_menu.append_items(&[
        &item_show,
        &item_start,
        &item_stop,
        &item_update,
        &PredefinedMenuItem::separator(),
        &item_exit,
    ]);

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == id_show {
            post_to_main(WM_APP_OPEN);
        } else if event.id == id_start {
            thread::spawn(do_start);
        } else if event.id == id_stop {
            thread::spawn(do_stop);
        } else if event.id == id_update {
            thread::spawn(|| {
                run_updates(
                    &[Component::Zapret, Component::Tgws],
                    UpdateMode {
                        install_unmanaged: false,
                        respect_flags: false,
                        verbose: true,
                    },
                );
            });
        } else if event.id == id_exit {
            post_to_main(WM_APP_QUIT);
        }
    }));

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if matches!(event, TrayIconEvent::DoubleClick { .. }) {
            post_to_main(WM_APP_OPEN);
        }
    }));

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_menu_on_left_click(false)
        .with_tooltip("TGWS + Zapret Launcher")
        .with_icon(build_tray_icon_image())
        .build()
        .ok();

    // Ручной запуск — сразу показываем окно. Автозапуск — тихо в трей.
    if !autostart_launch {
        unsafe { open_gui_window() };
    }

    run_message_loop();
}

fn main() {
    let autostart_launch = env::args().skip(1).any(|a| a == "--autostart");
    run_app(autostart_launch);
}
