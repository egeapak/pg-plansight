//! Phase 2a: automatic capture via a background worker that incrementally tails
//! the auto_explain log file and feeds the (validated) Phase 1 pipeline.
//!
//! The worker reads only newly-appended bytes each tick, advancing a durable
//! byte offset so restarts never double-count. To avoid splitting a log entry
//! across two reads it only consumes up to the start of the *last* entry header
//! in the new content (the trailing, possibly-incomplete entry is deferred to
//! the next tick).

use crate::aggregate::aggregate_captures;
use crate::{
    capture_mode, persist_rows, ring, CaptureMode, GUC_DATABASE, GUC_FLUSH_INTERVAL, GUC_LOG_PATH,
};
use pgrx::bgworkers::{BackgroundWorker, BackgroundWorkerBuilder, SignalWakeFlags};
use pgrx::pg_sys::pg_try::PgTryBuilder;
use pgrx::prelude::*;
use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

/// Never read more than this many bytes from the log in a single tick, to bound
/// the worker's transient memory. Larger backlogs drain over multiple ticks.
const MAX_READ_BYTES: u64 = 128 * 1024 * 1024;

/// Register the worker. Only valid from `_PG_init` during
/// `shared_preload_libraries` processing.
pub(crate) fn register() {
    BackgroundWorkerBuilder::new("pg_plansight flusher")
        .set_function("plansight_bgworker_main")
        .set_library("pg_plansight")
        .enable_spi_access()
        .set_restart_time(Some(Duration::from_secs(10)))
        .load();
}

#[pg_guard]
#[no_mangle]
pub extern "C-unwind" fn plansight_bgworker_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    let db = GUC_DATABASE
        .get()
        .and_then(|c| c.to_str().ok().map(str::to_owned))
        .unwrap_or_else(|| "postgres".to_string());
    BackgroundWorker::connect_worker_to_spi(Some(&db), None);

    log!("pg_plansight background worker started (database={db})");

    while BackgroundWorker::wait_latch(Some(Duration::from_secs(
        GUC_FLUSH_INTERVAL.get().max(1) as u64
    ))) {
        // Apply a pending SIGHUP config reload so GUC changes (e.g. switching
        // capture_mode or log_path at runtime) actually reach this worker; GUC
        // values are only refreshed by ProcessConfigFile.
        if BackgroundWorker::sighup_received() {
            unsafe { pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP) };
        }

        // Always drain the in-process capture ring. A backend can enable hook
        // capture for just its session (capture_mode is Suset), filling the ring
        // even when the worker's own capture_mode is still off — so draining must
        // not be gated on the worker's view of capture_mode.
        drain_hook_ring();

        // Beyond that, only log mode has worker-side work (tailing the file).
        if capture_mode() != CaptureMode::Log {
            continue;
        }

        let Some(path) = GUC_LOG_PATH
            .get()
            .and_then(|c| c.to_str().ok().map(str::to_owned))
        else {
            continue;
        };
        if path.is_empty() {
            continue;
        }

        // Each cycle is its own transaction: the offset advances iff the stats
        // it produced committed. A hard PG error aborts the transaction and the
        // worker restarts (set_restart_time); a soft spi::Error is logged.
        match BackgroundWorker::transaction(|| flush_cycle(&path)) {
            Ok(n) if n > 0 => log!("pg_plansight: ingested {n} query group(s) from {path}"),
            Ok(_) => {}
            Err(e) => warning!("pg_plansight: flush failed: {e}"),
        }
    }

    log!("pg_plansight background worker exiting");
}

