//! PostgreSQL persistence for the paired faction-world and calendar aggregates.
//!
//! After startup verifies them against snapshot replay, the normalized tables in
//! migrations 0004 and 0005 are the runtime source of truth.
//! History rows are append-only: replacements may append a suffix, but may
//! neither rewrite nor truncate an already persisted history.

use std::sync::Mutex;

use engine_core::{IdKind, StableId, faction::FactionSnapshot, world_event::CalendarSnapshot};
use game_api::{ApiError, CalendarStore, FactionStore, validate_calendar_snapshot};
use postgres::{Client, Config, NoTls, Transaction};
use serde_json::{Map, Value, json};

const SELECT_HELD_LEASE: &str = "\
SELECT 1 FROM lynx.zone_leases \
WHERE zone_id = $1 AND owner = $2 AND fencing_token = $3 \
    AND expires_at > clock_timestamp() \
FOR UPDATE";

#[derive(Debug, Clone)]
struct LeaseFence {
    zone_id: String,
    owner: String,
    token: i64,
}

/// PostgreSQL storage for the normalized, paired faction/calendar world state.
///
/// The store serializes access to its connection. When a zone lease is bound,
/// every replacement locks and validates that lease in the same transaction as
/// the normalized writes.
pub struct PostgresFactionCalendarStore {
    client: Mutex<Client>,
    fence: Mutex<Option<LeaseFence>>,
}

impl std::fmt::Debug for PostgresFactionCalendarStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresFactionCalendarStore")
            .finish_non_exhaustive()
    }
}

impl PostgresFactionCalendarStore {
    /// Wrap an established connection to a database with migrations 0004/0005.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client: Mutex::new(client),
            fence: Mutex::new(None),
        }
    }

    /// Connect using a PostgreSQL configuration.
    pub fn connect(config: &Config) -> Result<Self, ApiError> {
        let client = config.connect(NoTls).map_err(|_| unavailable())?;
        Ok(Self::new(client))
    }

    /// Bind subsequent replacement transactions to a held zone lease.
    ///
    /// The lease is checked under `FOR UPDATE` in the same transaction as each
    /// write. A stale owner/token or expired lease therefore fails closed.
    pub fn bind_zone_lease(&self, zone_id: &str, owner: &str, token: i64) -> Result<(), ApiError> {
        if !valid_lease_zone_id(zone_id) || owner.is_empty() || owner.len() > 256 || token <= 0 {
            return Err(ApiError::InvalidRequest(
                "zone lease binding is invalid".into(),
            ));
        }
        *self.fence.lock().map_err(|_| unavailable())? = Some(LeaseFence {
            zone_id: zone_id.to_owned(),
            owner: owner.to_owned(),
            token,
        });
        Ok(())
    }

    /// Load both normalized aggregates from one consistent database snapshot.
    ///
    /// `None` means no faction or calendar aggregate rows exist. A partially
    /// populated normalized state is an error rather than an empty snapshot.
    pub fn load_world_state(
        &self,
    ) -> Result<Option<(CalendarSnapshot, FactionSnapshot)>, ApiError> {
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let mut tx = client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .map_err(|_| unavailable())?;
        let state = Self::load_world_state_in_tx(&mut tx)?;
        tx.commit().map_err(|_| unavailable())?;
        Ok(state)
    }

    fn load_world_state_in_tx(
        tx: &mut Transaction<'_>,
    ) -> Result<Option<(CalendarSnapshot, FactionSnapshot)>, ApiError> {
        let has_factions: bool = tx
            .query_one("SELECT EXISTS (SELECT 1 FROM lynx.factions)", &[])
            .map_err(|_| unavailable())?
            .get(0);
        // Migrations seed the singleton rule row with TerritoryRules::default.
        // That bootstrap row alone is not an initialized aggregate. A changed
        // singleton without any represented faction/calendar rows is partial
        // state and must fail closed rather than being mistaken for empty.
        let has_nondefault_territory_rules: bool = tx
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM lynx.faction_world WHERE claim_enabled IS DISTINCT FROM TRUE OR capture_enabled IS DISTINCT FROM TRUE OR claim_protection_ms IS DISTINCT FROM 86400000 OR capture_cooldown_ms IS DISTINCT FROM 3600000 OR minimum_capture_power IS DISTINCT FROM 0 OR schema_version IS DISTINCT FROM 1)",
                &[],
            )
            .map_err(|_| unavailable())?
            .get(0);
        let has_territories: bool = tx
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM lynx.faction_territories)",
                &[],
            )
            .map_err(|_| unavailable())?
            .get(0);
        let has_events: bool = tx
            .query_one("SELECT EXISTS (SELECT 1 FROM lynx.world_events)", &[])
            .map_err(|_| unavailable())?
            .get(0);
        let has_snapshot_rows = has_factions || has_territories || has_events;
        if !has_snapshot_rows && !has_nondefault_territory_rules {
            return Ok(None);
        }

        decode_world_state(tx).map(Some)
    }

    /// Atomically validate and replace the paired aggregate state.
    pub fn replace_world_state(
        &self,
        calendar: &CalendarSnapshot,
        factions: &FactionSnapshot,
    ) -> Result<(), ApiError> {
        validate_calendar_snapshot(calendar, factions)?;
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        self.validate_fence(&mut tx)?;
        // The migrations use deferred constraint triggers for leader,
        // lifecycle, territory-history, transfer, and result consistency. A
        // paired replacement necessarily passes through temporarily
        // inconsistent intermediate states, so defer those checks until the
        // complete pair is present.
        tx.batch_execute("SET CONSTRAINTS ALL DEFERRED")
            .map_err(|_| unavailable())?;

        // Reconcile against the paired aggregate while holding this transaction
        // (and, when configured, its lease row lock). Never use a separately
        // loaded half-snapshot as the basis for a write.
        let stored = Self::load_world_state_in_tx(&mut tx)?;
        reconcile_world_state(&mut tx, stored.as_ref(), calendar, factions)?;
        tx.commit().map_err(pg_error)
    }

    /// Replace the complete normalized state after an explicit operator reset.
    ///
    /// Unlike [`Self::replace_world_state`], this is the sole path allowed to
    /// purge append-only rows. It is fenced, one transaction, and refuses to
    /// erase relation rows that this snapshot format cannot represent.
    pub fn reset_world_state(
        &self,
        calendar: &CalendarSnapshot,
        factions: &FactionSnapshot,
    ) -> Result<(), ApiError> {
        validate_calendar_snapshot(calendar, factions)?;
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        if self.fence.lock().map_err(|_| unavailable())?.is_none() {
            return Err(ApiError::Conflict(
                "a fenced zone lease is required for normalized fresh reset".into(),
            ));
        }
        self.validate_fence(&mut tx)?;
        let has_relations: bool = tx
            .query_one("SELECT EXISTS (SELECT 1 FROM lynx.faction_relations)", &[])
            .map_err(|_| unavailable())?
            .get(0);
        if has_relations {
            return Err(ApiError::Conflict(
                "cannot fresh-reset faction state while unrepresented faction relations exist"
                    .into(),
            ));
        }
        tx.batch_execute(
            "SET LOCAL lynx.allow_append_only_purge = 'on'; SET CONSTRAINTS ALL DEFERRED",
        )
        .map_err(|_| unavailable())?;
        tx.batch_execute("DELETE FROM lynx.world_event_result_sides; DELETE FROM lynx.world_event_results; DELETE FROM lynx.world_event_enrollments; DELETE FROM lynx.world_event_transitions; DELETE FROM lynx.world_event_sides; DELETE FROM lynx.world_events; DELETE FROM lynx.faction_territory_history; DELETE FROM lynx.faction_treasury_entries; DELETE FROM lynx.faction_treasury_transfers; DELETE FROM lynx.faction_territories; DELETE FROM lynx.faction_members; DELETE FROM lynx.faction_role_permissions; DELETE FROM lynx.faction_spend_limits; DELETE FROM lynx.factions")
            .map_err(|_| unavailable())?;
        reconcile_world_state(&mut tx, None, calendar, factions)?;
        tx.commit().map_err(pg_error)
    }

    /// Compare a supplied paired snapshot with the current normalized state.
    pub fn verify_world_state(
        &self,
        calendar: &CalendarSnapshot,
        factions: &FactionSnapshot,
    ) -> Result<bool, ApiError> {
        validate_calendar_snapshot(calendar, factions)?;
        match self.load_world_state()? {
            None => {
                Ok(calendar == &CalendarSnapshot::default()
                    && factions == &FactionSnapshot::default())
            }
            Some((stored_calendar, stored_factions)) => {
                Ok(stored_calendar == *calendar && stored_factions == *factions)
            }
        }
    }

    fn validate_fence(&self, tx: &mut Transaction<'_>) -> Result<(), ApiError> {
        let fence = self
            .fence
            .lock()
            .map_err(|_| unavailable())?
            .clone()
            .ok_or_else(|| {
                ApiError::Conflict("a zone lease must be bound before normalized writes".into())
            })?;
        if tx
            .query_opt(
                SELECT_HELD_LEASE,
                &[&fence.zone_id, &fence.owner, &fence.token],
            )
            .map_err(|_| unavailable())?
            .is_none()
        {
            return Err(ApiError::Conflict("zone lease was lost".into()));
        }
        Ok(())
    }
}

