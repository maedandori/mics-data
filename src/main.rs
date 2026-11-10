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
    #[serde(default)]
    sync_url: String,
}

impl Settings {
    // The portable INI always lives in the same folder as the executable.
    // This also means auto-updating the EXE leaves the INI untouched.
    fn path() -> Result<PathBuf, String> {
        std::env::current_exe()
            .map_err(|e| format!("Cannot locate executable: {e}"))?
            .parent()
            .map(|p| p.join("setting.ini"))
            .ok_or_else(|| "Cannot locate executable folder".into())
    }

    fn defaults() -> Self {
        Self { port: "3050".into(), user: "SYSDBA".into(), ..Self::default() }
    }

    fn parse_ini(text: &str) -> Self {
        let mut cfg = Self::defaults();
        let mut in_connection = false;
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') { continue; }
            if line.starts_with('[') && line.ends_with(']') {
                in_connection = line[1..line.len()-1].eq_ignore_ascii_case("firebird");
                continue;
            }
            if !in_connection { continue; }
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim().to_owned();
                match key.trim().to_ascii_lowercase().as_str() {
                    "host" => cfg.host = value,
                    "port" => cfg.port = value,
                    "database" => cfg.database = value,
                    "user" => cfg.user = value,
                    "sync_url" => cfg.sync_url = value,
                    // Password is deliberately not loaded or stored in plain text.
                    _ => {}
                }
            }
        }
        cfg
    }

    fn load() -> Self {
        if let Ok(path) = Self::path() {
            if let Ok(text) = fs::read_to_string(&path) {
                return Self::parse_ini(&text);
            }
        }
        // One-time compatibility with v0.2.1, which used AppData/settings.json.
        // Do not delete the original file until a new INI is saved successfully.
        dirs::config_dir()
            .map(|p| p.join("MICS-Data").join("settings.json"))
            .and_then(|path| fs::read(path).ok())
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_else(Self::defaults)
    }

    fn save(&self) -> Result<(), String> {
        let path = Self::path()?;
        for (key, val) in [
            ("host", &self.host),
            ("port", &self.port),
            ("database", &self.database),
            ("user", &self.user),
            ("sync_url", &self.sync_url),
        ] {
            if val.contains('\r') || val.contains('\n') {
                return Err(format!("Invalid newline in {key}"));
            }
        }
        let content = format!(
            "; MICS Data portable configuration (UTF-8)\n; Password is never written to disk.\n[firebird]\nhost={}\nport={}\ndatabase={}\nuser={}\nsync_url={}\n",
            self.host, self.port, self.database, self.user, self.sync_url,
        );
        // A fresh sidecar file in the EXE folder, not the previous AppData path.
        // Fail visibly when this folder is read-only.
        let temp = path.with_extension("ini.tmp");
        fs::write(&temp, content.as_bytes()).map_err(|e| format!("Cannot write {}: {e}", temp.display()))?;
        if path.exists() {
            // Windows rename does not replace destination atomically.
            fs::remove_file(&path).map_err(|e| format!("Cannot replace INI: {e}"))?;
        }
        fs::rename(&temp, &path).map_err(|e| format!("Cannot finish INI save: {e}"))
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
    Dashboard(Result<Vec<MachineNode>, String>),
    SyncResult { digest: String, result: Result<(), String> },
    UpdateStatus(String),
    UpdateReady { version: String, staged: PathBuf },
}


#[derive(Clone, Default)]
struct MachineNode {
    machine: String,
    order: String,
    status: String,
    product: String,
    tool_no: String,
    tool_name: String,
    good: String,
    bad: String,
    staff: String,
    planned: String,
    cycle: String,
}
impl MachineNode {
    fn state(&self) -> &'static str {
        match self.status.as_str() {
            "200" => "ĐANG CHẠY",
            "420" | "450" => "ĐANG DỪNG",
            _ => "CHƯA XÁC ĐỊNH",
        }
    }
    fn color(&self) -> egui::Color32 {
        match self.status.as_str() {
            "200" => egui::Color32::from_rgb(23, 92, 66),
            "420" | "450" => egui::Color32::from_rgb(132, 43, 46),
            _ => egui::Color32::from_rgb(64, 71, 86),
        }
    }
}
struct MicsApp {
    settings: Settings,
    password: String,
    output: Vec<String>,
    nodes: Vec<MachineNode>,
    node_last_update: Option<Instant>,
    show_setup: bool,
    selected_machine: Option<String>,
    last_synced_digest: String,
    sync_in_flight: bool,
    sync_status: String,
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
            nodes: vec![],
            node_last_update: None,
            show_setup: true,
            selected_machine: None,
            last_synced_digest: String::new(),
            sync_in_flight: false,
            sync_status: "Cloudflare: chưa đồng bộ".into(),
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
        if self.busy { return; }
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


