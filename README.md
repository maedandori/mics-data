# MICS Data – Firebird Connector (Rust) v0.2.2

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