impl FactionStore for PostgresFactionCalendarStore {
    fn load_faction_world(&self) -> Result<FactionSnapshot, ApiError> {
        self.load_world_state()?
            .map(|(_, factions)| factions)
            .ok_or_else(|| ApiError::NotFound("faction world not found".into()))
    }

    fn replace_faction_world(&self, snapshot: &FactionSnapshot) -> Result<(), ApiError> {
        snapshot
            .validate()
            .map_err(|_| ApiError::InvalidRequest("faction snapshot is invalid".into()))?;
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        self.validate_fence(&mut tx)?;
        tx.batch_execute("SET CONSTRAINTS ALL DEFERRED")
            .map_err(|_| unavailable())?;
        let Some((calendar, stored_factions)) = Self::load_world_state_in_tx(&mut tx)? else {
            return Err(ApiError::Conflict(
                "cannot replace one aggregate without its paired state".into(),
            ));
        };
        validate_calendar_snapshot(&calendar, snapshot)?;
        reconcile_world_state(
            &mut tx,
            Some(&(calendar.clone(), stored_factions)),
            &calendar,
            snapshot,
        )?;
        tx.commit().map_err(pg_error)
    }
}

impl CalendarStore for PostgresFactionCalendarStore {
    fn load_calendar(&self) -> Result<CalendarSnapshot, ApiError> {
        self.load_world_state()?
            .map(|(calendar, _)| calendar)
            .ok_or_else(|| ApiError::NotFound("calendar not found".into()))
    }

    fn replace_calendar(&self, snapshot: &CalendarSnapshot) -> Result<(), ApiError> {
        snapshot
            .validate()
            .map_err(|_| ApiError::InvalidRequest("calendar snapshot is invalid".into()))?;
        let mut client = self.client.lock().map_err(|_| unavailable())?;
        let mut tx = client.transaction().map_err(|_| unavailable())?;
        self.validate_fence(&mut tx)?;
        tx.batch_execute("SET CONSTRAINTS ALL DEFERRED")
            .map_err(|_| unavailable())?;
        let Some((stored_calendar, factions)) = Self::load_world_state_in_tx(&mut tx)? else {
            return Err(ApiError::Conflict(
                "cannot replace one aggregate without its paired state".into(),
            ));
        };
        validate_calendar_snapshot(snapshot, &factions)?;
        reconcile_world_state(
            &mut tx,
            Some(&(stored_calendar, factions.clone())),
            snapshot,
            &factions,
        )?;
        tx.commit().map_err(pg_error)
    }
}

fn valid_lease_zone_id(value: &str) -> bool {
    StableId::parse(value).is_ok_and(|id| id.kind() == IdKind::Zone)
}

