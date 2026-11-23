#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const ZAPRET_DOWNLOAD_URL: &str =
    "https://github.com/Flowseal/zapret-discord-youtube/releases/download/1.10.4/zapret-discord-youtube-1.10.4.zip";
const TGWS_DOWNLOAD_URL: &str =
    "https://github.com/Flowseal/tg-ws-proxy/releases/download/v1.11.4/TgWsProxy_windows.exe";
const TELEGRAM_CHANNEL_URL: &str = "https://t.me/fan4ez231";
const CONFIG_FILE_NAME: &str = "tgwszapret_config.json";
const TASK_NAME: &str = "TGWSZapretLauncher";
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn hide_console_window(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

fn exe_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn set_autostart(enabled: bool) -> Result<(), String> {
    if enabled {
        let exe_path = env::current_exe().map_err(|e| format!("путь exe: {e}"))?;
        let exe_str = exe_path.display().to_string().replace('\'', "''");
        let work_dir_str = exe_dir().display().to_string().replace('\'', "''");

        let ps_script = format!(
            "$action = New-ScheduledTaskAction -Execute '{exe_str}' -WorkingDirectory '{work_dir_str}'; \
             $trigger = New-ScheduledTaskTrigger -AtLogOn -User \"$env:USERDOMAIN\\$env:USERNAME\"; \
             $trigger.Delay = 'PT5S'; \
             $principal = New-ScheduledTaskPrincipal -UserId \"$env:USERDOMAIN\\$env:USERNAME\" -LogonType Interactive -RunLevel Highest; \
             $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit 0; \
             Register-ScheduledTask -TaskName '{TASK_NAME}' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null"
        );

        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &ps_script,
        ]);
        hide_console_window(&mut cmd);
        let out = cmd.output().map_err(|e| format!("powershell: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("ошибка автозапуска: {}", err.trim()))
        }
    } else {
        let ps_script = format!(
            "Unregister-ScheduledTask -TaskName '{TASK_NAME}' -Confirm:$false -ErrorAction SilentlyContinue"
        );
        let mut cmd = Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &ps_script,
        ]);
        hide_console_window(&mut cmd);
        let _ = cmd.output();
        Ok(())
    }
}

