//! Bounded, server-owned interaction presentation. No optimistic gameplay edits.
use engine_core::id::{DefinitionId, StableId};
use engine_net::envelope::{Envelope, MessageId};
use serde::Deserialize;

/// Only parameter-free verbs supported by this first client surface.
pub const VERBS: &[&str] = &[
    "inspect", "use", "open", "close", "talk", "activate", "attack",
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub target: StableId,
    pub schema: DefinitionId,
    pub title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PanelState {
    tick: u64,
    panel: Option<Panel>,
}

#[derive(Debug, Default)]
pub struct InteractionState {
    pub(crate) pending: Option<MessageId>,
    panel: Option<Panel>,
    tick: Option<u64>,
    status: &'static str,
    lock: Option<LockChallenge>,
}

/// Approved private hints only; never a server seed or answer.
#[derive(Debug, Clone, Deserialize)]
pub struct LockChallenge {
    pub target: StableId,
    pub length: u8,
    pub attempts_left: u8,
    pub solved: bool,
    pub retry_after_tick: Option<u64>,
    #[serde(default)]
    pub hints: Vec<DigitHint>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigitHint {
    Exact,
    Higher,
    Lower,
}

#[derive(Deserialize)]
struct LockAttempt {
    target: StableId,
    hints: Vec<DigitHint>,
    attempts_left: u8,
    solved: bool,
}

impl InteractionState {
    pub fn lock(&self) -> Option<&LockChallenge> {
        self.lock.as_ref().filter(|lock| {
            self.panel.as_ref().is_some_and(|panel| {
                panel.target == lock.target && panel.schema.as_str() == "ui.lockpick"
            })
        })
    }

    pub fn panel(&self) -> Option<&Panel> {
        self.panel.as_ref()
    }

    pub fn status(&self) -> &'static str {
        self.status
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn sent(&mut self, id: MessageId) {
        self.pending = Some(id);
        self.status = "Aguardando servidor…";
    }

    /// Returns whether this is an interaction message. Error means malformed wire data.
    pub(crate) fn receive(&mut self, message: &Envelope) -> Result<bool, ()> {
        if message.kind.as_str() == "ui.state" {
            let state: PanelState = serde_json::from_value(message.body.clone()).map_err(|_| ())?;
            if self.tick.is_some_and(|tick| state.tick < tick)
                || state.panel.as_ref().is_some_and(|panel| {
                    panel.title.len() > 512 || panel.title.chars().any(char::is_control)
                })
            {
                return Err(());
            }
            self.tick = Some(state.tick);
            if self.panel.is_some() && state.panel.is_none() {
                self.status = "Interface encerrada pelo servidor.";
            }
            self.panel = state.panel;
            if self.panel.is_none() {
                self.lock = None;
            }
            if self.pending.is_some() && message.correlation == self.pending {
                self.pending = None;
                self.status = "Interface atualizada.";
            }
            return Ok(true);
        }
        if self.pending.is_none() || message.correlation != self.pending {
            return Ok(false);
        }
        self.status = match message.kind.as_str() {
            "protocol.error" => rejection(message.body["code"].as_str()),
            "action.result" => match message.body["outcome"]["status"].as_str() {
                Some("applied") => {
                    self.apply_effects(&message.body["outcome"]["effects"])?;
                    "Ação confirmada pelo servidor."
                }
                Some("rejected") => rejection(message.body["outcome"]["error"]["code"].as_str()),
                _ => return Err(()),
            },
            _ => return Ok(false),
        };
        self.pending = None;
        Ok(true)
    }

    fn apply_effects(&mut self, effects: &serde_json::Value) -> Result<(), ()> {
        let effects = effects.as_array().ok_or(())?;
        if effects.len() > 64 {
            return Err(());
        }
        for effect in effects {
            match effect["kind"].as_str() {
                Some("minigame_started") => {
                    let lock: LockChallenge =
                        serde_json::from_value(effect.clone()).map_err(|_| ())?;
                    if !(2..=8).contains(&lock.length)
                        || lock.attempts_left > 20
                        || !lock.hints.is_empty()
                    {
                        return Err(());
                    }
                    self.lock = Some(lock);
                }
                Some("minigame_attempt") => {
                    let attempt: LockAttempt =
                        serde_json::from_value(effect.clone()).map_err(|_| ())?;
                    let lock = self.lock.as_mut().ok_or(())?;
                    if lock.target != attempt.target
                        || attempt.hints.len() != usize::from(lock.length)
                        || attempt.attempts_left > 20
                    {
                        return Err(());
                    }
                    lock.hints = attempt.hints;
                    lock.attempts_left = attempt.attempts_left;
                    lock.solved = attempt.solved;
                    lock.retry_after_tick = None;
                }
                Some("minigame_failed") => {
                    let target: StableId =
                        serde_json::from_value(effect["target"].clone()).map_err(|_| ())?;
                    let lock = self
                        .lock
                        .as_mut()
                        .filter(|lock| lock.target == target)
                        .ok_or(())?;
                    lock.retry_after_tick = Some(effect["retry_after_tick"].as_u64().ok_or(())?);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn rejection(code: Option<&str>) -> &'static str {
    match code {
        Some("out_of_range") => "Aproxime-se do alvo e tente novamente.",
        Some("forbidden" | "unauthorized") => "Você não tem permissão para esta ação.",
        Some("not_found") => "O alvo não está mais disponível.",
        Some("conflict") => "O estado mudou. Abra a interface novamente.",
        Some("rate_limited") => "Aguarde antes de tentar novamente.",
        _ => "Ação rejeitada. Verifique o alvo, a distância e suas permissões.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::IdKind;
    use engine_net::envelope::MessageKind;
    use serde_json::json;

    fn reply(kind: &str, body: serde_json::Value, correlation: &str) -> Envelope {
        let mut message = Envelope::new(
            MessageId::parse("reply").unwrap(),
            MessageKind::parse(kind).unwrap(),
            body,
        );
        message.correlation = Some(MessageId::parse(correlation).unwrap());
        message
    }

    #[test]
    fn authoritative_panels_close_and_reject_stale_ticks() {
        let mut state = InteractionState::default();
        let target = StableId::from_parts(IdKind::Entity, 1, 1);
        let panel = json!({"target":target,"schema":"ui.lockpick","title":"Vault"});
        state
            .receive(&reply("ui.state", json!({"tick":2,"panel":panel}), "open"))
            .unwrap();
        assert_eq!(state.panel().unwrap().target, target);
        assert!(
            state
                .receive(&reply("ui.state", json!({"tick":1,"panel":null}), "old"))
                .is_err()
        );
        assert!(state.panel().is_some());
        state.sent(MessageId::parse("close").unwrap());
        assert!(state.panel().is_some());
        state
            .receive(&reply("ui.state", json!({"tick":2,"panel":null}), "close"))
            .unwrap();
        assert!(state.panel().is_none());
        assert!(!state.pending());
    }

    #[test]
    fn rejected_panel_action_preserves_authoritative_panel_state() {
        let mut state = InteractionState::default();
        let target = StableId::from_parts(IdKind::Entity, 1, 7);
        state
            .receive(&reply(
                "ui.state",
                json!({
                    "tick":4,
                    "panel":{"target":target,"schema":"ui.display","title":"Display"}
                }),
                "open",
            ))
            .unwrap();
        let before = state.panel().cloned().unwrap();
        state.sent(MessageId::parse("panel-action").unwrap());

        assert!(
            state
                .receive(&reply(
                    "action.result",
                    json!({"outcome":{"status":"rejected","error":{"code":"out_of_range"}}}),
                    "panel-action",
                ))
                .unwrap()
        );

        assert_eq!(state.panel(), Some(&before));
        assert!(!state.pending());
    }

    #[test]
    fn unrelated_results_cannot_complete_pending_actions_and_errors_are_safe() {
        let mut state = InteractionState::default();
        state.sent(MessageId::parse("action").unwrap());
        let body = json!({"outcome":{"status":"rejected","error":{"code":"forbidden","message":"secret-token"}}});
        assert!(
            !state
                .receive(&reply("action.result", body.clone(), "other"))
                .unwrap()
        );
        assert!(state.pending());
        assert!(
            state
                .receive(&reply("action.result", body, "action"))
                .unwrap()
        );
        assert!(!state.pending());
        assert!(!format!("{state:?}").contains("secret-token"));
        state.sent(MessageId::parse("next").unwrap());
        state
            .receive(&reply("protocol.error", json!({"code":"conflict"}), "next"))
            .unwrap();
        assert!(!state.pending());
    }
}