/// Decode every represented row.  We deliberately build the serde wire shape
/// here rather than duplicating engine validation rules; deserialization is
/// followed by the public aggregate validator below.
fn decode_world_state(
    tx: &mut Transaction<'_>,
) -> Result<(CalendarSnapshot, FactionSnapshot), ApiError> {
    let rule = tx.query_one(
        "SELECT claim_enabled, capture_enabled, claim_protection_ms, capture_cooldown_ms, minimum_capture_power::text, schema_version FROM lynx.faction_world WHERE singleton",
        &[],
    ).map_err(|_| unavailable())?;
    let rule_version: i32 = rule.get(5);
    require_major(rule_version)?;
    let territory_rules = json!({
        "claim_enabled": rule.get::<_, bool>(0),
        "capture_enabled": rule.get::<_, bool>(1),
        "claim_protection": nonnegative_u64(rule.get::<_, i64>(2))?,
        "capture_cooldown": nonnegative_u64(rule.get::<_, i64>(3))?,
        "minimum_capture_power": numeric_u64(&rule.get::<_, String>(4))?,
    });

    let mut factions = Vec::new();
    for row in tx.query(
        "SELECT faction_id, display_name, description, controller, max_members, content_version_major, content_version_minor, power_version_major, power_version_minor, power_base::text, power_treasury_weight_bps, power_reserve::text, power_cap::text, schema_version FROM lynx.factions ORDER BY faction_id",
        &[],
    ).map_err(|_| unavailable())? {
        let faction_id: String = row.get(0);
        let schema_version: i32 = row.get(13);
        require_major(schema_version)?;
        let mut permissions = Map::new();
        for permission in tx.query("SELECT role, permission FROM lynx.faction_role_permissions WHERE faction_id = $1 ORDER BY role, permission", &[&faction_id]).map_err(|_| unavailable())? {
            let permission_role: String = permission.get(0);
            let item: String = permission.get(1);
            permissions.entry(permission_role).or_insert_with(|| Value::Array(Vec::new())).as_array_mut().expect("array").push(Value::String(item));
        }
        let mut spend_limits = Map::new();
        for limit in tx.query("SELECT role, max_amount::text FROM lynx.faction_spend_limits WHERE faction_id = $1 ORDER BY role", &[&faction_id]).map_err(|_| unavailable())? {
            spend_limits.insert(limit.get::<_, String>(0), json!(numeric_u64(&limit.get::<_, String>(1))?));
        }
        let mut members = Map::new();
        for member in tx.query("SELECT member_id, role, round(extract(epoch FROM joined_at) * 1000)::bigint, schema_version FROM lynx.faction_members WHERE faction_id = $1 ORDER BY member_id", &[&faction_id]).map_err(|_| unavailable())? {
            let version: i32 = member.get(3);
            require_major(version)?;
            let id: String = member.get(0);
            members.insert(id.clone(), json!({"id": id, "role": member.get::<_, String>(1), "joined_at": member.get::<_, i64>(2)}));
        }
        let mut entries = Vec::new();
        let mut balance: i128 = 0;
        for entry in tx.query("SELECT entry_key, kind, amount::text, counterparty_faction_id, reason, round(extract(epoch FROM occurred_at) * 1000)::bigint, schema_version FROM lynx.faction_treasury_entries WHERE faction_id = $1 ORDER BY entry_id", &[&faction_id]).map_err(|_| unavailable())? {
            let version: i32 = entry.get(6);
            require_major(version)?;
            let kind: String = entry.get(1);
            let amount = numeric_u64(&entry.get::<_, String>(2))?;
            match kind.as_str() { "deposit" | "reward" | "transfer_in" => balance += i128::from(amount), "spend" | "loss" | "transfer_out" => balance -= i128::from(amount), _ => return Err(unavailable()) }
            entries.push(json!({"key": entry.get::<_, String>(0), "kind": kind, "amount": amount, "counterparty": entry.get::<_, Option<String>>(3), "reason": entry.get::<_, String>(4), "at": entry.get::<_, i64>(5), "schema_version": version_json(version)}));
        }
        let balance = u64::try_from(balance).map_err(|_| unavailable())?;
        factions.push(json!({
            "definition": {"id": faction_id, "display_name": row.get::<_, String>(1), "version": {"major": row.get::<_, i32>(5), "minor": row.get::<_, i32>(6)}, "description": row.get::<_, Option<String>>(2)},
            "governance": {"controller": row.get::<_, String>(3), "role_permissions": permissions, "spend_limits": spend_limits, "max_members": nonnegative_usize(row.get::<_, i32>(4))?},
            "power_rules": {"version": {"major": row.get::<_, i32>(7), "minor": row.get::<_, i32>(8)}, "base_power": numeric_u64(&row.get::<_, String>(9))?, "treasury_weight_bps": nonnegative_u16(row.get::<_, i32>(10))?, "reserve": numeric_u64(&row.get::<_, String>(11))?, "cap": numeric_u64(&row.get::<_, String>(12))?},
            "treasury": {"balance": balance, "entries": entries},
            "membership": {"members": members},
            "schema_version": version_json(schema_version)
        }));
    }
    let mut territories = Vec::new();
    for row in tx.query("SELECT zone_id, owner_faction_id, contested, round(extract(epoch FROM claimed_at) * 1000)::bigint, round(extract(epoch FROM protection_until) * 1000)::bigint, round(extract(epoch FROM cooldown_until) * 1000)::bigint, revision, schema_version FROM lynx.faction_territories ORDER BY zone_id", &[]).map_err(|_| unavailable())? {
        let version: i32 = row.get(7); require_major(version)?;
        territories.push(json!({"zone": row.get::<_, String>(0), "owner": row.get::<_, Option<String>>(1), "contested": row.get::<_, bool>(2), "claimed_at": row.get::<_, Option<i64>>(3), "protection_until": row.get::<_, Option<i64>>(4), "cooldown_until": row.get::<_, Option<i64>>(5), "revision": nonnegative_u64(row.get::<_, i64>(6))?, "schema_version": version_json(version)}));
    }
    let territory_history = decode_territory_history(tx)?;
    let transfers = decode_transfers(tx)?;
    let factions_value = json!({"schema_version": version_json(1), "territory_rules": territory_rules, "factions": factions, "territories": territories, "territory_history": territory_history, "transfers": transfers});
    let factions: FactionSnapshot =
        serde_json::from_value(factions_value).map_err(|_| unavailable())?;

    let mut events = Vec::new();
    for row in tx.query("SELECT event_id, title, description, round(extract(epoch FROM enrollment_opens_at) * 1000)::bigint, round(extract(epoch FROM enrollment_closes_at) * 1000)::bigint, round(extract(epoch FROM starts_at) * 1000)::bigint, round(extract(epoch FROM ends_at) * 1000)::bigint, target_zone_id, status, enrollment_requires_faction_membership, enrollment_allow_unaffiliated, enrollment_max_participants_per_side, conflict_rule_version_major, conflict_rule_version_minor, conflict_metric, conflict_minimum_participants_per_side, cancellation_policy, schema_version FROM lynx.world_events ORDER BY event_id", &[]).map_err(|_| unavailable())? {
        let event_id: String = row.get(0); let version: i32 = row.get(17); require_major(version)?;
        let mut sides = Vec::new();
        for side in tx.query("SELECT side_id, role, display_name, faction_id FROM lynx.world_event_sides WHERE event_id = $1 ORDER BY side_id", &[&event_id]).map_err(|_| unavailable())? { sides.push(json!({"id": side.get::<_, String>(0), "role": side.get::<_, String>(1), "display_name": side.get::<_, String>(2), "faction": side.get::<_, String>(3)})); }
        events.push(json!({"id": event_id, "title": row.get::<_, String>(1), "description": row.get::<_, Option<String>>(2), "schedule": {"enrollment_opens_at": row.get::<_, i64>(3), "enrollment_closes_at": row.get::<_, i64>(4), "starts_at": row.get::<_, i64>(5), "ends_at": row.get::<_, i64>(6)}, "target_zone": row.get::<_, String>(7), "sides": sides, "enrollment": {"requires_faction_membership": row.get::<_, bool>(9), "allow_unaffiliated": row.get::<_, bool>(10), "max_participants_per_side": row.get::<_, Option<i32>>(11)}, "conflict": {"version": {"major": row.get::<_, i32>(12), "minor": row.get::<_, i32>(13)}, "metric": row.get::<_, String>(14), "minimum_participants_per_side": nonnegative_u32(row.get::<_, i32>(15))?}, "cancellation": row.get::<_, String>(16), "status": row.get::<_, String>(8), "schema_version": version_json(version)}));
    }
    let calendar_value = json!({"schema_version": version_json(1), "events": events, "transitions": decode_transitions(tx)?, "enrollments": decode_enrollments(tx)?, "resolutions": decode_resolutions(tx, &territory_history)?});
    let calendar: CalendarSnapshot =
        serde_json::from_value(calendar_value).map_err(|_| unavailable())?;
    validate_calendar_snapshot(&calendar, &factions)?;
    Ok((calendar, factions))
}

