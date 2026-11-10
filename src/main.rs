#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
use eframe::egui;
use rsfbclient::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "maedandori/mics-data";
const API: &str = "https://api.github.com/repos/maedandori/mics-data/releases/latest";
const EXE: &str = "mics-data.exe";
const HASH_FILE: &str = "mics-data.exe.sha256";

#[derive(Clone, Default, Serialize, Deserialize)]
struct Settings {
    host: String,
    port: String,
    database: String,
    user: String,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("MICS-Data").join("settings.json"))
    }
    fn load() -> Self {
        Self::path().and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| Self {
                port: "3050".into(),
                user: "SYSDBA".into(),
                ..Self::default()
            })
    }
    fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("Unable to locate configuration directory")?;
        fs::create_dir_all(path.parent().ok_or("Invalid configuration path")?)
            .map_err(|e| e.to_string())?;
        fs::write(path, serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

enum Event {
    Db(Result<Vec<String>, String>),
    UpdateStatus(String),
    UpdateReady { version: String, staged: PathBuf },
}

struct MicsApp {
    settings: Settings,
    password: String,
    output: Vec<String>,
    selected: String,
    busy: bool,
    updating: bool,
    status: String,
    update_status: String,
    last_check: Instant,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl MicsApp {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            settings: Settings::load(),
            password: String::new(),
            output: vec![],
            selected: String::new(),
            busy: false,
            updating: false,
            status: "Chưa kết nối / Not connected".into(),
            update_status: format!("Installed v{VERSION}"),
            last_check: Instant::now(),
            tx,
            rx,
        };
        app.check_updates();
        app
    }

    fn start_db(&mut self, operation: DbOperation) {
        if self.busy || self.updating { return; }
        if self.settings.host.trim().is_empty()
            || self.settings.database.trim().is_empty()
            || self.settings.user.trim().is_empty()
            || self.password.is_empty()
        {
            self.status = "Nhập host, đường dẫn database, user và password trước.".into();
            return;
        }
        if let Err(e) = self.settings.save() {
            self.status = format!("Cannot save connection settings: {e}");
            return;
        }
        let cfg = self.settings.clone();
        let pass = self.password.clone();
        self.busy = true;
        self.status = "Đang kết nối Firebird...".into();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = query_db(&cfg, &pass, operation);
            let _ = tx.send(Event::Db(result));
        });
    }

    fn check_updates(&mut self) {
        if self.updating { return; }
        self.updating = true;
        self.last_check = Instant::now();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let send_status = |s: String| { let _ = tx.send(Event::UpdateStatus(s)); };
            match download_if_newer(&send_status) {
                Ok(Some((version, staged))) => {
                    let _ = tx.send(Event::UpdateReady { version, staged });
                }
                Ok(None) => send_status(format!("v{VERSION} - mới nhất / up to date")),
                Err(e) => send_status(format!("Không thể kiểm tra cập nhật: {e}")),
            }
        });
    }

    fn events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Db(result) => {
                    self.busy = false;
                    match result {
                        Ok(lines) => {
                            self.status = "Kết nối thành công - READ ONLY".into();
                            self.output = lines;
                        }
                        Err(e) => self.status = format!("Lỗi Firebird: {e}"),
                    }
                }
                Event::UpdateStatus(s) => {
                    self.updating = false;
                    self.update_status = s;
                }
                Event::UpdateReady { version, staged } => {
                    self.update_status = format!("Đã tải v{version}, tự cài đặt...");
                    match install_update(&staged) {
                        Ok(()) => {
                            // The helper now owns the staged EXE. Exit the GUI
                            // so Windows can allow replacing this running EXE.
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Err(e) => {
                            self.updating = false;
                            self.update_status = format!("Update install failed: {e}");
                        }
                    }
                }
            }
        }
        // One check per 6 hours; only when no database task is active.
        if !self.busy && !self.updating && self.last_check.elapsed() >= Duration::from_secs(21600) {
            self.check_updates();
        }
    }
}

#[derive(Clone)]
enum DbOperation { Test, Tables, Columns(String), Count(String) }

