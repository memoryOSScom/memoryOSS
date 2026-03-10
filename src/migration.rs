// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use redb::{Database, ReadableTable, TableDefinition};

const SCHEMA_VERSION_TABLE: TableDefinition<&str, u64> = TableDefinition::new("_schema_version");
const VERSION_KEY: &str = "current_version";

pub const CURRENT_VERSION: u64 = 3;

type MigrationFn = fn(&Database) -> anyhow::Result<()>;

fn migrations() -> Vec<(u64, &'static str, MigrationFn)> {
    vec![
        (1, "initial schema", migrate_to_v1),
        (2, "add confidence field to memories", migrate_to_v2),
        (3, "add memory lifecycle fields", migrate_to_v3),
    ]
}

fn migrate_to_v1(_db: &Database) -> anyhow::Result<()> {
    // V1: Create the schema version table itself — it's bootstrapped by ensure_version_table
    // Future migrations will create document tables, outbox, audit log, etc.
    tracing::info!("Migration v1: initial schema created");
    Ok(())
}

fn migrate_to_v2(_db: &Database) -> anyhow::Result<()> {
    // V2: Memory.confidence field (Option<f64>) added via #[serde(default)].
    // Existing memories deserialize with confidence: None (full trust).
    // No data transformation needed — serde handles backward compatibility.
    tracing::info!("Migration v2: confidence field available (serde-compatible, no data rewrite)");
    Ok(())
}

fn migrate_to_v3(_db: &Database) -> anyhow::Result<()> {
    // V3: Memory.status, evidence_count, last_verified_at, superseded_by fields added
    // via #[serde(default)] / Option fields.
    // Existing memories deserialize as active with default evidence count.
    tracing::info!("Migration v3: lifecycle fields available (serde-compatible, no data rewrite)");
    Ok(())
}

fn ensure_version_table(db: &Database) -> anyhow::Result<()> {
    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(SCHEMA_VERSION_TABLE)?;
        if table.get(VERSION_KEY)?.is_none() {
            table.insert(VERSION_KEY, 0u64)?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn get_version(db: &Database) -> anyhow::Result<u64> {
    let tx = db.begin_read()?;
    let table = tx.open_table(SCHEMA_VERSION_TABLE)?;
    let version = table.get(VERSION_KEY)?.map(|v| v.value()).unwrap_or(0);
    Ok(version)
}

fn set_version(db: &Database, version: u64) -> anyhow::Result<()> {
    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(SCHEMA_VERSION_TABLE)?;
        table.insert(VERSION_KEY, version)?;
    }
    tx.commit()?;
    Ok(())
}

pub struct MigrationResult {
    pub from_version: u64,
    pub to_version: u64,
    pub applied: Vec<String>,
}

pub fn pending_migrations(db: &Database) -> anyhow::Result<Vec<(u64, String)>> {
    ensure_version_table(db)?;
    let current = get_version(db)?;
    let pending: Vec<_> = migrations()
        .into_iter()
        .filter(|(v, _, _)| *v > current)
        .map(|(v, desc, _)| (v, desc.to_string()))
        .collect();
    Ok(pending)
}

pub fn run_migrations(db: &Database) -> anyhow::Result<MigrationResult> {
    ensure_version_table(db)?;
    let from_version = get_version(db)?;
    let mut applied = Vec::new();

    for (version, description, migrate_fn) in migrations() {
        if version <= from_version {
            continue;
        }
        tracing::info!("Applying migration v{version}: {description}");
        migrate_fn(db)?;
        set_version(db, version)?;
        applied.push(format!("v{version}: {description}"));
    }

    let to_version = get_version(db)?;
    Ok(MigrationResult {
        from_version,
        to_version,
        applied,
    })
}

pub fn auto_migrate(db: &Database) -> anyhow::Result<()> {
    let result = run_migrations(db)?;
    if result.applied.is_empty() {
        tracing::info!("Schema at v{}, no migrations needed", result.to_version);
    } else {
        tracing::info!(
            "Migrated schema v{} -> v{} ({} migrations applied)",
            result.from_version,
            result.to_version,
            result.applied.len()
        );
    }
    Ok(())
}