/// Drain the in-process capture ring (hook mode): aggregate off the hot path,
/// then persist in a transaction.
fn drain_hook_ring() {
    // Cumulative dropped count seen last cycle, to log only the new drops.
    thread_local! {
        static LAST_DROPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    let (captures, dropped_total) = ring::drain();
    let new_drops = dropped_total.saturating_sub(LAST_DROPPED.with(std::cell::Cell::get));
    LAST_DROPPED.with(|c| c.set(dropped_total));
    if new_drops > 0 {
        warning!(
            "pg_plansight: capture ring full, dropped {new_drops} execution(s) \
             (raise plansight.flush_interval frequency or lower sample_rate)"
        );
    }
    if captures.is_empty() {
        return;
    }
    // Heavy parse/analysis happens here, outside the transaction and off the
    // query hot path. It runs over captured (possibly truncated/odd) plan text,
    // so isolate it: a panic in the parser/analyzers degrades to a dropped batch
    // and a warning, never a worker FATAL/restart.
    let rows =
        PgTryBuilder::new(|| aggregate_captures(captures, crate::GUC_SLO_THRESHOLD_MS.get()))
            .catch_others(|_| {
                warning!("pg_plansight: analysis failed on a captured batch; dropping it");
                Vec::new()
            })
            .execute();
    if rows.is_empty() {
        return;
    }
    match BackgroundWorker::transaction(move || {
        Spi::connect_mut(|client| persist_rows(client, &rows))
    }) {
        Ok(n) if n > 0 => log!("pg_plansight: captured {n} query group(s) via hook"),
        Ok(_) => {}
        Err(e) => warning!("pg_plansight: hook persist failed: {e}"),
    }
}

/// One flush: read new complete log entries, aggregate, persist, advance offset.
/// All SPI work runs in the surrounding `BackgroundWorker::transaction`.
fn flush_cycle(path: &str) -> Result<i64, spi::Error> {
    Spi::connect_mut(|client| {
        let offset = current_offset(client, path)?;
        let (text, new_offset) = match read_new_complete(path, offset) {
            Ok(v) => v,
            Err(e) => {
                warning!("pg_plansight: cannot read {path}: {e}");
                return Ok(0);
            }
        };
        if text.is_empty() {
            // Nothing complete yet; still record the (possibly rotation-reset)
            // offset so a truncated file is handled.
            if new_offset != offset {
                store_offset(client, path, new_offset)?;
            }
            return Ok(0);
        }

        let rows = crate::aggregate::aggregate_log(&text, crate::GUC_SLO_THRESHOLD_MS.get());
        let written = crate::persist_rows(client, &rows)?;
        store_offset(client, path, new_offset)?;
        Ok(written)
    })
}

fn current_offset(client: &mut spi::SpiClient<'_>, path: &str) -> Result<u64, spi::Error> {
    let table = client.select(
        "SELECT byte_offset FROM plansight.ingest_offset WHERE log_path = $1",
        Some(1),
        &[path.into()],
    )?;
    let off = match table.first().get::<i64>(1) {
        Ok(Some(v)) => v.max(0) as u64,
        _ => 0,
    };
    Ok(off)
}

fn store_offset(
    client: &mut spi::SpiClient<'_>,
    path: &str,
    offset: u64,
) -> Result<(), spi::Error> {
    client.update(
        "INSERT INTO plansight.ingest_offset (log_path, byte_offset, updated_at) \
         VALUES ($1, $2, now()) \
         ON CONFLICT (log_path) DO UPDATE SET byte_offset = EXCLUDED.byte_offset, updated_at = now()",
        None,
        &[path.into(), (offset as i64).into()],
    )?;
    Ok(())
}

/// Read newly-appended bytes from `path` starting at `offset`, returning the
/// text up to the last complete entry boundary and the offset to resume from.
/// Handles rotation (file shorter than the stored offset → restart at 0).
fn read_new_complete(path: &str, offset: u64) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    let start = if size < offset { 0 } else { offset };
    if size <= start {
        return Ok((String::new(), start));
    }
    let to_read = (size - start).min(MAX_READ_BYTES);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; to_read as usize];
    f.read_exact(&mut buf)?;

    // Compute the boundary on the RAW bytes: the durable offset must be a
    // file position. Decoding first and indexing into the lossy string would
    // drift whenever invalid UTF-8 is replaced (U+FFFD is 3 bytes standing in
    // for 1-3 raw bytes), skipping or re-reading log content forever after.
    let boundary = last_complete_boundary(&buf);
    let text = String::from_utf8_lossy(&buf[..boundary]).into_owned();
    Ok((text, start + boundary as u64))
}

/// Byte offset of the last auto_explain entry header in `bytes`. Entries before
/// it are complete (each ends where the next begins); the trailing entry is
/// deferred. Returns 0 when fewer than two entries are present.
fn last_complete_boundary(bytes: &[u8]) -> usize {
    let mut last_header: Option<usize> = None;
    let mut header_count = 0usize;
    let mut pos = 0usize;
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        if is_entry_header(line) {
            header_count += 1;
            last_header = Some(pos);
        }
        pos += line.len();
    }
    if header_count < 2 {
        0
    } else {
        last_header.unwrap_or(0)
    }
}

/// True if `line` begins an auto_explain plan entry, i.e. a `YYYY-MM-DD HH:MM:SS`
/// timestamped log line carrying `duration: ... plan:`. The timestamp shape
/// check is shared with the parser (core's is_log_line_start) so the durable
/// offset and the parser agree on what starts a line.
fn is_entry_header(line: &[u8]) -> bool {
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
    pg_plansight_core::parser_utils::is_log_line_start(line)
        && contains(line, b"duration:")
        && contains(line, b"plan:")
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "2025-06-25 00:03:51.601 UTC [1] LOG:  duration: 10.0 ms  plan:\n";
    const BODY: &str =
        "\tQuery Text: SELECT 1\n\tSeq Scan on t  (cost=0.00..1.10 rows=1 width=4)\n";

    #[test]
    fn no_boundary_for_single_entry() {
        let text = format!("{H1}{BODY}");
        assert_eq!(last_complete_boundary(text.as_bytes()), 0);
    }

    #[test]
    fn boundary_at_last_header_for_two_entries() {
        let first = format!("{H1}{BODY}");
        let text = format!("{first}{H1}{BODY}");
        assert_eq!(last_complete_boundary(text.as_bytes()), first.len());
    }

    #[test]
    fn ignores_non_header_lines() {
        assert!(!is_entry_header(b"\tIndex Cond: (a = 1)\n"));
        assert!(!is_entry_header(
            b"2025-06-25 00:03:51.601 UTC [1] LOG:  statement: SELECT 1\n"
        ));
        assert!(is_entry_header(H1.as_bytes()));
    }

    #[test]
    fn boundary_is_a_raw_byte_offset_despite_invalid_utf8() {
        // A LATIN1 'é' (0xE9) inside the first entry: the boundary must count
        // raw bytes, not positions in a lossy-decoded string (U+FFFD is 3
        // bytes where the raw input had 1).
        let mut first = Vec::new();
        first.extend_from_slice(H1.as_bytes());
        first.extend_from_slice(
            b"\tQuery Text: SELECT 'caf\xE9'\n\tSeq Scan on t  (cost=0.00..1.10 rows=1 width=4)\n",
        );
        let mut text = first.clone();
        text.extend_from_slice(H1.as_bytes());
        text.extend_from_slice(BODY.as_bytes());

        assert_eq!(last_complete_boundary(&text), first.len());
    }
}