fn query_db(cfg: &Settings, password: &str, op: DbOperation) -> Result<Vec<String>, String> {
    let port: u16 = cfg.port.parse().map_err(|_| "Invalid TCP port".to_string())?;
    let mut builder = rsfbclient::builder_pure_rust();
    builder.host(cfg.host.trim())
        .port(port)
        .db_name(cfg.database.trim())
        .user(cfg.user.trim())
        .pass(password);
    builder.with_transaction(|tr| tr.read_only().no_wait());
    let mut conn = builder.connect().map_err(|e| e.to_string())?;
    match op {
        DbOperation::Test => {
            let rows: Vec<(i64,)> = conn.query("SELECT COUNT(*) FROM RDB$DATABASE", ())
                .map_err(|e| e.to_string())?;
            Ok(vec![format!("Connection OK - {} system row(s)", rows[0].0)])
        }
        DbOperation::Tables => {
            let rows: Vec<(String,)> = conn.query(
                "SELECT TRIM(RDB$RELATION_NAME) FROM RDB$RELATIONS \
                 WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0 AND RDB$VIEW_BLR IS NULL \
                 ORDER BY RDB$RELATION_NAME", ()
            ).map_err(|e| e.to_string())?;
            let mut out = vec![format!("{} tables:", rows.len())];
            out.extend(rows.into_iter().map(|(name,)| name));
            Ok(out)
        }
        DbOperation::Columns(table) => {
            let rows: Vec<(String, i64)> = conn.query(
                "SELECT TRIM(RDB$FIELD_NAME), CAST(RDB$FIELD_POSITION AS BIGINT) \
                 FROM RDB$RELATION_FIELDS WHERE TRIM(RDB$RELATION_NAME) = ? \
                 ORDER BY RDB$FIELD_POSITION", (table.as_str(),)
            ).map_err(|e| e.to_string())?;
            if rows.is_empty() { return Err("Table not found".into()); }
            Ok(rows.into_iter().map(|(name, position)| format!("{}: {name}", position+1)).collect())
        }
        DbOperation::Count(table) => {
            // Validated unquoted identifiers only; never build raw SQL from
            // unvalidated user input.
            if table.is_empty() || table.len() > 63
                || !table.starts_with(|c: char| c.is_ascii_alphabetic())
                || !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            { return Err("Invalid table identifier".into()); }
            let exists: Vec<(i64,)> = conn.query(
                "SELECT COUNT(*) FROM RDB$RELATIONS \
                 WHERE TRIM(RDB$RELATION_NAME) = ? \
                 AND COALESCE(RDB$SYSTEM_FLAG, 0) = 0 AND RDB$VIEW_BLR IS NULL",
                (table.as_str(),)
            ).map_err(|e| e.to_string())?;
            if exists[0].0 != 1 { return Err("Unknown table".into()); }
            let rows: Vec<(i64,)> = conn.query(
                &format!("SELECT COUNT(*) FROM {table}"), ()
            ).map_err(|e| e.to_string())?;
            Ok(vec![format!("{table}: {} rows", rows[0].0)])
        }
    }
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(45))
        .build()
        .get(url)
        .set("User-Agent", "mics-data-rust")
        .set("Accept", "application/vnd.github+json")
        .call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    response.into_reader().take(120 * 1024 * 1024)
        .read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn download_if_newer(status: &dyn Fn(String)) -> Result<Option<(String, PathBuf)>, String> {
    status("Checking GitHub Releases...".into());
    let release: Release = serde_json::from_slice(&http_get(API)?)
        .map_err(|e| format!("Release JSON: {e}"))?;
    let latest = release.tag_name.trim_start_matches('v');
    let current = semver::Version::parse(VERSION).map_err(|e| e.to_string())?;
    let incoming = semver::Version::parse(latest).map_err(|e| e.to_string())?;
    if incoming <= current { return Ok(None); }
    let asset = release.assets.iter().find(|a| a.name == EXE)
        .ok_or("Release missing mics-data.exe")?;
    let checksum = release.assets.iter().find(|a| a.name == HASH_FILE)
        .ok_or("Release missing SHA256 checksum")?;
    status(format!("Downloading update v{latest}..."));
    let bytes = http_get(&asset.browser_download_url)?;
    if bytes.len() < 1024 || bytes.len() >= 120 * 1024 * 1024 {
        return Err("Unexpected update size".into());
    }
    let hash_data = http_get(&checksum.browser_download_url)?;
    let hash_text = String::from_utf8(hash_data).map_err(|e| e.to_string())?;
    let expected = hash_text.split_whitespace().next().ok_or("Empty checksum")?;
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Malformed checksum".into());
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("SHA256 mismatch - update rejected".into());
    }
    let staged = std::env::temp_dir().join(format!("mics-data-v{latest}-{}.exe", std::process::id()));
    let mut file = fs::File::create(&staged).map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(Some((latest.to_string(), staged)))
}

