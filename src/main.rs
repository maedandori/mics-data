use std::{env, error::Error, io::{self, Write}};
use rsfbclient::prelude::*;

type AppResult<T> = Result<T, Box<dyn Error>>;

fn required(name: &str) -> AppResult<String> {
    let value = env::var(name).map_err(|_| format!("Missing environment variable: {name}"))?;
    if value.trim().is_empty() { return Err(format!("{name} must not be empty").into()); }
    Ok(value)
}

fn usage() {
    eprintln!("MICS Data v0.1.0 - Firebird read-only inspector");
    eprintln!("Usage: mics-data <test|tables|columns TABLE|count TABLE>");
    eprintln!("Required env: MICS_FB_HOST, MICS_FB_DATABASE, MICS_FB_USER");
    eprintln!("Optional env: MICS_FB_PORT (default 3050)");
    eprintln!("Password is prompted securely unless MICS_FB_PASSWORD is set.");
}

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() { usage(); return Err("Missing command".into()); }
    match args[0].as_str() {
        "test" | "tables" if args.len() == 1 => (),
        "columns" | "count" if args.len() == 2 => (),
        _ => { usage(); return Err("Invalid command or arguments".into()); }
    };

    let host = required("MICS_FB_HOST")?;
    let database = required("MICS_FB_DATABASE")?;
    let user = required("MICS_FB_USER")?;
    let port: u16 = env::var("MICS_FB_PORT")
        .unwrap_or_else(|_| "3050".into()).parse()?;
    let password = match env::var("MICS_FB_PASSWORD") {
        Ok(value) => value,
        Err(_) => {
            print!("Firebird password for {user}: ");
            io::stdout().flush()?;
            rpassword::read_password()?
        }
    };
    if password.is_empty() { return Err("Password cannot be empty".into()); }

    // Pure Rust network protocol: no fbclient.dll installation needed.
    // Every query uses a read-only transaction to protect the production DB.
    let mut builder = rsfbclient::builder_pure_rust();
    builder.host(host).port(port).db_name(database).user(user).pass(password);
    builder.with_transaction(|tr| tr.read_only().no_wait());
    let mut conn = builder.connect()?;
    println!("Connected to Firebird (read-only).");

    match args[0].as_str() {
        "test" => {
            let rows: Vec<(i64,)> = conn.query(
                "SELECT COUNT(*) FROM RDB$DATABASE", ()
            )?;
            println!("Connection test OK: {} system row(s).", rows[0].0);
        }
        "tables" => {
            let tables: Vec<(String,)> = conn.query(
                "SELECT TRIM(RDB$RELATION_NAME) FROM RDB$RELATIONS \
                 WHERE RDB$SYSTEM_FLAG = 0 AND RDB$VIEW_BLR IS NULL \
                 ORDER BY RDB$RELATION_NAME", ()
            )?;
            println!("User tables: {}", tables.len());
            for (name,) in tables { println!("{name}"); }
        }
        "columns" => {
            let table = &args[1];
            let columns: Vec<(String, i64)> = conn.query(
                "SELECT TRIM(RDB$FIELD_NAME), CAST(RDB$FIELD_POSITION AS BIGINT) \
                 FROM RDB$RELATION_FIELDS \
                 WHERE TRIM(RDB$RELATION_NAME) = ? \
                 ORDER BY RDB$FIELD_POSITION",
                (table.as_str(),)
            )?;
            if columns.is_empty() { return Err("Table not found or no columns".into()); }
            for (name, pos) in columns { println!("{}: {}", pos + 1, name); }
        }
        "count" => {
            // Never interpolate untrusted input into SQL. Resolve to actual
            // metadata names and only accept conservative unquoted identifiers.
            let table = &args[1];
            if table.is_empty() || table.len() > 63
                || !table.starts_with(|c: char| c.is_ascii_alphabetic())
                || !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            { return Err("Invalid table identifier".into()); }
            let exists: Vec<(i64,)> = conn.query(
                "SELECT COUNT(*) FROM RDB$RELATIONS \
                 WHERE TRIM(RDB$RELATION_NAME) = ? \
                 AND RDB$SYSTEM_FLAG = 0 AND RDB$VIEW_BLR IS NULL",
                (table.as_str(),)
            )?;
            if exists[0].0 != 1 { return Err("Table does not exist".into()); }
            let sql = format!("SELECT COUNT(*) FROM {table}");
            let rows: Vec<(i64,)> = conn.query(&sql, ())?;
            println!("{table}: {} rows", rows[0].0);
        }
        _ => unreachable!()
    };
    Ok(())
}
