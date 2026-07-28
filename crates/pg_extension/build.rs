//! Compiles `src/nesting.c`, the executor-nesting shim.
//!
//! See the header comment in `src/nesting.c` for why those two hooks must be C
//! and not Rust: a Rust frame on the executor error path loses
//! `constraint_name`/`table_name`/`schema_name`/`cursorpos` from every error
//! PostgreSQL raises.
//!
//! `pg_config` is resolved exactly the way `pgrx-pg-sys` resolves it, so this
//! builds against the same server headers pgrx generated its bindings from:
//! the environment-described `PgConfig` first (`PGRX_PG_CONFIG_PATH` /
//! `PGRX_PG_CONFIG_AS_ENV`, which `cargo pgrx` sets), falling back to the
//! `pgNN` entry in `$PGRX_HOME/config.toml`.

use pgrx_pg_config::{PgConfig, Pgrx};

/// Majors this crate has a `pgNN` feature for.
const SUPPORTED_MAJORS: &[u16] = &[13, 14, 15, 16, 17, 18, 19];

fn main() {
    println!("cargo:rerun-if-changed=src/nesting.c");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PGRX_PG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=PGRX_PG_CONFIG_AS_ENV");

    let major = enabled_major();

    let pg_config = PgConfig::from_env().unwrap_or_else(|_| {
        Pgrx::from_config()
            .expect("no PgConfig in the environment and $PGRX_HOME/config.toml is unreadable")
            .get(&format!("pg{major}"))
            .unwrap_or_else(|e| panic!("no pg_config for pg{major} in $PGRX_HOME/config.toml: {e}"))
    });

    let configured_major = pg_config
        .major_version()
        .expect("pg_config did not report a major version");
    assert_eq!(
        configured_major, major,
        "feature pg{major} does not match the pg_config in use (pg{configured_major}); \
         the C shim would compile against the wrong server headers"
    );

    let includedir = pg_config
        .includedir_server()
        .expect("pg_config --includedir-server failed");

    let mut build = cc::Build::new();
    build.file("src/nesting.c").include(&includedir);

    // Windows keeps some server headers under port/win32{,_msvc}.
    if let Ok(dir) = pg_config.includedir_server_port_win32() {
        if dir.exists() {
            build.include(dir);
        }
    }
    if cfg!(target_env = "msvc") {
        if let Ok(dir) = pg_config.includedir_server_port_win32_msvc() {
            if dir.exists() {
                build.include(dir);
            }
        }
    }

    // The shim is linked into a cdylib.
    build.pic(true);
    // PostgreSQL's own headers are not warning-clean under -Wall for every
    // major; the shim itself is tiny and reviewed.
    build.warnings(false);

    build.compile("plansight_nesting");
}

/// Exactly one `pgNN` feature must be enabled, matching pgrx's own rule.
fn enabled_major() -> u16 {
    let enabled: Vec<u16> = SUPPORTED_MAJORS
        .iter()
        .copied()
        .filter(|m| std::env::var_os(format!("CARGO_FEATURE_PG{m}")).is_some())
        .collect();

    match enabled.as_slice() {
        [major] => *major,
        [] => panic!(
            "no pgNN feature enabled; pg_plansight requires exactly one of {SUPPORTED_MAJORS:?}"
        ),
        many => panic!(
            "multiple pgNN features enabled ({many:?}); `--no-default-features` is probably needed"
        ),
    }
}
