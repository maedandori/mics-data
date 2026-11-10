# MICS Data – Windows Firebird Connector

v0.2.0 is a graphical Windows application. Double-click `mics-data.exe`; it stays open and shows server configuration, connection test, tables, columns, counts, version and update status.

## Safety and database access
- Transactions are **read-only**, no INSERT/UPDATE/DELETE.
- Use a SELECT-only Firebird user if possible.
- Database path means the **absolute path on the Firebird server**, not on your local PC.
- Password is never persisted; re-enter it whenever you launch the app.
- Host, port, database path and user are saved under Windows user config directory (AppData).
- Never commit real database credentials or .fdb files to this public repository.

## Install and update
1. Download `mics-data.exe` from the latest **GitHub Release** or from Actions artifact.
2. Save it in a **writable folder** (for example `%LOCALAPPDATA%\\MICS-Data\\mics-data.exe`). Do not place it in Program Files unless the user has write access.
3. Double-click. Fill in connection details and click **Kiểm tra kết nối**.
4. On launch and once every 6 hours while idle, it checks GitHub Releases. When a newer semantic version exists, it downloads the signed-by-checksum release payload, verifies SHA-256, closes the UI, replaces the EXE via an isolated helper copy, and reopens.
5. Click **Kiểm tra cập nhật** to trigger a check immediately.

The updater only installs **GitHub Releases**, not Actions artifact builds. Creating a tagged release `v0.2.0` (or a later version) runs the Windows build and publishes its EXE and SHA-256 checksum. Builds on `main` are development artifacts and do not become auto-updates.

The checksum protects against download corruption, but is not a cryptographic publisher signature. For high-assurance deployments, add Windows Authenticode signing.

## Build
The workflow builds and checks Rust on `windows-latest`, uploads EXE+hash, and publishes a release on version tags.

## Notes
If no internet is available, the update check reports an error but the GUI stays usable.
If no server parameters are configured, the GUI stays open and asks for settings.
Database connections are background threads so the UI remains responsive. Avoid scanning a large database with counts during production peaks.
If the server uses unsupported legacy Firebird authentication, a native client mode may be necessary.
