# MICS Data (Rust)

Small **read-only** Firebird inspector for MICS on Windows. Connects to an existing Firebird server over TCP; it does not control MICS DBEdit, create a local copy of the database, or change records. The pure-Rust driver does not require `fbclient.dll`.

## Windows setup

Set environment variables in Command Prompt (examples are placeholders; use actual company server values):

```bat
set "MICS_FB_HOST=192.168.1.10"
set "MICS_FB_PORT=3050"
set "MICS_FB_DATABASE=D:\\MICS\\data.fdb"
set "MICS_FB_USER=read_only_user"
mics-data.exe test
mics-data.exe tables
mics-data.exe columns YOUR_TABLE
mics-data.exe count YOUR_TABLE
```

The database path is the path **on the Firebird server**, not the PC running the application. The password is asked interactively and hidden when typed. For unattended use, an optional `MICS_FB_PASSWORD` environment variable is supported, but secure secret storage is preferable.

**Do not commit passwords, production database files, exports, or real connection settings into this public repository.** Use a Firebird login with SELECT-only permissions where possible; the application also configures read-only transactions.

## Build

From this repository run `cargo build --release` with a current stable Rust toolchain. Or open GitHub Actions → **Build MICS Data Windows** → **Run workflow**, then retrieve the build artifact after success.

## Roadmap

After confirming a successful connection and the correct source tables, add a configurable incremental read-only sync to Cloudflare D1 (with batching/retries, mapping, and access control). That synchronization is **not included** in v0.1.0.

## Limitations

Works for Firebird servers whose authentication and wire protocol are supported by the pure-Rust driver. If the company server uses unsupported legacy auth or encryption, a native `fbclient.dll` mode may be required. The first build and a real Firebird connection must be validated on the company network.