fn version_json(major: i32) -> Value {
    json!({"major": major, "minor": 0})
}
fn require_major(major: i32) -> Result<(), ApiError> {
    if major == 1 {
        Ok(())
    } else {
        Err(unavailable())
    }
}
fn numeric_u64(value: &str) -> Result<u64, ApiError> {
    value.parse().map_err(|_| unavailable())
}
fn nonnegative_u64(value: i64) -> Result<u64, ApiError> {
    u64::try_from(value).map_err(|_| unavailable())
}
fn nonnegative_u32(value: i32) -> Result<u32, ApiError> {
    u32::try_from(value).map_err(|_| unavailable())
}
fn nonnegative_u16(value: i32) -> Result<u16, ApiError> {
    u16::try_from(value).map_err(|_| unavailable())
}
fn nonnegative_usize(value: i32) -> Result<usize, ApiError> {
    usize::try_from(value).map_err(|_| unavailable())
}

fn decode_territory_history(tx: &mut Transaction<'_>) -> Result<Vec<Value>, ApiError> {
    let mut values = Vec::new();
    for row in tx.query("SELECT change_key, zone_id, action, from_faction_id, to_faction_id, actor_id, round(extract(epoch FROM occurred_at) * 1000)::bigint, revision, schema_version FROM lynx.faction_territory_history ORDER BY change_id", &[]).map_err(|_| unavailable())? {
        let version: i32 = row.get(8); require_major(version)?;
        values.push(json!({"key": row.get::<_, String>(0), "zone": row.get::<_, String>(1), "action": row.get::<_, String>(2), "from": row.get::<_, Option<String>>(3), "to": row.get::<_, Option<String>>(4), "actor": row.get::<_, String>(5), "at": row.get::<_, i64>(6), "revision": nonnegative_u64(row.get::<_, i64>(7))?, "schema_version": version_json(version)}));
    }
    Ok(values)
}

fn decode_transfers(tx: &mut Transaction<'_>) -> Result<Vec<Value>, ApiError> {
    let mut values = Vec::new();
    for row in tx.query("SELECT transfer_key, source_faction_id, target_faction_id, amount::text, round(extract(epoch FROM occurred_at) * 1000)::bigint, schema_version FROM lynx.faction_treasury_transfers ORDER BY transfer_key", &[]).map_err(|_| unavailable())? {
        let version: i32 = row.get(5); require_major(version)?;
        values.push(json!({"key": row.get::<_, String>(0), "source": row.get::<_, String>(1), "target": row.get::<_, String>(2), "amount": numeric_u64(&row.get::<_, String>(3))?, "at": row.get::<_, i64>(4), "schema_version": version_json(version)}));
    }
    Ok(values)
}

fn decode_transitions(tx: &mut Transaction<'_>) -> Result<Vec<Value>, ApiError> {
    let mut values = Vec::new();
    for row in tx.query("SELECT transition_key, event_id, action, from_status, to_status, actor_id, round(extract(epoch FROM occurred_at) * 1000)::bigint, reason, schema_version FROM lynx.world_event_transitions ORDER BY transition_id", &[]).map_err(|_| unavailable())? {
        let version: i32 = row.get(8); require_major(version)?;
        values.push(json!({"key": row.get::<_, String>(0), "event": row.get::<_, String>(1), "action": row.get::<_, String>(2), "from": row.get::<_, Option<String>>(3), "to": row.get::<_, String>(4), "actor": row.get::<_, String>(5), "at": row.get::<_, i64>(6), "reason": row.get::<_, Option<String>>(7), "schema_version": version_json(version)}));
    }
    Ok(values)
}

fn decode_enrollments(tx: &mut Transaction<'_>) -> Result<Vec<Value>, ApiError> {
    let mut values = Vec::new();
    for row in tx.query("SELECT event_id, participant_id, side_id, faction_id, round(extract(epoch FROM enrolled_at) * 1000)::bigint, schema_version FROM lynx.world_event_enrollments ORDER BY event_id, participant_id", &[]).map_err(|_| unavailable())? {
        let version: i32 = row.get(5); require_major(version)?;
        values.push(json!({"event": row.get::<_, String>(0), "participant": row.get::<_, String>(1), "side": row.get::<_, String>(2), "faction": row.get::<_, Option<String>>(3), "at": row.get::<_, i64>(4), "schema_version": version_json(version)}));
    }
    Ok(values)
}

fn decode_resolutions(tx: &mut Transaction<'_>, history: &[Value]) -> Result<Vec<Value>, ApiError> {
    let mut values = Vec::new();
    for row in tx.query("SELECT event_id, resolver_id, round(extract(epoch FROM resolved_at) * 1000)::bigint, rule_version_major, rule_version_minor, metric, minimum_participants_per_side, winning_side_id, winning_faction_id, outcome, territory_change_key, schema_version FROM lynx.world_event_results ORDER BY event_id", &[]).map_err(|_| unavailable())? {
        let event: String = row.get(0); let version: i32 = row.get(11); require_major(version)?;
        let territory_key: Option<String> = row.get(10);
        let territory_change = territory_key.as_ref().map(|key| history.iter().find(|value| value.get("key") == Some(&Value::String(key.clone()))).cloned().ok_or_else(unavailable)).transpose()?;
        let mut sides = Vec::new();
        for side in tx.query("SELECT side_id, role, faction_id, participants, faction_power::text, score::text FROM lynx.world_event_result_sides WHERE event_id = $1 ORDER BY side_id", &[&event]).map_err(|_| unavailable())? {
            sides.push(json!({"side": side.get::<_, String>(0), "role": side.get::<_, String>(1), "faction": side.get::<_, String>(2), "participants": nonnegative_u32(side.get::<_, i32>(3))?, "faction_power": numeric_u64(&side.get::<_, String>(4))?, "score": numeric_u64(&side.get::<_, String>(5))?}));
        }
        values.push(json!({"event": event, "resolver": row.get::<_, String>(1), "resolved_at": row.get::<_, i64>(2), "rule": {"version": {"major": row.get::<_, i32>(3), "minor": row.get::<_, i32>(4)}, "metric": row.get::<_, String>(5), "minimum_participants_per_side": nonnegative_u32(row.get::<_, i32>(6))?}, "winning_side": row.get::<_, String>(7), "winning_faction": row.get::<_, String>(8), "outcome": row.get::<_, String>(9), "sides": sides, "territory_change": territory_change, "schema_version": version_json(version)}));
    }
    Ok(values)
}

