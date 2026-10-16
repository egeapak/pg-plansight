//! Shims over the pgrx API differences between the 0.19 line (PostgreSQL 13+)
//! and the 0.13 line (PostgreSQL 12, built from `pg12/Cargo.toml`).
//!
//! The rest of the crate is written against the 0.19 API; each item here has the
//! 0.19 signature, with the `pg12` implementation adapting to 0.13.

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};
use std::ffi::CStr;
#[cfg(not(feature = "pg12"))]
use std::ffi::CString;

/// A string-valued GUC. `get()` yields `Option<CString>` on 0.19 and
/// `Option<&CStr>` on 0.13; both support `to_str()`.
#[cfg(not(feature = "pg12"))]
pub(crate) type StringGuc = GucSetting<Option<CString>>;
#[cfg(feature = "pg12")]
pub(crate) type StringGuc = GucSetting<Option<&'static CStr>>;

#[cfg(not(feature = "pg12"))]
pub(crate) const fn string_guc(default: Option<&'static CStr>) -> StringGuc {
    GucSetting::<Option<CString>>::new(default)
}
#[cfg(feature = "pg12")]
pub(crate) const fn string_guc(default: Option<&'static CStr>) -> StringGuc {
    GucSetting::<Option<&'static CStr>>::new(default)
}

/// 0.13 takes GUC names and descriptions as `&str`.
#[cfg(feature = "pg12")]
fn text(value: &'static CStr) -> &'static str {
    value.to_str().expect("GUC text is valid UTF-8")
}

pub(crate) fn define_string_guc(
    name: &'static CStr,
    short_description: &'static CStr,
    long_description: &'static CStr,
    setting: &'static StringGuc,
    context: GucContext,
    flags: GucFlags,
) {
    #[cfg(not(feature = "pg12"))]
    GucRegistry::define_string_guc(
        name,
        short_description,
        long_description,
        setting,
        context,
        flags,
    );
    #[cfg(feature = "pg12")]
    GucRegistry::define_string_guc(
        text(name),
        text(short_description),
        text(long_description),
        setting,
        context,
        flags,
    );
}

pub(crate) fn define_bool_guc(
    name: &'static CStr,
    short_description: &'static CStr,
    long_description: &'static CStr,
    setting: &'static GucSetting<bool>,
    context: GucContext,
    flags: GucFlags,
) {
    #[cfg(not(feature = "pg12"))]
    GucRegistry::define_bool_guc(
        name,
        short_description,
        long_description,
        setting,
        context,
        flags,
    );
    #[cfg(feature = "pg12")]
    GucRegistry::define_bool_guc(
        text(name),
        text(short_description),
        text(long_description),
        setting,
        context,
        flags,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn define_int_guc(
    name: &'static CStr,
    short_description: &'static CStr,
    long_description: &'static CStr,
    setting: &'static GucSetting<i32>,
    min: i32,
    max: i32,
    context: GucContext,
    flags: GucFlags,
) {
    #[cfg(not(feature = "pg12"))]
    GucRegistry::define_int_guc(
        name,
        short_description,
        long_description,
        setting,
        min,
        max,
        context,
        flags,
    );
    #[cfg(feature = "pg12")]
    GucRegistry::define_int_guc(
        text(name),
        text(short_description),
        text(long_description),
        setting,
        min,
        max,
        context,
        flags,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn define_float_guc(
    name: &'static CStr,
    short_description: &'static CStr,
    long_description: &'static CStr,
    setting: &'static GucSetting<f64>,
    min: f64,
    max: f64,
    context: GucContext,
    flags: GucFlags,
) {
    #[cfg(not(feature = "pg12"))]
    GucRegistry::define_float_guc(
        name,
        short_description,
        long_description,
        setting,
        min,
        max,
        context,
        flags,
    );
    #[cfg(feature = "pg12")]
    GucRegistry::define_float_guc(
        text(name),
        text(short_description),
        text(long_description),
        setting,
        min,
        max,
        context,
        flags,
    );
}