    fn refresh_dashboard(&mut self) {
        if self.busy || self.updating { return; }
        if self.settings.host.trim().is_empty()
            || self.settings.database.trim().is_empty()
            || self.settings.user.trim().is_empty()
            || self.password.is_empty()
        {
            self.status = "Nhập mật khẩu Firebird và nhấn Làm mới để tải dashboard.".into();
            self.show_setup = true;
            return;
        }
        let cfg = self.settings.clone();
        let pass = self.password.clone();
        self.busy = true;
        self.status = "Đang tải trạng thái máy trực tiếp từ Firebird...".into();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Dashboard(query_machine_nodes(&cfg, &pass)));
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
                Event::Dashboard(result) => {
                    self.busy = false;
                    match result {
                        Ok(nodes) => {
                            self.status = format!("Đã tải {} máy trực tiếp từ Firebird.", nodes.len());
                            self.node_last_update = Some(Instant::now());
                            self.nodes = nodes;
                            if !self.sync_in_flight && !self.settings.sync_url.is_empty() {
                                let digest = format!("{:x}", Sha256::digest(
                                    serde_json::to_vec(&self.nodes.iter().map(|n| {
                                        (&n.machine,&n.order,&n.status,&n.product,&n.tool_no,&n.tool_name,
                                         &n.good,&n.bad,&n.planned,&n.cycle,&n.staff)
                                    }).collect::<Vec<_>>()).unwrap_or_default()
                                ));
                                if digest != self.last_synced_digest {
                                    self.sync_in_flight = true;
                                    let cfg = self.settings.clone();
                                    let payload = self.nodes.clone();
                                    let tx = self.tx.clone();
                                    let digest_for_task = digest.clone();
                                    thread::spawn(move || {
                                        let result = send_mics_snapshot(&cfg, &payload);
                                        let _ = tx.send(Event::SyncResult { digest:digest_for_task, result });
                                    });
                                }
                            }
                        }
                        Err(error) => self.status = format!("Lỗi tải dashboard: {error}"),
                    }
                }
                Event::SyncResult {digest,result} => {
                    self.sync_in_flight=false;
                    match result {
                        Ok(()) => { self.last_synced_digest=digest;self.sync_status="Cloudflare D1: đồng bộ thành công".into(); }
                        Err(e) => self.sync_status=format!("Cloudflare: {e}"),
                    }
                }
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
        if !self.busy && !self.updating && !self.nodes.is_empty()
            && self.node_last_update.is_some_and(|t| t.elapsed() >= Duration::from_secs(60))
        {
            self.refresh_dashboard();
        }
        // One check per 6 hours; only when no database task is active.
        if !self.busy && !self.updating && self.last_check.elapsed() >= Duration::from_secs(21600) {
            self.check_updates();
        }
    }
}