fn reconcile_world_state(
    tx: &mut Transaction<'_>,
    stored: Option<&(CalendarSnapshot, FactionSnapshot)>,
    calendar: &CalendarSnapshot,
    factions: &FactionSnapshot,
) -> Result<(), ApiError> {
    validate_schema_versions(calendar, factions)?;
    if let Some((old_calendar, old_factions)) = stored {
        ensure_core_rows_retained(old_calendar, old_factions, calendar, factions)?;
        ensure_history_prefix(old_calendar, old_factions, calendar, factions)?;
    }
    write_factions(tx, factions)?;
    write_territory_rules(tx, factions)?;
    write_territories(tx, factions)?;
    write_events(tx, calendar)?;
    write_transfers(tx, stored.map(|(_, factions)| factions), factions)?;
    write_treasury_entries(tx, stored.map(|(_, factions)| factions), factions)?;
    write_territory_history(tx, stored.map(|(_, factions)| factions), factions)?;
    write_transitions(tx, stored.map(|(calendar, _)| calendar), calendar)?;
    write_enrollments(tx, stored.map(|(calendar, _)| calendar), calendar)?;
    write_resolutions(tx, stored.map(|(calendar, _)| calendar), calendar)?;
    Ok(())
}

fn validate_schema_versions(
    calendar: &CalendarSnapshot,
    factions: &FactionSnapshot,
) -> Result<(), ApiError> {
    let supported =
        |version: engine_core::schema::SchemaVersion| version.major == 1 && version.minor == 0;
    if !supported(calendar.schema_version) || !supported(factions.schema_version) {
        return Err(ApiError::InvalidRequest(
            "normalized schema only supports version 1.0".into(),
        ));
    }
    if factions.factions.iter().any(|f| {
        !supported(f.schema_version)
            || f.treasury
                .entries()
                .iter()
                .any(|e| !supported(e.schema_version))
    }) {
        return Err(ApiError::InvalidRequest(
            "normalized schema only supports version 1.0".into(),
        ));
    }
    if factions
        .territories
        .iter()
        .any(|t| !supported(t.schema_version))
        || factions
            .territory_history
            .iter()
            .any(|h| !supported(h.schema_version))
        || factions
            .transfers
            .iter()
            .any(|t| !supported(t.schema_version))
        || calendar.events.iter().any(|e| !supported(e.schema_version))
        || calendar
            .transitions
            .iter()
            .any(|t| !supported(t.schema_version))
        || calendar
            .enrollments
            .iter()
            .any(|e| !supported(e.schema_version))
        || calendar
            .resolutions
            .iter()
            .any(|r| !supported(r.schema_version))
    {
        return Err(ApiError::InvalidRequest(
            "normalized schema only supports version 1.0".into(),
        ));
    }
    if calendar
        .events
        .iter()
        .any(|event| event.sides.windows(2).any(|pair| pair[0].id >= pair[1].id))
    {
        return Err(ApiError::InvalidRequest(
            "event sides must be ordered by stable ID for normalized persistence".into(),
        ));
    }
    Ok(())
}

fn ensure_core_rows_retained(
    old_calendar: &CalendarSnapshot,
    old_factions: &FactionSnapshot,
    calendar: &CalendarSnapshot,
    factions: &FactionSnapshot,
) -> Result<(), ApiError> {
    let incoming_factions: std::collections::BTreeSet<_> =
        factions.factions.iter().map(|f| &f.definition.id).collect();
    let incoming_territories: std::collections::BTreeSet<_> =
        factions.territories.iter().map(|t| &t.zone).collect();
    let incoming_events: std::collections::BTreeSet<_> =
        calendar.events.iter().map(|e| &e.id).collect();
    if old_factions
        .factions
        .iter()
        .any(|f| !incoming_factions.contains(&f.definition.id))
        || old_factions
            .territories
            .iter()
            .any(|t| !incoming_territories.contains(&t.zone))
        || old_calendar
            .events
            .iter()
            .any(|e| !incoming_events.contains(&e.id))
    {
        return Err(ApiError::Conflict("ordinary normalized replacement cannot remove a faction, territory, or event; use an explicit fenced reset".into()));
    }
    for old in &old_calendar.events {
        let replacement = calendar
            .events
            .iter()
            .find(|event| event.id == old.id)
            .ok_or_else(|| ApiError::Conflict("event missing".into()))?;
        let old_sides: std::collections::BTreeMap<_, _> =
            old.sides.iter().map(|side| (&side.id, side)).collect();
        let new_sides: std::collections::BTreeMap<_, _> = replacement
            .sides
            .iter()
            .map(|side| (&side.id, side))
            .collect();
        if old_sides != new_sides {
            return Err(ApiError::Conflict(
                "event sides are immutable after creation".into(),
            ));
        }
    }
    Ok(())
}

fn ensure_prefix<T: PartialEq>(
    existing: &[T],
    replacement: &[T],
    name: &str,
) -> Result<(), ApiError> {
    if existing.len() > replacement.len() || !existing.iter().zip(replacement).all(|(a, b)| a == b)
    {
        return Err(ApiError::Conflict(format!(
            "{name} is append-only and the replacement rewrites or truncates history"
        )));
    }
    Ok(())
}