// Self-update uses a copy of *the currently running trusted executable*
// as the updater, avoiding command scripts and third-party executables.
fn install_update(staged: &Path) -> Result<(), String> {
    let original = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper = std::env::temp_dir().join(format!("mics-data-updater-{}.exe", std::process::id()));
    fs::copy(&original, &helper).map_err(|e| e.to_string())?;
    Command::new(&helper).arg("--apply-update")
        .arg(&original).arg(staged)
        .spawn().map_err(|e| e.to_string())?;
    Ok(())
}

fn updater(args: &[String]) -> Result<(), String> {
    if args.len() != 4 { return Err("Incorrect updater arguments".into()); }
    let original = PathBuf::from(&args[2]);
    let staged = PathBuf::from(&args[3]);
    if original.file_name().and_then(|n| n.to_str()) != Some(EXE) {
        return Err("Invalid target executable".into());
    }
    if !staged.is_file() { return Err("Staged update not found".into()); }
    let backup = original.with_extension("exe.old");
    for _ in 0..100 {
        thread::sleep(Duration::from_millis(250));
        let _ = fs::remove_file(&backup);
        if fs::rename(&original, &backup).is_ok() {
            match fs::rename(&staged, &original) {
                Ok(()) => {
                    let _ = Command::new(&original).spawn();
                    let _ = fs::remove_file(&backup);
                    return Ok(());
                }
                Err(e) => {
                    let _ = fs::rename(&backup, &original);
                    return Err(format!("Replacement failed: {e}"));
                }
            }
        }
    }
    Err("Timed out waiting for main process to exit; old version kept".into())
}

impl eframe::App for MicsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.events(ctx);
        ctx.request_repaint_after(Duration::from_millis(250));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("MICS Data - Firebird Connector");
            ui.label(format!("Version v{VERSION} | Windows | Read-only"));
            ui.separator();

            egui::Grid::new("settings").num_columns(2).spacing([10.0, 9.0]).show(ui, |ui| {
                ui.label("Server IP / Host");
                ui.text_edit_singleline(&mut self.settings.host); ui.end_row();
                ui.label("Port");
                ui.text_edit_singleline(&mut self.settings.port); ui.end_row();
                ui.label("Database path (on server)");
                ui.text_edit_singleline(&mut self.settings.database); ui.end_row();
                ui.label("Username");
                ui.text_edit_singleline(&mut self.settings.user); ui.end_row();
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true)); ui.end_row();
            });
            ui.label("Password chỉ giữ trong RAM. Host/port/path/user được lưu trên PC.");
            ui.horizontal(|ui| {
                if ui.button("Lưu cấu hình").clicked() {
                    self.status = match self.settings.save() {
                        Ok(()) => "Đã lưu cấu hình (không lưu mật khẩu).".into(),
                        Err(e) => format!("Save error: {e}"),
                    };
                }
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Kiểm tra kết nối")).clicked() {
                    self.start_db(DbOperation::Test);
                }
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Danh sách bảng")).clicked() {
                    self.start_db(DbOperation::Tables);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Table:");
                ui.text_edit_singleline(&mut self.selected);
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Xem cột")).clicked() {
                    self.start_db(DbOperation::Columns(self.selected.trim().to_string()));
                }
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Đếm dòng")).clicked() {
                    self.start_db(DbOperation::Count(self.selected.trim().to_string()));
                }
            });
            ui.separator();
            ui.label(&self.status);
            ui.horizontal(|ui| {
                ui.label(&self.update_status);
                if ui.add_enabled(!self.updating && !self.busy, egui::Button::new("Kiểm tra cập nhật")).clicked() {
                    self.check_updates();
                }
            });
            if self.busy || self.updating { ui.spinner(); }
            ui.separator();
            ui.heading("Kết quả");
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for item in &self.output { ui.monospace(item); }
            });
        });
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--apply-update") {
        if let Err(e) = updater(&args) {
            let path = std::env::temp_dir().join("mics-data-update-error.txt");
            let _ = fs::write(path, e);
        }
        return;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 620.0])
            .with_min_inner_size([600.0, 490.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "MICS Data", options,
        Box::new(|_cc| Ok(Box::new(MicsApp::new())))
    ) {
        let path = std::env::temp_dir().join("mics-data-startup-error.txt");
        let _ = fs::write(path, e.to_string());
    }
}