fn taskkill_by_name(exe_name: &str) -> bool {
    if exe_name.trim().is_empty() {
        return false;
    }
    let mut cmd = Command::new("taskkill");
    cmd.args(["/IM", exe_name, "/F", "/T"]);
    hide_console_window(&mut cmd);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn stop_windivert_service(service_name: &str) {
    let mut cmd = Command::new("sc");
    cmd.args(["stop", service_name]);
    hide_console_window(&mut cmd);
    let _ = cmd.output();
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

struct CoreState {
    cfg: SavedConfig,
    tgws_child: Option<Child>,
    status_log: String,
    is_downloading: bool,
}

impl CoreState {
    fn new() -> Self {
        Self {
            cfg: load_config(),
            tgws_child: None,
            status_log: "Готов к запуску.".to_string(),
            is_downloading: false,
        }
    }

    fn persist(&self) {
        save_config(&self.cfg);
    }

    fn work_dir_for(&self, target_path: &str) -> PathBuf {
        Path::new(target_path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&self.cfg.default_dir))
    }

    fn start_selected(&mut self) {
        self.persist();

        if !self.cfg.use_zapret && !self.cfg.use_tgws {
            self.status_log = "Ничего не выбрано: отметь Zapret и/или tgwsproxy.".to_string();
            return;
        }

        if self.cfg.use_zapret {
            if self.cfg.zapret_bat_path.trim().is_empty() {
                self.status_log = "Укажи путь к .bat файлу zapret.".to_string();
                return;
            }
            if !Path::new(&self.cfg.zapret_bat_path).is_file() {
                self.status_log = format!("Не найден батник zapret: {}", self.cfg.zapret_bat_path);
                return;
            }
        }

        if self.cfg.use_tgws {
            if self.cfg.tgws_file_path.trim().is_empty() {
                self.status_log = "Укажи путь к файлу tgwsproxy.".to_string();
                return;
            }
            if !Path::new(&self.cfg.tgws_file_path).is_file() {
                self.status_log = format!("Не найден файл tgwsproxy: {}", self.cfg.tgws_file_path);
                return;
            }
        }

        let mut started_parts: Vec<&str> = Vec::new();

        if self.cfg.use_zapret {
            let work_dir = self.work_dir_for(&self.cfg.zapret_bat_path);
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", &self.cfg.zapret_bat_path])
                .current_dir(work_dir);
            hide_console_window(&mut cmd);

            match cmd.spawn() {
                Ok(_) => started_parts.push("zapret"),
                Err(e) => {
                    self.status_log = format!("Ошибка запуска zapret: {e}");
                    return;
                }
            }
        }

        if self.cfg.use_tgws {
            let work_dir = self.work_dir_for(&self.cfg.tgws_file_path);
            let mut cmd = Command::new(&self.cfg.tgws_file_path);
            cmd.current_dir(work_dir);
            hide_console_window(&mut cmd);

            match cmd.spawn() {
                Ok(child) => {
                    self.tgws_child = Some(child);
                    started_parts.push("tgwsproxy");
                }
                Err(e) => {
                    self.status_log = format!("Ошибка запуска tgwsproxy: {e}");
                    return;
                }
            }
        }

        self.status_log = format!("Запущено: {}", started_parts.join(" + "));
    }

    fn stop_all(&mut self) {
        if let Some(mut child) = self.tgws_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        let tgws_exe_name = Path::new(&self.cfg.tgws_file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("TgWsProxy_windows.exe");

        taskkill_by_name(tgws_exe_name);
        taskkill_by_name("TgWsProxy_windows.exe");

        let killed_winws = taskkill_by_name("winws.exe");
        let killed_winws64 = taskkill_by_name("winws64.exe");
        stop_windivert_service("windivert");
        stop_windivert_service("windivert14");

        if killed_winws || killed_winws64 {
            self.status_log = "Остановка выполнена (zapret и tgwsproxy остановлены).".to_string();
        } else {
            self.status_log = "Остановка выполнена.".to_string();
        }
    }
}

enum BgEvent {
    Status(String),
    ZapretReady(String),
    TgwsReady(String),
    DownloadFailed(String),
}

struct LauncherApp {
    core: Arc<Mutex<CoreState>>,
    tx: Sender<BgEvent>,
    rx: Receiver<BgEvent>,
    _tray_icon: Option<TrayIcon>,
    allow_exit: bool,
}

impl LauncherApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let core = Arc::new(Mutex::new(CoreState::new()));
        let (tx, rx) = mpsc::channel();

        let tray_menu = Menu::new();
        let item_show = MenuItem::new("Открыть окно", true, None);
        let item_start = MenuItem::new("Запустить выбранное", true, None);
        let item_stop = MenuItem::new("Остановить всё", true, None);
        let item_exit = MenuItem::new("Выход", true, None);

        let id_show = item_show.id().clone();
        let id_start = item_start.id().clone();
        let id_stop = item_stop.id().clone();
        let id_exit = item_exit.id().clone();

        let _ = tray_menu.append_items(&[
            &item_show,
            &item_start,
            &item_stop,
            &PredefinedMenuItem::separator(),
            &item_exit,
        ]);

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("TGWS + Zapret Launcher")
            .with_icon(build_tray_icon_image())
            .build()
            .ok();

        let core_clone = Arc::clone(&core);
        let ctx_clone = cc.egui_ctx.clone();
        thread::spawn(move || {
            let menu_rx = MenuEvent::receiver();
            while let Ok(event) = menu_rx.recv() {
                if event.id == id_show {
                    ctx_clone.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx_clone.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx_clone.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx_clone.request_repaint();
                } else if event.id == id_start {
                    if let Ok(mut st) = core_clone.lock() {
                        st.start_selected();
                    }
                    ctx_clone.request_repaint();
                } else if event.id == id_stop {
                    if let Ok(mut st) = core_clone.lock() {
                        st.stop_all();
                    }
                    ctx_clone.request_repaint();
                } else if event.id == id_exit {
                    if let Ok(st) = core_clone.lock() {
                        st.persist();
                    }
                    std::process::exit(0);
                }
            }
        });

        Self {
            core,
            tx,
            rx,
            _tray_icon: tray_icon,
            allow_exit: false,
        }
    }

    fn poll_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            if let Ok(mut st) = self.core.lock() {
                match ev {
                    BgEvent::Status(msg) => st.status_log = msg,
                    BgEvent::ZapretReady(path) => {
                        st.cfg.zapret_bat_path = path;
                        st.is_downloading = false;
                        st.persist();
                    }
                    BgEvent::TgwsReady(path) => {
                        st.cfg.tgws_file_path = path;
                        st.is_downloading = false;
                        st.persist();
                    }
                    BgEvent::DownloadFailed(msg) => {
                        st.status_log = msg;
                        st.is_downloading = false;
                    }
                }
            }
        }
    }

    fn pick_folder(&mut self) {
        let current_dir = self
            .core
            .lock()
            .map(|s| s.cfg.default_dir.clone())
            .unwrap_or_default();

        let mut dialog = rfd::FileDialog::new();
        let start = PathBuf::from(&current_dir);
        if start.exists() {
            dialog = dialog.set_directory(start);
        }
        if let Some(folder) = dialog.pick_folder() {
            if let Ok(mut st) = self.core.lock() {
                st.cfg.default_dir = folder.display().to_string();
                st.status_log = format!("Общая папка: {}", st.cfg.default_dir);
                st.persist();
            }
        }
    }

    fn pick_file_dialog(current_path: &str, fallback_dir: &str, filter_name: &str, extensions: &[&str]) -> Option<String> {
        let mut dialog = rfd::FileDialog::new().add_filter(filter_name, extensions);

        let start_dir: PathBuf = Path::new(current_path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(fallback_dir));

        if start_dir.exists() {
            dialog = dialog.set_directory(start_dir);
        }

        dialog.pick_file().map(|p| p.display().to_string())
    }

    fn start_download_component(&mut self, component: &'static str, url: &'static str) {
        let dest_root = {
            let Ok(mut st) = self.core.lock() else {
                return;
            };
            if st.is_downloading {
                st.status_log = "Загрузка уже выполняется, подожди немного.".to_string();
                return;
            }
            st.is_downloading = true;
            PathBuf::from(&st.cfg.default_dir)
        };

        let tx = self.tx.clone();

        thread::spawn(move || {
            let _ = tx.send(BgEvent::Status(format!(
                "Создаю папку {} и качаю {component}...",
                dest_root.display()
            )));

            if let Err(e) = fs::create_dir_all(&dest_root) {
                let _ = tx.send(BgEvent::DownloadFailed(format!(
                    "Ошибка создания папки: {e}"
                )));
                return;
            }

            let target_subdir = dest_root.join(component);
            if let Err(e) = fs::create_dir_all(&target_subdir) {
                let _ = tx.send(BgEvent::DownloadFailed(format!(
                    "Ошибка создания подпапки: {e}"
                )));
                return;
            }

            match download_component_to_dir(component, url, &target_subdir) {
                Ok(found_path) => {
                    let p = found_path.display().to_string();
                    if component == "zapret" {
                        let _ = tx.send(BgEvent::Status(format!("zapret готов: {p}")));
                        let _ = tx.send(BgEvent::ZapretReady(p));
                    } else {
                        let _ = tx.send(BgEvent::Status(format!("tgwsproxy готов: {p}")));
                        let _ = tx.send(BgEvent::TgwsReady(p));
                    }
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::DownloadFailed(format!(
                        "Не удалось скачать {component}: {e}"
                    )));
                }
            }
        });
    }
}