fn ensure_history_prefix(
    old_calendar: &CalendarSnapshot,
    old_factions: &FactionSnapshot,
    calendar: &CalendarSnapshot,
    factions: &FactionSnapshot,
) -> Result<(), ApiError> {
    ensure_prefix(
        &old_factions.territory_history,
        &factions.territory_history,
        "territory history",
    )?;
    ensure_prefix(
        &old_calendar.transitions,
        &calendar.transitions,
        "event transitions",
    )?;
    let old_transfers: std::collections::BTreeMap<_, _> = old_factions
        .transfers
        .iter()
        .map(|value| (&value.key, value))
        .collect();
    let transfers: std::collections::BTreeMap<_, _> = factions
        .transfers
        .iter()
        .map(|value| (&value.key, value))
        .collect();
    if old_transfers
        .iter()
        .any(|(key, value)| transfers.get(key) != Some(value))
    {
        return Err(ApiError::Conflict(
            "treasury transfers are append-only and cannot be changed or removed".into(),
        ));
    }
    for old in &old_factions.factions {
        let replacement = factions
            .factions
            .iter()
            .find(|f| f.definition.id == old.definition.id)
            .ok_or_else(|| ApiError::Conflict("faction missing".into()))?;
        ensure_prefix(
            old.treasury.entries(),
            replacement.treasury.entries(),
            "treasury ledger",
        )?;
    }
    let old_enrollments: std::collections::BTreeMap<_, _> = old_calendar
        .enrollments
        .iter()
        .map(|value| ((&value.event, value.participant), value))
        .collect();
    let enrollments: std::collections::BTreeMap<_, _> = calendar
        .enrollments
        .iter()
        .map(|value| ((&value.event, value.participant), value))
        .collect();
    if old_enrollments
        .iter()
        .any(|(key, value)| enrollments.get(key) != Some(value))
    {
        return Err(ApiError::Conflict(
            "event enrollments are append-only and cannot be changed or removed".into(),
        ));
    }
    let old_results: std::collections::BTreeMap<_, _> = old_calendar
        .resolutions
        .iter()
        .map(|value| (&value.event, value))
        .collect();
    let results: std::collections::BTreeMap<_, _> = calendar
        .resolutions
        .iter()
        .map(|value| (&value.event, value))
        .collect();
    if old_results
        .iter()
        .any(|(key, value)| results.get(key) != Some(value))
    {
        return Err(ApiError::Conflict(
            "event resolutions are append-only and cannot be changed or removed".into(),
        ));
    }
    Ok(())
}

fn write_factions(tx: &mut Transaction<'_>, snapshot: &FactionSnapshot) -> Result<(), ApiError> {
    for faction in &snapshot.factions {
        let id = faction.definition.id.to_string();
        let base = faction.power_rules.base_power.to_string();
        let reserve = faction.power_rules.reserve.to_string();
        let cap = faction.power_rules.cap.to_string();
        let max_members = i32::try_from(faction.governance.max_members).map_err(pg_error)?;
        let weight = i32::from(faction.power_rules.treasury_weight_bps);
        let content_major = i32::from(faction.definition.version.major);
        let content_minor = i32::from(faction.definition.version.minor);
        let power_major = i32::from(faction.power_rules.version.major);
        let power_minor = i32::from(faction.power_rules.version.minor);
        let schema_major = i32::from(faction.schema_version.major);
        tx.execute("INSERT INTO lynx.factions (faction_id, display_name, description, controller, max_members, content_version_major, content_version_minor, power_version_major, power_version_minor, power_base, power_treasury_weight_bps, power_reserve, power_cap, schema_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::text::numeric,$11,$12::text::numeric,$13::text::numeric,$14) ON CONFLICT (faction_id) DO UPDATE SET display_name=EXCLUDED.display_name, description=EXCLUDED.description, controller=EXCLUDED.controller, max_members=EXCLUDED.max_members, content_version_major=EXCLUDED.content_version_major, content_version_minor=EXCLUDED.content_version_minor, power_version_major=EXCLUDED.power_version_major, power_version_minor=EXCLUDED.power_version_minor, power_base=EXCLUDED.power_base, power_treasury_weight_bps=EXCLUDED.power_treasury_weight_bps, power_reserve=EXCLUDED.power_reserve, power_cap=EXCLUDED.power_cap, schema_version=EXCLUDED.schema_version", &[&id, &faction.definition.display_name, &faction.definition.description, &faction.governance.controller.as_str(), &max_members, &content_major, &content_minor, &power_major, &power_minor, &base, &weight, &reserve, &cap, &schema_major]).map_err(pg_error)?;
        tx.execute(
            "DELETE FROM lynx.faction_role_permissions WHERE faction_id = $1",
            &[&id],
        )
        .map_err(pg_error)?;
        for (role, permissions) in &faction.governance.role_permissions {
            for permission in permissions {
                tx.execute("INSERT INTO lynx.faction_role_permissions (faction_id, role, permission) VALUES ($1,$2,$3)", &[&id, &role.as_str(), &permission.as_str()]).map_err(pg_error)?;
            }
        }
        tx.execute(
            "DELETE FROM lynx.faction_spend_limits WHERE faction_id = $1",
            &[&id],
        )
        .map_err(pg_error)?;
        for (role, limit) in &faction.governance.spend_limits {
            let limit = limit.to_string();
            tx.execute("INSERT INTO lynx.faction_spend_limits (faction_id, role, max_amount) VALUES ($1,$2,$3::text::numeric)", &[&id, &role.as_str(), &limit]).map_err(pg_error)?;
        }
    }
    // A member can move between factions in one valid aggregate replacement.
    // Clear every replaceable roster before reinserting, rather than depending
    // on faction-ID order to happen to remove the old row first.
    tx.execute("DELETE FROM lynx.faction_members", &[])
        .map_err(pg_error)?;
    for faction in &snapshot.factions {
        let id = faction.definition.id.to_string();
        for member in faction.membership.iter() {
            let member_id = member.id.to_string();
            tx.execute("INSERT INTO lynx.faction_members (faction_id, member_id, role, joined_at, schema_version) VALUES ($1,$2,$3,to_timestamp(($4::bigint / 1000.0)::double precision),1)", &[&id, &member_id, &member.role.as_str(), &member.joined_at.value()]).map_err(pg_error)?;
        }
    }
    Ok(())
}

fn write_territory_rules(
    tx: &mut Transaction<'_>,
    snapshot: &FactionSnapshot,
) -> Result<(), ApiError> {
    let rules = &snapshot.territory_rules;
    let claim = i64::try_from(rules.claim_protection.0).map_err(pg_error)?;
    let cooldown = i64::try_from(rules.capture_cooldown.0).map_err(pg_error)?;
    let power = rules.minimum_capture_power.to_string();
    tx.execute("UPDATE lynx.faction_world SET claim_enabled=$1, capture_enabled=$2, claim_protection_ms=$3, capture_cooldown_ms=$4, minimum_capture_power=$5::text::numeric, schema_version=1 WHERE singleton", &[&rules.claim_enabled, &rules.capture_enabled, &claim, &cooldown, &power]).map_err(pg_error)?;
    Ok(())
}

