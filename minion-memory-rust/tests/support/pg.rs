//! Project-role PostgreSQL fixtures; credentials are read, never committed or printed.
use postgres::{Client, Config, NoTls};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

pub fn credentials() -> HashMap<String, String> {
    let path = std::env::var_os("MNEME_PG_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .find_map(|p| {
                    let candidate = p.join(".tmp/work/pg.env");
                    candidate.is_file().then_some(candidate)
                })
                .expect("project-local pg.env or MNEME_PG_ENV is required")
        });
    std::fs::read_to_string(path)
        .expect("read project credentials")
        .lines()
        .filter(|s| !s.trim().starts_with('#'))
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.trim().into(), v.trim().into()))
        .collect()
}
fn base_config() -> Config {
    let env = credentials();
    let mut config = Config::new();
    assert_eq!(
        env.get("PGUSER").map(String::as_str),
        Some("mneme_dev"),
        "only the ordinary project role is allowed for fixtures"
    );
    config.host(env.get("PGHOST").map_or("127.0.0.1", String::as_str));
    config.port(env.get("PGPORT").map_or(5432, |p| p.parse().expect("port")));
    config.user(env.get("PGUSER").expect("ordinary role"));
    if let Some(password) = env.get("PGPASSWORD") {
        config.password(password);
    }
    config.dbname(env.get("PGDATABASE").map_or("postgres", String::as_str));
    config
}
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}
fn conninfo(config: &Config, dbname: &str, options: &str) -> String {
    let env = credentials();
    let _ = config;
    [
        (
            "host",
            env.get("PGHOST")
                .map_or("127.0.0.1", String::as_str)
                .to_owned(),
        ),
        (
            "port",
            env.get("PGPORT").map_or("5432", String::as_str).to_owned(),
        ),
        ("user", env.get("PGUSER").expect("role").clone()),
        (
            "password",
            env.get("PGPASSWORD").cloned().unwrap_or_default(),
        ),
        ("dbname", dbname.into()),
        ("options", options.into()),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", quoted(v)))
    .collect::<Vec<_>>()
    .join(" ")
}
static DATABASE: LazyLock<(Config, String)> = LazyLock::new(|| {
    let name=std::env::var("MNEME_RUST_PG_DATABASE").expect("set MNEME_RUST_PG_DATABASE to the owned throwaway DB; the gate wrapper drops it afterwards");
    assert!(
        name.starts_with("mneme_codex_")
            && name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    );
    let mut config = base_config();
    let mut c = config.connect(NoTls).expect("connect as project role");
    if c.query_opt(
        "SELECT 1 FROM pg_catalog.pg_database WHERE datname=$1",
        &[&name],
    )
    .unwrap()
    .is_none()
    {
        c.batch_execute(&format!("CREATE DATABASE \"{name}\" TEMPLATE template0 ENCODING 'UTF8' LC_COLLATE 'C' LC_CTYPE 'C' TABLESPACE mneme_ts")).expect("create E: test database");
    }
    config.dbname(&name);
    (config, name)
});
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// One private schema inside the gate's owned throwaway database.
pub struct Backend {
    pub config: Config,
    pub conninfo: String,
    pub namespace: String,
}
impl Backend {
    pub fn new() -> Self {
        let namespace = format!(
            "mneme_test_{}_{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let (base, name) = &*DATABASE;
        let options =
            format!("-c search_path={namespace} -c statement_timeout=120000 -c lock_timeout=10000");
        let mut config = base.clone();
        config.options(&options);
        let info = conninfo(base, name, &options);
        base.connect(NoTls)
            .unwrap()
            .batch_execute(&format!("CREATE SCHEMA \"{namespace}\""))
            .unwrap();
        Self {
            config,
            conninfo: info,
            namespace,
        }
    }
    pub fn client(&self) -> Client {
        self.config.connect(NoTls).unwrap()
    }
    pub fn with_options(&self, extra: &str) -> String {
        let (config, name) = &*DATABASE;
        conninfo(
            config,
            name,
            &format!(
                "-c search_path={} -c statement_timeout=120000 -c lock_timeout=10000 {extra}",
                self.namespace
            ),
        )
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        if let Ok(mut c) = DATABASE.0.connect(NoTls) {
            let _ = c.batch_execute(&format!(
                "DROP SCHEMA IF EXISTS \"{}\" CASCADE",
                self.namespace
            ));
        }
    }
}