impl Drop for LauncherApp {
    fn drop(&mut self) {
        if let Ok(st) = self.core.lock() {
            st.persist();
        }
    }
}

fn download_component_to_dir(
    component: &str,
    url: &str,
    out_dir: &Path,
) -> Result<PathBuf, String> {
    let resp = ureq::get(url).call().map_err(|e| format!("сеть: {e}"))?;

    let mut bytes = Vec::new();
    resp.into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("чтение ответа: {e}"))?;

    let is_zip = url.to_lowercase().ends_with(".zip")
        || bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06");

    if is_zip {
        let reader = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(reader).map_err(|e| format!("архив ZIP: {e}"))?;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| format!("элемент ZIP: {e}"))?;
            let Some(enclosed_name) = file.enclosed_name() else {
                continue;
            };
            let out_path = out_dir.join(enclosed_name);

            if file.is_dir() {
                fs::create_dir_all(&out_path).map_err(|e| format!("папка: {e}"))?;
            } else {
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| format!("родительская папка: {e}"))?;
                }
                let mut outfile =
                    File::create(&out_path).map_err(|e| format!("создание файла: {e}"))?;
                io::copy(&mut file, &mut outfile).map_err(|e| format!("запись файла: {e}"))?;
            }
        }

        let target_ext = if component == "zapret" { "bat" } else { "exe" };
        find_first_with_ext(out_dir, target_ext)
            .ok_or_else(|| format!("распаковано в {}, выбери файл вручную", out_dir.display()))
    } else {
        let file_name = url
            .split('/')
            .next_back()
            .filter(|s| !s.is_empty() && !s.contains('?'))
            .map(|s| {
                if s.to_lowercase().ends_with(".exe") {
                    s.to_string()
                } else {
                    format!("{s}.exe")
                }
            })
            .unwrap_or_else(|| "TgWsProxy_windows.exe".to_string());

        fs::create_dir_all(out_dir).map_err(|e| format!("папка: {e}"))?;
        let exe_path = out_dir.join(file_name);
        fs::write(&exe_path, &bytes).map_err(|e| format!("сохранение .exe: {e}"))?;
        Ok(exe_path)
    }
}