fn write_territories(tx: &mut Transaction<'_>, snapshot: &FactionSnapshot) -> Result<(), ApiError> {
    for territory in &snapshot.territories {
        let zone = territory.zone.to_string();
        let owner = territory.owner.as_ref().map(ToString::to_string);
        let revision = i64::try_from(territory.revision).map_err(pg_error)?;
        let claimed = territory.claimed_at.map(engine_core::time::Millis::value);
        let protection = territory
            .protection_until
            .map(engine_core::time::Millis::value);
        let cooldown = territory
            .cooldown_until
            .map(engine_core::time::Millis::value);
        tx.execute("INSERT INTO lynx.faction_territories (zone_id, owner_faction_id, contested, claimed_at, protection_until, cooldown_until, revision, schema_version) VALUES ($1,$2,$3,to_timestamp(($4::bigint / 1000.0)::double precision),to_timestamp(($5::bigint / 1000.0)::double precision),to_timestamp(($6::bigint / 1000.0)::double precision),$7,1) ON CONFLICT (zone_id) DO UPDATE SET owner_faction_id=EXCLUDED.owner_faction_id, contested=EXCLUDED.contested, claimed_at=EXCLUDED.claimed_at, protection_until=EXCLUDED.protection_until, cooldown_until=EXCLUDED.cooldown_until, revision=EXCLUDED.revision, schema_version=EXCLUDED.schema_version", &[&zone, &owner, &territory.contested, &claimed, &protection, &cooldown, &revision]).map_err(pg_error)?;
    }
    Ok(())
}

fn write_events(tx: &mut Transaction<'_>, snapshot: &CalendarSnapshot) -> Result<(), ApiError> {
    for event in &snapshot.events {
        let id = event.id.to_string();
        let zone = event.target_zone.to_string();
        let max = event
            .enrollment
            .max_participants_per_side
            .map(|value| i32::try_from(value).map_err(|_| unavailable()))
            .transpose()?;
        let minimum =
            i32::try_from(event.conflict.minimum_participants_per_side).map_err(pg_error)?;
        let rule_major = i32::from(event.conflict.version.major);
        let rule_minor = i32::from(event.conflict.version.minor);
        tx.execute("INSERT INTO lynx.world_events (event_id,title,description,enrollment_opens_at,enrollment_closes_at,starts_at,ends_at,target_zone_id,status,enrollment_requires_faction_membership,enrollment_allow_unaffiliated,enrollment_max_participants_per_side,conflict_rule_version_major,conflict_rule_version_minor,conflict_metric,conflict_minimum_participants_per_side,cancellation_policy,schema_version) VALUES ($1,$2,$3,to_timestamp(($4::bigint / 1000.0)::double precision),to_timestamp(($5::bigint / 1000.0)::double precision),to_timestamp(($6::bigint / 1000.0)::double precision),to_timestamp(($7::bigint / 1000.0)::double precision),$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,1) ON CONFLICT (event_id) DO UPDATE SET title=EXCLUDED.title,description=EXCLUDED.description,enrollment_opens_at=EXCLUDED.enrollment_opens_at,enrollment_closes_at=EXCLUDED.enrollment_closes_at,starts_at=EXCLUDED.starts_at,ends_at=EXCLUDED.ends_at,target_zone_id=EXCLUDED.target_zone_id,status=EXCLUDED.status,enrollment_requires_faction_membership=EXCLUDED.enrollment_requires_faction_membership,enrollment_allow_unaffiliated=EXCLUDED.enrollment_allow_unaffiliated,enrollment_max_participants_per_side=EXCLUDED.enrollment_max_participants_per_side,conflict_rule_version_major=EXCLUDED.conflict_rule_version_major,conflict_rule_version_minor=EXCLUDED.conflict_rule_version_minor,conflict_metric=EXCLUDED.conflict_metric,conflict_minimum_participants_per_side=EXCLUDED.conflict_minimum_participants_per_side,cancellation_policy=EXCLUDED.cancellation_policy,schema_version=1", &[&id,&event.title,&event.description,&event.schedule.enrollment_opens_at.value(),&event.schedule.enrollment_closes_at.value(),&event.schedule.starts_at.value(),&event.schedule.ends_at.value(),&zone,&event.status.as_str(),&event.enrollment.requires_faction_membership,&event.enrollment.allow_unaffiliated,&max,&rule_major,&rule_minor,&event.conflict.metric.as_str(),&minimum,&event.cancellation.as_str()]).map_err(pg_error)?;
        for side in &event.sides {
            let side_id = side.id.to_string();
            let faction = side.faction.to_string();
            tx.execute("INSERT INTO lynx.world_event_sides (event_id,side_id,role,display_name,faction_id) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (event_id,side_id) DO UPDATE SET role=EXCLUDED.role, display_name=EXCLUDED.display_name, faction_id=EXCLUDED.faction_id", &[&id,&side_id,&side.role.as_str(),&side.display_name,&faction]).map_err(pg_error)?;
        }
    }
    Ok(())
}

fn write_transfers(
    tx: &mut Transaction<'_>,
    stored: Option<&FactionSnapshot>,
    snapshot: &FactionSnapshot,
) -> Result<(), ApiError> {
    let existing: std::collections::BTreeSet<_> = stored
        .into_iter()
        .flat_map(|value| value.transfers.iter())
        .map(|value| value.key.as_str())
        .collect();
    for transfer in &snapshot.transfers {
        if existing.contains(transfer.key.as_str()) {
            continue;
        }
        let key = &transfer.key;
        let source = transfer.source.to_string();
        let target = transfer.target.to_string();
        let amount = transfer.amount.to_string();
        tx.execute("INSERT INTO lynx.faction_treasury_transfers (transfer_key,source_faction_id,target_faction_id,amount,occurred_at,schema_version) VALUES ($1,$2,$3,$4::text::numeric,to_timestamp(($5::bigint / 1000.0)::double precision),1)", &[key,&source,&target,&amount,&transfer.at.value()]).map_err(pg_error)?;
    }
    Ok(())
}

fn write_treasury_entries(
    tx: &mut Transaction<'_>,
    stored: Option<&FactionSnapshot>,
    snapshot: &FactionSnapshot,
) -> Result<(), ApiError> {
    for faction in &snapshot.factions {
        let prior = stored
            .and_then(|world| {
                world
                    .factions
                    .iter()
                    .find(|value| value.definition.id == faction.definition.id)
            })
            .map_or(0, |value| value.treasury.entries().len());
        let id = faction.definition.id.to_string();
        for entry in &faction.treasury.entries()[prior..] {
            let amount = entry.amount.to_string();
            let counterparty = entry.counterparty.as_ref().map(ToString::to_string);
            tx.execute("INSERT INTO lynx.faction_treasury_entries (faction_id,entry_key,kind,amount,counterparty_faction_id,reason,occurred_at,schema_version) VALUES ($1,$2,$3,$4::text::numeric,$5,$6,to_timestamp(($7::bigint / 1000.0)::double precision),1)", &[&id,&entry.key,&entry.kind.as_str(),&amount,&counterparty,&entry.reason,&entry.at.value()]).map_err(pg_error)?;
        }
    }
    Ok(())
}