#[derive(Clone)]
enum DbOperation { Test, Tables, Columns(String), Count(String), ScheduleNow, ToolColumns, MachineWork, CauseCatalog, CauseReferences }

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
        DbOperation::CauseCatalog => {
            let rows: Vec<(i64, i64, i64, String, i64)> = conn.query(
                "SELECT CAST(ENTRYNO AS BIGINT), CAST(CAUSETYPE AS BIGINT), \
                 CAST(SEQNO AS BIGINT), TRIM(CAUSENAME), CAST(STOPFLG AS BIGINT) \
                 FROM CAUSE ORDER BY CAUSETYPE, SEQNO", ()
            ).map_err(|e| e.to_string())?;
            let mut out = vec![format!("CAUSE: {} nguyên nhân trong danh mục", rows.len()),
                "ENTRYNO | CAUSETYPE | SEQNO | CAUSENAME | STOPFLG".into()];
            out.extend(rows.into_iter().map(|(entry, kind, seq, name, stop)|
                format!("{entry} | {kind} | {seq} | {name} | {stop}")));
            Ok(out)
        }
        DbOperation::CauseReferences => {
            // Search metadata for potential stop-reason event tables.
            // A name match is a *candidate*, not proof of a foreign key.
            let rows: Vec<(String, String)> = conn.query(
                "SELECT TRIM(RF.RDB$RELATION_NAME), TRIM(RF.RDB$FIELD_NAME) \
                 FROM RDB$RELATION_FIELDS RF \
                 JOIN RDB$RELATIONS R ON R.RDB$RELATION_NAME = RF.RDB$RELATION_NAME \
                 WHERE COALESCE(R.RDB$SYSTEM_FLAG, 0) = 0 \
                 AND (UPPER(TRIM(RF.RDB$FIELD_NAME)) LIKE '%CAUSE%' \
                      OR UPPER(TRIM(RF.RDB$FIELD_NAME)) LIKE '%STOP%' \
                      OR UPPER(TRIM(RF.RDB$FIELD_NAME)) LIKE '%DOWNTIME%') \
                 ORDER BY RF.RDB$RELATION_NAME, RF.RDB$FIELD_NAME", ()
            ).map_err(|e| e.to_string())?;
            let mut out = vec![format!("Tìm thấy {} cột ứng viên chứa mã dừng máy", rows.len()),
                "BẢNG | CỘT (chỉ là gợi ý; cần xem dữ liệu để xác minh JOIN)".into()];
            out.extend(rows.into_iter().map(|(table, column)| format!("{table} | {column}")));
            Ok(out)
        }
        DbOperation::MachineWork => {
            // MACHINEWORK is the expected table name based on the export filename.
            // If the database uses a different name the query will report an error.
            // The live production order must match BOTH machine and order number:
            // never infer actual production from MACHINESEQ alone.
            let rows: Vec<(String, String, String, String, String, String, String, String, String)> = conn.query(
                "SELECT TRIM(CAST(W.MACHINENO AS VARCHAR(24))), \
                        COALESCE(TRIM(CAST(W.MORDERNO AS VARCHAR(40))), ''), \
                        COALESCE(TRIM(CAST(W.STATUS AS VARCHAR(16))), ''), \
                        COALESCE(TRIM(CAST(S.PARTSNAME AS VARCHAR(255))), ''), \
                        COALESCE(TRIM(CAST(S.TOOLENTRYNO AS VARCHAR(24))), ''), \
                        COALESCE(TRIM(CAST(T.TOOLNO AS VARCHAR(80))), ''), \
                        COALESCE(TRIM(CAST(T.TOOLNAME AS VARCHAR(255))), ''), \
                        COALESCE(TRIM(CAST(W.TOTALQTYOK AS VARCHAR(24))), ''), \
                        COALESCE(TRIM(CAST(W.STAFFNAME AS VARCHAR(100))), ''), \
                COALESCE(TRIM(CAST(W.QTYPLAN AS VARCHAR(24))), ''), \
                COALESCE(TRIM(CAST(W.REALCYCLETIME AS VARCHAR(24))), '') \
                 FROM MACHINEWORK W \
                 LEFT JOIN SCHEDULE S ON S.MORDERNO = W.MORDERNO \
                    AND S.MACHINENO = W.MACHINENO \
                    AND COALESCE(S.DELETEFLG, 0) = 0 \
                 LEFT JOIN TOOL T ON T.ENTRYNO = S.TOOLENTRYNO \
                 WHERE W.MACHINENO IN (5,6,7,8,10,11,12,13,14,15,16,17,18,19) \
                 ORDER BY W.MACHINENO", ()
            ).map_err(|e| format!("MACHINEWORK query failed (verify actual table name): {e}"))?;
            let mut out = vec![
                format!("MACHINEWORK → SCHEDULE → TOOL: {} dòng", rows.len()),
                "Theo mẫu: 200=đang chạy, 420=dừng bảo trì (máy 5/13), 450=dừng (máy 15 thiếu người). Lý do chưa có cột nguồn xác nhận.".into(),
                "MÁY | TÌNH TRẠNG | TÊN KHUÔN | MÃ KHUÔN | SẢN PHẨM | LỆNH SX | OK | NHÂN VIÊN".into(),
            ];
            out.extend(rows.into_iter().map(|(machine, order, status, product, _entry, tool_no, tool_name, qty_ok, staff)| {
                let state = match status.as_str() {
                    "200" => "ĐANG CHẠY",
                    "420" => "DỪNG (máy 5/13: 保守作業 theo xác nhận)",
                    "450" => "DỪNG (lý do cần xác minh từ DB)",
                    _ => "CHƯA XÁC ĐỊNH",
                };
                let mold = if tool_name.is_empty() { "(chưa ghép được khuôn)" } else { &tool_name };
                format!("{machine} | {state} [{status}] | {mold} | {tool_no} | {product} | {order} | {qty_ok} | {staff}")
            }));
            Ok(out)
        }
        DbOperation::ToolColumns => {
            let rows: Vec<(String, i64)> = conn.query(
                "SELECT TRIM(RDB$FIELD_NAME), CAST(RDB$FIELD_POSITION AS BIGINT) \
                 FROM RDB$RELATION_FIELDS WHERE TRIM(RDB$RELATION_NAME) = 'TOOL' \
                 ORDER BY RDB$FIELD_POSITION", ()
            ).map_err(|e| e.to_string())?;
            if rows.is_empty() { return Err("TOOL has no visible columns or does not exist".into()); }
            let mut out = vec!["TOOL: danh sách cột để xác định khóa nối với SCHEDULE.TOOLENTRYNO".to_owned()];
            out.extend(rows.into_iter().map(|(name, position)| format!("{}: {name}", position + 1)));
            Ok(out)
        }
        DbOperation::ScheduleNow => {
            // Confirmed from exported data:
            // SCHEDULE.TOOLENTRYNO references TOOL.ENTRYNO.
            // MACHINESEQ=1 is the head of the *schedule*, not a live run sensor.
            let rows: Vec<(String, String, String, String, String, String, String)> = conn.query(
                "SELECT TRIM(CAST(S.MACHINENO AS VARCHAR(24))), \
                        COALESCE(TRIM(CAST(S.PARTSNAME AS VARCHAR(255))), ''), \
                        COALESCE(TRIM(CAST(S.TOOLENTRYNO AS VARCHAR(24))), ''), \
                        COALESCE(TRIM(CAST(S.STATUS AS VARCHAR(16))), ''), \
                        COALESCE(TRIM(CAST(T.TOOLNO AS VARCHAR(80))), ''), \
                        COALESCE(TRIM(CAST(T.TOOLNAME AS VARCHAR(255))), ''), \
                        COALESCE(TRIM(CAST(S.MORDERNO AS VARCHAR(40))), '') \
                 FROM SCHEDULE S \
                 LEFT JOIN TOOL T ON T.ENTRYNO = S.TOOLENTRYNO \
                 WHERE S.MACHINESEQ = 1 AND COALESCE(S.DELETEFLG, 0) = 0 \
                 ORDER BY S.MACHINENO", ()
            ).map_err(|e| e.to_string())?;
            let mut out = vec![
                format!("SCHEDULE + TOOL: {} máy có công việc đầu tiên trong lịch", rows.len()),
                "LỊCH DỰ KIẾN: STATUS chưa được xác nhận là trạng thái chạy thực tế.".to_owned(),
                "MÁY | TÊN KHUÔN | MÃ KHUÔN | SẢN PHẨM | STATUS | LỆNH SX | TOOLENTRYNO".to_owned(),
            ];
            out.extend(rows.into_iter().map(|(machine, product, entry, status, tool_no, tool_name, order)| {
                let mold = if tool_name.is_empty() { "(chưa có khuôn trong TOOL)" } else { tool_name.as_str() };
                format!("{machine} | {mold} | {tool_no} | {product} | {status} | {order} | {entry}")
            }));
            Ok(out)
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


fn query_machine_nodes(cfg: &Settings, password: &str) -> Result<Vec<MachineNode>, String> {
    let port: u16 = cfg.port.parse().map_err(|_| "TCP port không hợp lệ".to_string())?;
    let mut builder = rsfbclient::builder_pure_rust();
    builder.host(cfg.host.trim()).port(port)
        .db_name(cfg.database.trim()).user(cfg.user.trim()).pass(password);
    builder.with_transaction(|tr| tr.read_only().no_wait());
    let mut conn = builder.connect().map_err(|e| e.to_string())?;
    // MACHINEWORK is a current-state snapshot: one row per machine.
    // Match production order AND machine ID to avoid mixing scheduled molds.
    // The data does not currently expose which CAUSE entry is active.
    let rows: Vec<(String,String,String,String,String,String,String,String,String,String,String)> = conn.query(
        "SELECT TRIM(CAST(W.MACHINENO AS VARCHAR(24))), \
                COALESCE(TRIM(CAST(W.MORDERNO AS VARCHAR(40))), ''), \
                COALESCE(TRIM(CAST(W.STATUS AS VARCHAR(16))), ''), \
                COALESCE(TRIM(CAST(S.PARTSNAME AS VARCHAR(255))), ''), \
                COALESCE(TRIM(CAST(T.TOOLNO AS VARCHAR(80))), ''), \
                COALESCE(TRIM(CAST(T.TOOLNAME AS VARCHAR(255))), ''), \
                COALESCE(TRIM(CAST(W.TOTALQTYOK AS VARCHAR(24))), ''), \
                COALESCE(TRIM(CAST(W.TOTALQTYNG AS VARCHAR(24))), ''), \
                COALESCE(TRIM(CAST(W.STAFFNAME AS VARCHAR(100))), '') \
         FROM MACHINEWORK W \
         LEFT JOIN SCHEDULE S ON S.MORDERNO = W.MORDERNO \
            AND S.MACHINENO = W.MACHINENO \
            AND COALESCE(S.DELETEFLG, 0) = 0 \
         LEFT JOIN TOOL T ON T.ENTRYNO = S.TOOLENTRYNO \
         WHERE W.MACHINENO IN (5,6,7,8,10,11,12,13,14,15,16,17,18,19) \
         ORDER BY W.MACHINENO", ()
    ).map_err(|e| e.to_string())?;
    // Some schedules may contain multiple records for the same order.
    // Keep only one node per machine and mark conflicting records as unknown.
    let mut nodes: Vec<MachineNode> = Vec::new();
    for (machine, order, status, product, tool_no, tool_name, good, bad, staff, planned, cycle) in rows {
        if let Some(existing) = nodes.iter_mut().find(|n| n.machine == machine) {
            if existing.order != order || existing.tool_name != tool_name {
                existing.status = "AMBIGUOUS".into();
                existing.tool_name = "Nhiều lịch khớp - kiểm tra SCHEDULE".into();
            }
            continue;
        }
        nodes.push(MachineNode { machine, order, status, product, tool_no, tool_name, good, bad, staff, planned, cycle });
    }
    nodes.sort_by_key(|n| n.machine.parse::<u32>().unwrap_or(u32::MAX));
    Ok(nodes)
}


fn send_mics_snapshot(cfg: &Settings, nodes: &[MachineNode]) -> Result<(), String> {
    let token = std::env::var("MICS_SYNC_TOKEN")
        .map_err(|_| "MICS_SYNC_TOKEN chưa cấu hình trên Windows".to_owned())?;
    if token.is_empty() {return Err("Sync token rỗng".into());}
    if !cfg.sync_url.starts_with("https://") {
        return Err("sync_url phải bắt đầu bằng https://".into());
    }
    let parse_qty = |v: &str| -> i64 { v.trim().parse::<i64>().unwrap_or(0).max(0) };
    let machines: Vec<serde_json::Value> = nodes.iter().map(|n| {
        let qty_plan=parse_qty(&n.planned);
        let qty_ok=parse_qty(&n.good);
        serde_json::json!({
            "machine_no":n.machine.parse::<i32>().unwrap_or(0),
            "order_no":n.order,"status_code":n.status,
            "product_name":n.product,"mold_no":n.tool_no,"mold_name":n.tool_name,
            "qty_plan":qty_plan,"qty_ok":qty_ok,"qty_ng":parse_qty(&n.bad),
            "qty_remaining":(qty_plan-qty_ok).max(0),
            "cycle_seconds":n.cycle.trim().parse::<f64>().ok(),
            "staff_name":n.staff
        })
    }).collect();
    let payload=serde_json::json!({
        "source":"mics-data",
        "sampled_at":chrono::Utc::now().to_rfc3339(),
        "machines":machines
    });
    let response=ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).build()
        .post(&cfg.sync_url)
        .set("Authorization",&format!("Bearer {token}"))
        .set("Content-Type","application/json")
        .send_json(payload).map_err(|e| format!("HTTP sync: {e}"))?;
    if response.status()!=200 {return Err(format!("HTTP {}",response.status()));}
    Ok(())
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


// egui's built-in font may not contain Vietnamese combining marks.
// Load installed Windows fonts that cover Vietnamese and Japanese glyphs.
fn install_windows_fonts(ctx: &egui::Context) {
    let font_root = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let mut definitions = egui::FontDefinitions::default();
    let candidates = [
        ("windows-segoe", "segoeui.ttf"),
        ("windows-arial", "arial.ttf"),
        ("windows-meiryo", "meiryo.ttc"),
        ("windows-yugothic", "YuGothR.ttc"),
    ];
    let mut installed = Vec::new();
    for (name, filename) in candidates {
        if let Ok(data) = fs::read(font_root.join(filename)) {
            definitions.font_data.insert(
                name.to_string(),
                egui::FontData::from_owned(data).into(),
            );
            installed.push(name.to_string());
        }
    }
    // Proportional and monospaced UI (including database output) both
    // prefer Windows font fallbacks before egui's limited bundled fonts.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(list) = definitions.families.get_mut(&family) {
            for font in installed.iter().rev() {
                list.insert(0, font.clone());
            }
        }
    }
    ctx.set_fonts(definitions);
}