fn find_first_with_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(e) = path.extension().and_then(|s| s.to_str()) {
                if e.eq_ignore_ascii_case(ext) {
                    return Some(path);
                }
            }
        } else if path.is_dir() {
            if let Some(found) = find_first_with_ext(&path, ext) {
                return Some(found);
            }
        }
    }
    None
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();

        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            if let Ok(mut st) = self.core.lock() {
                st.status_log = "Свёрнуто в трей.".to_string();
            }
            return;
        }

        let is_downloading = self.core.lock().map(|s| s.is_downloading).unwrap_or(false);
        if is_downloading {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }

        let mut pick_folder_clicked = false;
        let mut download_zapret_clicked = false;
        let mut download_tgws_clicked = false;
        let mut pick_bat_clicked = false;
        let mut pick_tgws_clicked = false;

        egui::CentralPanel::default().show(ctx, |ui| {
            let Ok(mut st) = self.core.lock() else {
                return;
            };

            ui.horizontal(|ui| {
                ui.heading("TGWS + Zapret Launcher");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.hyperlink_to("Telegram: @fan4ez231", TELEGRAM_CHANNEL_URL);
                });
            });
            ui.add_space(6.0);

            ui.label("Общая папка (куда качать и складывать):");
            ui.horizontal(|ui| {
                if ui.text_edit_singleline(&mut st.cfg.default_dir).lost_focus() {
                    st.persist();
                }
                if ui.button("Папка...").clicked() {
                    pick_folder_clicked = true;
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!st.is_downloading, egui::Button::new("Скачать zapret"))
                    .clicked()
                {
                    download_zapret_clicked = true;
                }
                if ui
                    .add_enabled(!st.is_downloading, egui::Button::new("Скачать tgwsproxy"))
                    .clicked()
                {
                    download_tgws_clicked = true;
                }
            });

            ui.add_space(8.0);
            ui.label("Что запустить:");
            ui.horizontal(|ui| {
                if ui.checkbox(&mut st.cfg.use_zapret, "Zapret (.bat)").changed() {
                    st.persist();
                }
                if ui.checkbox(&mut st.cfg.use_tgws, "tgwsproxy").changed() {
                    st.persist();
                }
            });

            ui.add_space(4.0);
            if ui
                .checkbox(&mut st.cfg.autostart, "Автозапуск с Windows")
                .changed()
            {
                match set_autostart(st.cfg.autostart) {
                    Ok(()) => {
                        st.status_log = if st.cfg.autostart {
                            "Автозапуск включён.".to_string()
                        } else {
                            "Автозапуск выключен.".to_string()
                        };
                        st.persist();
                    }
                    Err(e) => {
                        st.cfg.autostart = !st.cfg.autostart;
                        st.status_log = e;
                    }
                }
            }

            ui.add_space(6.0);
            ui.label("Путь к .bat файлу zapret:");
            ui.horizontal(|ui| {
                if ui
                    .text_edit_singleline(&mut st.cfg.zapret_bat_path)
                    .lost_focus()
                {
                    st.persist();
                }
                if ui.button("Выбрать .bat...").clicked() {
                    pick_bat_clicked = true;
                }
            });

            ui.add_space(6.0);
            ui.label("Путь к файлу tgwsproxy:");
            ui.horizontal(|ui| {
                if ui
                    .text_edit_singleline(&mut st.cfg.tgws_file_path)
                    .lost_focus()
                {
                    st.persist();
                }
                if ui.button("Выбрать файл...").clicked() {
                    pick_tgws_clicked = true;
                }
            });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Запустить выбранное").clicked() {
                    st.start_selected();
                }
                if ui.button("Остановить всё").clicked() {
                    st.stop_all();
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.label(format!("Статус: {}", st.status_log));
        });

        if pick_folder_clicked {
            self.pick_folder();
        }
        if download_zapret_clicked {
            self.start_download_component("zapret", ZAPRET_DOWNLOAD_URL);
        }
        if download_tgws_clicked {
            self.start_download_component("tgwsproxy", TGWS_DOWNLOAD_URL);
        }
        if pick_bat_clicked {
            let (cur, def) = self
                .core
                .lock()
                .map(|s| (s.cfg.zapret_bat_path.clone(), s.cfg.default_dir.clone()))
                .unwrap_or_default();
            if let Some(p) = Self::pick_file_dialog(&cur, &def, "Batch script", &["bat", "cmd"]) {
                if let Ok(mut st) = self.core.lock() {
                    st.cfg.zapret_bat_path = p.clone();
                    st.status_log = format!("Выбран файл: {p}");
                    st.persist();
                }
            }
        }
        if pick_tgws_clicked {
            let (cur, def) = self
                .core
                .lock()
                .map(|s| (s.cfg.tgws_file_path.clone(), s.cfg.default_dir.clone()))
                .unwrap_or_default();
            if let Some(p) =
                Self::pick_file_dialog(&cur, &def, "Executable / Script", &["exe", "bat", "cmd"])
            {
                if let Ok(mut st) = self.core.lock() {
                    st.cfg.tgws_file_path = p.clone();
                    st.status_log = format!("Выбран файл: {p}");
                    st.persist();
                }
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([540.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        "TGWS + Zapret",
        options,
        Box::new(|cc| Ok(Box::new(LauncherApp::new(cc)))),
    )
}