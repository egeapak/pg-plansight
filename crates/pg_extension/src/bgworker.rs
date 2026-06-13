//! Phase 2a: automatic capture via a background worker that incrementally tails
//! the auto_explain log file and feeds the (validated) Phase 1 pipeline.
//!
//! The worker reads only newly-appended bytes each tick, advancing a durable
//! byte offset so restarts never double-count. To avoid splitting a log entry
//! across two reads it only consumes up to the start of the *last* entry header
//! in the new content (the trailing, possibly-incomplete entry is deferred to
//! the next tick).

use crate::{capture_mode, CaptureMode, GUC_DATABASE, GUC_FLUSH_INTERVAL, GUC_LOG_PATH};
use pgrx::bgworkers::{BackgroundWorker, BackgroundWorkerBuilder, SignalWakeFlags};
use pgrx::prelude::*;
use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

/// Never read more than this many bytes from the log in a single tick, to bound
/// the worker's transient memory. Larger backlogs drain over multiple ticks.
const MAX_READ_BYTES: u64 = 128 * 1024 * 1024;

/// Register the worker. Only valid from `_PG_init` during
/// `shared_preload_libraries` processing.
pub(crate) fn register() {
    BackgroundWorkerBuilder::new("pg_loganalyze flusher")
        .set_function("loganalyze_bgworker_main")
        .set_library("pg_loganalyze")
        .enable_spi_access()
        .set_restart_time(Some(Duration::from_secs(10)))
        .load();
}

#[pg_guard]
#[no_mangle]
pub extern "C-unwind" fn loganalyze_bgworker_main(_arg: pg_sys::Datum) {
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    let db = GUC_DATABASE
        .get()
        .and_then(|c| c.to_str().ok().map(str::to_owned))
        .unwrap_or_else(|| "postgres".to_string());
    BackgroundWorker::connect_worker_to_spi(Some(&db), None);

    log!("pg_loganalyze background worker started (database={db})");

    let mut warned_hook = false;
    while BackgroundWorker::wait_latch(Some(Duration::from_secs(
        GUC_FLUSH_INTERVAL.get().max(1) as u64
    ))) {
        // Apply a pending SIGHUP config reload so GUC changes (e.g. switching
        // capture_mode or log_path at runtime) actually reach this worker; GUC
        // values are only refreshed by ProcessConfigFile.
        if BackgroundWorker::sighup_received() {
            unsafe { pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP) };
        }

        match capture_mode() {
            CaptureMode::Off => continue,
            CaptureMode::Hook => {
                // Phase 2b drains an in-process shmem ring here. Until that lands,
                // warn once so a misconfiguration is visible.
                if !warned_hook {
                    warning!(
                        "pg_loganalyze: capture_mode='hook' is not yet implemented; \
                         use 'log' for now"
                    );
                    warned_hook = true;
                }
                continue;
            }
            CaptureMode::Log => {}
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
            Ok(n) if n > 0 => log!("pg_loganalyze: ingested {n} query group(s) from {path}"),
            Ok(_) => {}
            Err(e) => warning!("pg_loganalyze: flush failed: {e}"),
        }
    }

    log!("pg_loganalyze background worker exiting");
}

/// One flush: read new complete log entries, aggregate, persist, advance offset.
/// All SPI work runs in the surrounding `BackgroundWorker::transaction`.
fn flush_cycle(path: &str) -> Result<i64, spi::Error> {
    Spi::connect_mut(|client| {
        let offset = current_offset(client, path)?;
        let (text, new_offset) = match read_new_complete(path, offset) {
            Ok(v) => v,
            Err(e) => {
                warning!("pg_loganalyze: cannot read {path}: {e}");
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

        let rows = crate::aggregate::aggregate_log(&text);
        let written = crate::persist_rows(client, &rows)?;
        store_offset(client, path, new_offset)?;
        Ok(written)
    })
}

fn current_offset(client: &mut spi::SpiClient<'_>, path: &str) -> Result<u64, spi::Error> {
    let table = client.select(
        "SELECT byte_offset FROM loganalyze.ingest_offset WHERE log_path = $1",
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
        "INSERT INTO loganalyze.ingest_offset (log_path, byte_offset, updated_at) \
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
    let text = String::from_utf8_lossy(&buf).into_owned();

    let boundary = last_complete_boundary(&text);
    let consumed = boundary as u64;
    Ok((text[..boundary].to_string(), start + consumed))
}

/// Byte offset of the last auto_explain entry header in `text`. Entries before
/// it are complete (each ends where the next begins); the trailing entry is
/// deferred. Returns 0 when fewer than two entries are present.
fn last_complete_boundary(text: &str) -> usize {
    let mut last_header: Option<usize> = None;
    let mut header_count = 0usize;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
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
/// timestamped log line carrying `duration: ... plan:`.
fn is_entry_header(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() > 19
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b' '
        && line.contains("duration:")
        && line.contains("plan:")
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
        assert_eq!(last_complete_boundary(&text), 0);
    }

    #[test]
    fn boundary_at_last_header_for_two_entries() {
        let first = format!("{H1}{BODY}");
        let text = format!("{first}{H1}{BODY}");
        assert_eq!(last_complete_boundary(&text), first.len());
    }

    #[test]
    fn ignores_non_header_lines() {
        assert!(!is_entry_header("\tIndex Cond: (a = 1)\n"));
        assert!(!is_entry_header(
            "2025-06-25 00:03:51.601 UTC [1] LOG:  statement: SELECT 1\n"
        ));
        assert!(is_entry_header(H1));
    }
}