impl eframe::App for MicsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.events(ctx);
        ctx.request_repaint_after(Duration::from_millis(250));
        egui::CentralPanel::default().show(ctx, |ui| {

            ui.horizontal(|ui| {
                ui.heading("MICS DATA | MACHINE DASHBOARD");
                ui.label(format!("v{VERSION}"));
                if ui.add_enabled(!self.busy, egui::Button::new("Làm mới")).clicked() {
                    self.refresh_dashboard();
                }
                if ui.button(if self.show_setup { "Ẩn cài đặt" } else { "Cài đặt" }).clicked() {
                    self.show_setup = !self.show_setup;
                }
            });
            let running = self.nodes.iter().filter(|n| n.status == "200").count();
            let stopped = self.nodes.iter().filter(|n| n.status == "420" || n.status == "450").count();
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(45, 170, 105), format!("Đang chạy: {running}"));
                ui.colored_label(egui::Color32::from_rgb(218, 80, 85), format!("Đang dừng: {stopped}"));
                ui.label(format!("Tổng: {}", self.nodes.len()));
                if let Some(at) = self.node_last_update {
                    ui.small(format!("Cập nhật {} giây trước | tự làm mới 60 giây", at.elapsed().as_secs()));
                }
            });
            ui.label(&self.status);
            ui.small(&self.sync_status);
            ui.separator();
            egui::ScrollArea::vertical().max_height(if self.show_setup { 220.0 } else { 500.0 }).show(ui, |ui| {
                if self.nodes.is_empty() {
                    ui.label("Chưa có dữ liệu. Nhập thông tin Firebird và chọn Làm mới.");
                } else {
                    let nodes = self.nodes.clone();
                    egui::Grid::new("machine_dashboard_grid").num_columns(3).spacing([9.0, 9.0]).show(ui, |ui| {
                        for (idx, node) in nodes.iter().enumerate() {
                            egui::Frame::group(ui.style()).fill(node.color()).show(ui, |ui| {
                                ui.set_min_width(202.0);
                                ui.set_max_width(220.0);
                                let text = egui::Color32::WHITE;
                                ui.label(egui::RichText::new(format!("{} 号機", node.machine)).size(21.0).strong().color(text));
                                ui.label(egui::RichText::new(node.state()).strong().color(text));
                                ui.separator();
                                ui.label(egui::RichText::new(if node.tool_name.is_empty() { "金型未確認" } else { &node.tool_name }).size(15.0).strong().color(text));
                                ui.label(egui::RichText::new(format!("型番: {}", node.tool_no)).color(text));
                                ui.label(egui::RichText::new(&node.product).color(text));
                                if ui.button("Chi tiết").clicked() {
                                    self.selected_machine = Some(node.machine.clone());
                                }
                            });
                            if idx % 3 == 2 { ui.end_row(); }
                        }
                    });
                }
                if let Some(machine) = &self.selected_machine {
                    if let Some(node) = self.nodes.iter().find(|n| &n.machine == machine) {
                        ui.separator();
                        ui.heading(format!("Chi tiết máy {}", node.machine));
                        ui.label(format!("Lệnh sản xuất: {}", node.order));
                        ui.label(format!("Tên khuôn: {}", node.tool_name));
                        ui.label(format!("Tên sản phẩm: {}", node.product));
                        ui.label(format!("STATUS: {} ({})", node.status, node.state()));
                        ui.label(format!("Sản phẩm OK: {} | NG: {}", node.good, node.bad));
                        ui.label(format!("Nhân viên: {}", node.staff));
                        ui.label("Lý do dừng từ CAUSE: chưa có khóa sự kiện để truy xuất.");
                    }
                }
            });
            if !self.show_setup { return; }
            ui.separator();
            ui.heading("MICS Data - Firebird Connector");
            ui.label(format!("Version v{VERSION} | Windows | Read-only"));
            if let Ok(path) = Settings::path() {
                ui.small(format!("INI: {}", path.display()));
            }
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
            ui.label("Cấu hình: setting.ini (cùng thư mục EXE). Mật khẩu và sync token không ghi vào file.");
            ui.horizontal(|ui|{
                ui.label("Cloudflare sync URL");
                ui.text_edit_singleline(&mut self.settings.sync_url);
            });
            ui.small("Token đặt trong biến môi trường Windows MICS_SYNC_TOKEN; Worker secret MICS_SYNC_TOKEN phải giống nhau.");
            ui.horizontal(|ui| {
                if ui.button("Lưu cấu hình").clicked() {
                    self.status = match self.settings.save() {
                        Ok(()) => "Đã lưu setting.ini cạnh mics-data.exe (không lưu mật khẩu).".into(),
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
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Trạng thái máy (MACHINEWORK)")).clicked() {
                    self.start_db(DbOperation::MachineWork);
                }
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Máy và khuôn (SCHEDULE + TOOL)")).clicked() {
                    self.start_db(DbOperation::ScheduleNow);
                }
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Cột khuôn (TOOL)")).clicked() {
                    self.start_db(DbOperation::ToolColumns);
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Danh mục lý do (CAUSE)")).clicked() {
                    self.start_db(DbOperation::CauseCatalog);
                }
                if ui.add_enabled(!self.busy && !self.updating, egui::Button::new("Tìm bảng ghi lý do dừng")).clicked() {
                    self.start_db(DbOperation::CauseReferences);
                }
            });
            ui.small("MACHINEWORK → SCHEDULE → TOOL. CAUSE là danh mục; chưa xác định bảng lưu mã lý do theo máy.");
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
        Box::new(|cc| {
            install_windows_fonts(&cc.egui_ctx);
            Ok(Box::new(MicsApp::new()))
        })
    ) {
        let path = std::env::temp_dir().join("mics-data-startup-error.txt");
        let _ = fs::write(path, e.to_string());
    }
}
