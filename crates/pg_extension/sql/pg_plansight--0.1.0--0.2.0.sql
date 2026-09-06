/* pg_plansight 0.1.0 -> 0.2.0
 *
 * Deliberately empty: nothing in the SQL surface changed between these two
 * versions. No table, view, function or type was added, removed or altered;
 * the 0.2.0 work in this extension is all below SQL level (the executor hooks
 * moved from Rust into src/nesting.c so PostgreSQL errors keep their
 * structured fields, capture setup moved inside its error-isolation guard, and
 * the capture defaults changed -- all of which are GUCs and C code, not SQL
 * objects).
 *
 * The file still has to exist. `default_version` in the .control file tracks
 * the crate version, so once that reads 0.2.0 PostgreSQL needs an explicit
 * path to get an already-installed 0.1.0 there:
 *
 *     ALTER EXTENSION pg_plansight UPDATE;
 *
 * Without this script that command fails with "extension pg_plansight has no
 * update path from version 0.1.0 to version 0.2.0", and the only way forward
 * is DROP EXTENSION -- which discards every captured statistic.
 *
 * cargo-pgrx copies any crates/pg_extension/sql/pg_plansight--<old>--<new>.sql
 * into the cluster's extension directory during `cargo pgrx package`, and the
 * globbed deb/rpm assets in Cargo.toml pick it up from there. Nothing else
 * needs wiring.
 *
 * version-check.yml fails the build if a release has no upgrade script
 * targeting it, so this must not be deleted when the version next moves.
 */
