//! Startup selection between a saved world and the authored demo.
//!
//! `lynx-world serve` either resumes the last snapshot or rebuilds the demo.
//! The `--fresh` flag deliberately selects the authored world even when a
//! played snapshot exists, which is the supported way to reset a development
//! world to its pristine state. Keeping the choice here makes it testable
//! without binding a socket or spawning the binary.

use anyhow::{Context, Result};
use engine_core::faction::FactionWorld;
use engine_core::world_event::WorldCalendar;
use engine_sim::snapshot::QuarantinedRecord;
use engine_sim::zone::Zone;

use crate::auth::Grant;
use crate::demo;
use crate::persistence::{ContentMigrationCheckpoint, SnapshotStore, WorldSnapshot};

/// The aggregates a new gateway starts from.
#[derive(Debug)]
pub struct StartupWorld {
    /// Authoritative zone, either restored or newly authored.
    pub zone: Zone,
    /// Calendar aggregate paired with `factions`.
    pub calendar: WorldCalendar,
    /// Faction aggregate paired with `calendar`.
    pub factions: FactionWorld,
    /// Saved entries isolated during restore instead of being adopted.
    pub quarantined: Vec<QuarantinedRecord>,
    /// Latest migration checkpoint, retained so regular saves preserve rollback.
    pub migration_checkpoint: Option<Box<ContentMigrationCheckpoint>>,
}

/// Build the world the service starts on.
///
/// With `fresh`, the saved snapshot is ignored entirely and the authored demo
/// is rebuilt, so changes made by play never leak into the reset world.
/// Otherwise a readable snapshot is restored; when no snapshot exists (or no
/// store is configured) the authored demo is built.
///
/// # Errors
///
/// Returns an error when the snapshot cannot be read, when it fails restore
/// validation, or when the authored demo cannot be built.
pub fn load(
    store: Option<&dyn SnapshotStore>,
    fresh: bool,
    grants: &[Grant],
    now_ms: i64,
) -> Result<StartupWorld> {
    load_with_content_version(store, fresh, grants, now_ms, None)
}

/// Build the running world and require an exact content version and manifest
/// hash match before restoring saved entities.
pub fn load_with_content_identity(
    store: Option<&dyn SnapshotStore>,
    fresh: bool,
    grants: &[Grant],
    now_ms: i64,
    content_version: Option<&str>,
    content_manifest_sha256: Option<&str>,
) -> Result<StartupWorld> {
    load_world_with_content_identity(
        store,
        fresh,
        grants,
        now_ms,
        content_version,
        content_manifest_sha256,
    )
}

/// Build the running world and require any saved snapshot to match the
/// configured content version exactly. A version change needs a migration
/// path; this loader fails closed until that migration is available.
pub fn load_with_content_version(
    store: Option<&dyn SnapshotStore>,
    fresh: bool,
    grants: &[Grant],
    now_ms: i64,
    content_version: Option<&str>,
) -> Result<StartupWorld> {
    load_world_with_content_identity(store, fresh, grants, now_ms, content_version, None)
}

fn load_world_with_content_identity(
    store: Option<&dyn SnapshotStore>,
    fresh: bool,
    grants: &[Grant],
    now_ms: i64,
    content_version: Option<&str>,
    content_manifest_sha256: Option<&str>,
) -> Result<StartupWorld> {
    let saved = if fresh {
        None
    } else {
        match store {
            Some(store) => store
                .load()
                .context("failed to read the world snapshot file")?,
            None => None,
        }
    };

    if let Some(world) = saved.as_ref()
        && (world.content_version.as_deref() != content_version
            || world.content_manifest_sha256.as_deref() != content_manifest_sha256)
    {
        anyhow::bail!(
            "saved world content identity (version {:?}, manifest SHA-256 {:?}) does not match configured identity (version {:?}, manifest SHA-256 {:?}); migrate or reset the snapshot before startup",
            world.content_version,
            world.content_manifest_sha256,
            content_version,
            content_manifest_sha256,
        );
    }

    let (zone, mut quarantined) = match saved.as_ref() {
        Some(world) => {
            tracing::info!(
                saved_at_ms = world.saved_at_ms,
                entities = world.zone.entities.len(),
                "restoring the world from the snapshot file"
            );
            let report = demo::restore_zone_quarantining(&world.zone)
                .context("failed to restore the demo zone")?;
            for record in &report.quarantined {
                tracing::warn!(
                    section = %record.section,
                    entity = ?record.entity,
                    code = %record.code,
                    message = %record.message,
                    "saved entry quarantined during recovery"
                );
            }
            (report.zone, report.quarantined)
        }
        None => (
            demo::build_zone(grants).context("failed to build the demo zone")?,
            Vec::new(),
        ),
    };
    let migration_checkpoint = saved
        .as_ref()
        .and_then(|world| world.migration_checkpoint.clone());
    if let Some(checkpoint) = migration_checkpoint.as_deref() {
        quarantined.extend(checkpoint.quarantined.iter().cloned());
    }

    let (calendar, factions) = if let Some(WorldSnapshot {
        calendar: Some(calendar),
        factions: Some(factions),
        ..
    }) = saved.as_ref()
    {
        (
            WorldCalendar::from_snapshot(calendar.clone())
                .context("failed to restore the demo calendar")?,
            FactionWorld::from_snapshot(factions.clone())
                .context("failed to restore faction state")?,
        )
    } else {
        // `WorldSnapshot::validate` rejects an unpaired aggregate, so a
        // snapshot without a calendar is a zone-only snapshot: restore the
        // zone and author a fresh calendar for it.
        let mut factions =
            demo::build_faction_world(now_ms).context("failed to build the demo faction world")?;
        let calendar = demo::build_calendar(&mut factions, now_ms)
            .context("failed to build the demo calendar")?;
        (calendar, factions)
    };

    Ok(StartupWorld {
        zone,
        calendar,
        factions,
        quarantined,
        migration_checkpoint,
    })
}