fn write_territory_history(
    tx: &mut Transaction<'_>,
    stored: Option<&FactionSnapshot>,
    snapshot: &FactionSnapshot,
) -> Result<(), ApiError> {
    let start = stored.map_or(0, |value| value.territory_history.len());
    for change in &snapshot.territory_history[start..] {
        let zone = change.zone.to_string();
        let from = change.from.as_ref().map(ToString::to_string);
        let to = change.to.as_ref().map(ToString::to_string);
        let actor = change.actor.to_string();
        let revision = i64::try_from(change.revision).map_err(pg_error)?;
        tx.execute("INSERT INTO lynx.faction_territory_history (change_key,zone_id,action,from_faction_id,to_faction_id,actor_id,occurred_at,revision,schema_version) VALUES ($1,$2,$3,$4,$5,$6,to_timestamp(($7::bigint / 1000.0)::double precision),$8,1)", &[&change.key,&zone,&change.action.as_str(),&from,&to,&actor,&change.at.value(),&revision]).map_err(pg_error)?;
    }
    Ok(())
}

fn write_transitions(
    tx: &mut Transaction<'_>,
    stored: Option<&CalendarSnapshot>,
    snapshot: &CalendarSnapshot,
) -> Result<(), ApiError> {
    let start = stored.map_or(0, |value| value.transitions.len());
    for transition in &snapshot.transitions[start..] {
        let event = transition.event.to_string();
        let actor = transition.actor.to_string();
        let from = transition
            .from
            .map(engine_core::world_event::WorldEventStatus::as_str);
        tx.execute("INSERT INTO lynx.world_event_transitions (transition_key,event_id,action,from_status,to_status,actor_id,occurred_at,reason,schema_version) VALUES ($1,$2,$3,$4,$5,$6,to_timestamp(($7::bigint / 1000.0)::double precision),$8,1)", &[&transition.key,&event,&transition.action.as_str(),&from,&transition.to.as_str(),&actor,&transition.at.value(),&transition.reason]).map_err(pg_error)?;
    }
    Ok(())
}

fn write_enrollments(
    tx: &mut Transaction<'_>,
    stored: Option<&CalendarSnapshot>,
    snapshot: &CalendarSnapshot,
) -> Result<(), ApiError> {
    let existing: std::collections::BTreeSet<_> = stored
        .into_iter()
        .flat_map(|value| value.enrollments.iter())
        .map(|value| (&value.event, value.participant))
        .collect();
    for enrollment in &snapshot.enrollments {
        if existing.contains(&(&enrollment.event, enrollment.participant)) {
            continue;
        }
        let event = enrollment.event.to_string();
        let participant = enrollment.participant.to_string();
        let side = enrollment.side.to_string();
        let faction = enrollment.faction.as_ref().map(ToString::to_string);
        tx.execute("INSERT INTO lynx.world_event_enrollments (event_id,participant_id,side_id,faction_id,enrolled_at,schema_version) VALUES ($1,$2,$3,$4,to_timestamp(($5::bigint / 1000.0)::double precision),1)", &[&event,&participant,&side,&faction,&enrollment.at.value()]).map_err(pg_error)?;
    }
    Ok(())
}

fn write_resolutions(
    tx: &mut Transaction<'_>,
    stored: Option<&CalendarSnapshot>,
    snapshot: &CalendarSnapshot,
) -> Result<(), ApiError> {
    let existing: std::collections::BTreeSet<_> = stored
        .into_iter()
        .flat_map(|value| value.resolutions.iter())
        .map(|value| &value.event)
        .collect();
    for resolution in &snapshot.resolutions {
        if existing.contains(&resolution.event) {
            continue;
        }
        let event = resolution.event.to_string();
        let resolver = resolution.resolver.to_string();
        let winning_side = resolution.winning_side.to_string();
        let winning_faction = resolution.winning_faction.to_string();
        let territory_key = resolution
            .territory_change
            .as_ref()
            .map(|value| value.key.as_str());
        let minimum =
            i32::try_from(resolution.rule.minimum_participants_per_side).map_err(pg_error)?;
        let rule_major = i32::from(resolution.rule.version.major);
        let rule_minor = i32::from(resolution.rule.version.minor);
        tx.execute("INSERT INTO lynx.world_event_results (event_id,resolver_id,resolved_at,rule_version_major,rule_version_minor,metric,minimum_participants_per_side,winning_side_id,winning_faction_id,outcome,territory_change_key,schema_version) VALUES ($1,$2,to_timestamp(($3::bigint / 1000.0)::double precision),$4,$5,$6,$7,$8,$9,$10,$11,1)", &[&event,&resolver,&resolution.resolved_at.value(),&rule_major,&rule_minor,&resolution.rule.metric.as_str(),&minimum,&winning_side,&winning_faction,&resolution.outcome.as_str(),&territory_key]).map_err(pg_error)?;
        for side in &resolution.sides {
            let side_id = side.side.to_string();
            let faction = side.faction.to_string();
            let participants = i32::try_from(side.participants).map_err(pg_error)?;
            let power = side.faction_power.to_string();
            let score = side.score.to_string();
            tx.execute("INSERT INTO lynx.world_event_result_sides (event_id,side_id,role,faction_id,participants,faction_power,score) VALUES ($1,$2,$3,$4,$5,$6::text::numeric,$7::text::numeric)", &[&event,&side_id,&side.role.as_str(),&faction,&participants,&power,&score]).map_err(pg_error)?;
        }
    }
    Ok(())
}

fn unavailable() -> ApiError {
    ApiError::Unavailable("PostgreSQL faction/calendar store is unavailable".into())
}

fn pg_error(error: impl std::fmt::Display + 'static) -> ApiError {
    if let Some(error) = (&error as &dyn std::any::Any).downcast_ref::<postgres::Error>() {
        if let Some(database) = error.as_db_error() {
            tracing::error!(
                sqlstate = %database.code().code(),
                constraint = ?database.constraint(),
                "normalized faction/calendar SQL operation failed"
            );
        } else {
            tracing::error!(%error, "normalized faction/calendar operation failed");
        }
    } else {
        tracing::error!(%error, "normalized faction/calendar value conversion failed");
    }
    unavailable()
}
