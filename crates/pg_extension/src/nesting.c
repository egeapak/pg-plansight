/*
 * Executor nesting tracking, in C.
 *
 * WHY THIS IS NOT RUST
 * --------------------
 * These two hooks sit directly on the executor's error path: every ERROR a
 * statement raises (unique violation, check constraint, division by zero, ...)
 * unwinds through ExecutorRun.
 *
 * pgrx cannot carry a PostgreSQL error across a Rust frame without losing
 * information. `#[pg_guard]` expands to `pgrx_extern_c_guard`, which catches
 * the longjmp-turned-panic and re-raises it with `do_ereport`; the payload it
 * re-raises from was produced by `CopyErrorData`, which preserves only
 * elevel, sqlerrcode, message, domain, detail, hint, funcname, filename and
 * lineno. Everything else in ErrorData is dropped — including
 * `constraint_name`, `table_name`, `schema_name`, `column_name`,
 * `datatype_name` and `cursorpos`.
 *
 * Measured on PostgreSQL 16 with the extension merely present in
 * shared_preload_libraries (capture_mode did not matter):
 *
 *   without pg_plansight            with pg_plansight (Rust hooks)
 *   ---------------------------     ------------------------------
 *   ERROR:  23505: duplicate key    ERROR:  23505: duplicate key
 *   DETAIL: Key (i)=(1) exists.     DETAIL: Key (i)=(1) exists.
 *   SCHEMA NAME:  public            (absent)
 *   TABLE NAME:   t                 (absent)
 *   CONSTRAINT NAME: t_pkey         (absent)
 *
 * Every driver and ORM that dispatches on constraint name (Rails
 * ActiveRecord::RecordNotUnique#constraint, SQLAlchemy, sqlx, node-pg
 * err.constraint) silently falls into its generic branch — cluster-wide, for
 * every database, whether or not anything is being captured.
 *
 * PG_TRY/PG_FINALLY here does what pg_stat_statements does: on error the
 * FINALLY block runs and PG_RE_THROW() resumes the *original* longjmp with the
 * original ErrorData untouched. No copy, no loss.
 *
 * The counter is deliberately kept here too, rather than in a Rust
 * thread_local, so that decrementing it is part of the same PG_FINALLY and
 * cannot be skipped.
 */

#include "postgres.h"
#include "executor/executor.h"

static int plansight_nesting_level = 0;

static ExecutorRun_hook_type plansight_prev_ExecutorRun = NULL;
static ExecutorFinish_hook_type plansight_prev_ExecutorFinish = NULL;

/*
 * PostgreSQL 18 dropped ExecutorRun's `execute_once` parameter.
 */
#if PG_VERSION_NUM >= 180000
static void
plansight_ExecutorRun(QueryDesc *queryDesc, ScanDirection direction, uint64 count)
{
	plansight_nesting_level++;
	PG_TRY();
	{
		if (plansight_prev_ExecutorRun)
			plansight_prev_ExecutorRun(queryDesc, direction, count);
		else
			standard_ExecutorRun(queryDesc, direction, count);
	}
	PG_FINALLY();
	{
		plansight_nesting_level--;
	}
	PG_END_TRY();
}
#else
static void
plansight_ExecutorRun(QueryDesc *queryDesc, ScanDirection direction, uint64 count,
					  bool execute_once)
{
	plansight_nesting_level++;
	PG_TRY();
	{
		if (plansight_prev_ExecutorRun)
			plansight_prev_ExecutorRun(queryDesc, direction, count, execute_once);
		else
			standard_ExecutorRun(queryDesc, direction, count, execute_once);
	}
	PG_FINALLY();
	{
		plansight_nesting_level--;
	}
	PG_END_TRY();
}
#endif

/*
 * Triggers and deferred constraint checks fire during ExecutorFinish and can
 * execute nested queries, so it counts as nesting too.
 */
static void
plansight_ExecutorFinish(QueryDesc *queryDesc)
{
	plansight_nesting_level++;
	PG_TRY();
	{
		if (plansight_prev_ExecutorFinish)
			plansight_prev_ExecutorFinish(queryDesc);
		else
			standard_ExecutorFinish(queryDesc);
	}
	PG_FINALLY();
	{
		plansight_nesting_level--;
	}
	PG_END_TRY();
}

/*
 * Chain the two nesting hooks. Called from _PG_init (Rust) while
 * shared_preload_libraries is being processed, so no locking is needed.
 */
void
plansight_install_nesting_hooks(void)
{
	plansight_prev_ExecutorRun = ExecutorRun_hook;
	ExecutorRun_hook = plansight_ExecutorRun;

	plansight_prev_ExecutorFinish = ExecutorFinish_hook;
	ExecutorFinish_hook = plansight_ExecutorFinish;
}

int
plansight_nesting_level_get(void)
{
	return plansight_nesting_level;
}

/*
 * Belt-and-braces reset at transaction end. PG_FINALLY already balances the
 * counter on the error path; this covers a longjmp that escapes the executor
 * entirely (e.g. from a co-loaded hook that does not restore it).
 */
void
plansight_nesting_level_reset(void)
{
	plansight_nesting_level = 0;
}
