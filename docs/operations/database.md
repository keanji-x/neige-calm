# Locate and inspect SQLite

## Locate the active database

1. Identify the target instance and read the configuration file named in its start command. Check explicit command-line overrides.
2. Resolve the effective `child.db_url` and `child.data_dir`. When the DB URL is omitted, neige-app uses `sqlite://<data_dir>/calm.db?mode=rwc`. An explicit URL can point elsewhere. The development `mock` setting uses an in-memory database.
3. Resolve relative paths against the service working directory. Confirm the file belongs to the target instance and record its absolute path for the investigation.

See the [configuration guide](../neige-app-config.md) for precedence and installation layouts.

## Inspect the current format

Persistent state uses SQLite. In WAL mode, the `-wal` and `-shm` sidecars can coexist with the main file. Use the [backup and restore procedure](../deploy-and-upgrade.md#81-database-backup-and-manual-restore-read-this-before-allowbreaking) for a consistent recovery copy.

Prefer the product's read-only state interfaces. When a database read is needed, open the resolved file with:

```sh
sqlite3 -readonly /absolute/path/to/calm.db
```

Inspect the live schema before writing a query:

```sql
SELECT sqlite_version();
PRAGMA database_list;
PRAGMA journal_mode;
.tables
.schema <table_name>
```

The last two lines are sqlite3 CLI commands. Replace `<table_name>` with a name from `.tables`. Where present, inspect the schema and migration history in `_sqlx_migrations` and compare it with the running release.

Select the fields and stable IDs needed for the investigation, and limit results. Confirm Track, attempt, and session ownership before joining rows. Use [transaction observations](../db-transaction-observation.md) for write wait and retry evidence. Record the query time and scope, and keep failures distinct from empty results. Use the applicable runbook for integrity checks, checkpoints, and recovery.
