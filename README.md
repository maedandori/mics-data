# MICS Data – Firebird Connector (Rust) v0.3.0

Windows graphical Firebird database inspector. Read-only database transactions.

## Portable setting.ini

After launching `mics-data.exe`, enter the connection values and press **Lưu cấu hình**. The application creates `setting.ini` **in the same folder as the running executable**. It does not write new settings to AppData. An existing AppData `MICS-Data/settings.json` from v0.2.1 is read only as a one-time fallback when `setting.ini` is absent. Save once to create the new INI.

Example:

```ini
[firebird]
host=192.168.1.10
port=3050
database=D:\\MICS\\DATA.FDB
user=SYSDBA
```

The database path must be the path on the Firebird server. The password is kept only in RAM and is not saved as plaintext. Use a Firebird user with SELECT-only privileges where feasible.

**Important:** The executable folder must be writable. For example, use `C:\\MICS-Data\\mics-data.exe` where your Windows account can write. Running from `Program Files` often needs elevated permissions to create the INI and auto-update. A save failure is reported visibly inside the app.

## Icon

A custom blue database icon is generated in `build.rs` and embedded in the Windows EXE at compilation; no adjacent `.ico` file is needed. Windows Explorer may cache an older icon until refreshed.

## Building and updating

Open GitHub Actions **Build MICS Data Windows**, run the workflow and optionally enable `publish_release` to publish `v0.2.2` after build success.

New builds on main are Actions artifacts. Only GitHub Releases with a newer semantic version are used by the built-in updater.

Updates download an EXE and SHA-256 file, verify the checksum, close the app, replace the EXE using a helper and restart. The `setting.ini` in the directory is not replaced.

## Safety

- No SQL data-writing operations are exposed.
- Do not commit private production addresses, password files, database files or exports to this public repo.
- A SHA-256 checksum is only an integrity check, not a signature proving publisher authenticity.
- The first build and real Windows/Firebird connection must be validated before production use.

## Live machine dashboard (v0.3.0)

The first screen shows 14 machine nodes, read directly from Firebird without any Cloudflare service:

- `MACHINEWORK` is the source of machine order, status, OK/NG counters, and staff.
- `SCHEDULE` is joined using **both** `MORDERNO` and `MACHINENO`.
- `TOOL` is joined using `SCHEDULE.TOOLENTRYNO = TOOL.ENTRYNO`.
- Known status codes based on observed sample: 200 = running, 420/450 = stopped. All other codes = unknown.
- The current stop-cause event/foreign key is NOT established, so the dashboard does not claim to read a stop reason from `CAUSE`.
- Nodes show machine number, mold name, product, mold number, state. Click **Chi tiết** for order, OK/NG count and staff.
- Press **Làm mới** for immediate Firebird refresh. While a successful data snapshot is loaded, the app refreshes every 60 seconds in the background.
- Because the Firebird password is intentionally not persisted, enter it and press **Làm mới** after launching the application.
- The dashboard uses only SELECT statements and remains usable if GitHub update checks fail.
- Multiple matching schedules per machine are marked ambiguous, not silently selected.

**Deployment note:** Version v0.3.0 code has not been validated against the live company Firebird server. Verify the Windows CI result and the actual database schema before trusting the live display.

## One-way MICS → Cloudflare D1 sync (v0.3.1)

The Windows GUI queries Firebird in **read-only** transactions. It sends only changed machine snapshots, in one HTTPS POST (14 machines per batch), to the Machine-Control Worker. The Worker stores the latest values in a separate D1 table and broadcasts a **mics-changed** message over its existing WebSocket room to logged-in viewers. No Cloudflare-to-Firebird connection is made, and MICS records are never changed.

Setup (Windows, run in a Command Prompt as the same Windows user running the app):

```bat
setx MICS_SYNC_TOKEN "PASTE_THE_SHARED_TOKEN_HERE"
```

Restart MICS Data after using `setx`; existing processes do not inherit newly set environment variables. In app settings, fill in:

```ini
[firebird]
sync_url=https://machine-control.maedandoris.workers.dev/api/integrations/mics/snapshot
```

Keep existing host/port/database/user in `setting.ini`; **do not replace the whole file with the abbreviated example**. Use the **same secret value** as Cloudflare Worker secret `MICS_SYNC_TOKEN`, configured in Cloudflare settings (never in GitHub source).

In the GUI, enter the Firebird password and click **Làm mới**. After data refresh, the app attempts a POST when the snapshot hash has changed. Success and failure appear next to the dashboard status. Retries occur on subsequent refreshes (60 s) until successful. Existing data remains visible in D1 if the company PC goes offline; timestamps indicate staleness.

Security: `setx` stores the token in the Windows user's environment (not an encrypted vault). Restrict Windows user access; rotate the token if exposed. Keep the token out of logs, GitHub, screenshots, and the INI. HTTPS is required. Do not expose Firebird on the public internet.

At this stage, runtime validation against the real Firebird schema and Cloudflare configuration is still required.
