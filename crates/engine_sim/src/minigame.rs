//! Server-authoritative minigames.
//!
//! A minigame is a small challenge attached to an entity-provided interface.
//! The server owns the combination, the attempt budget, and the lockout. A
//! client submits guesses through the panel action channel and receives
//! per-digit hints; it never receives the answer and never decides the outcome.
//!
//! The combination is derived from a server-held seed and the authenticated
//! player's principal. The seed must come from server entropy, not from content
//! the client also downloads. Minigame state is server-only: it is not part of
//! zone replication.

use std::collections::BTreeMap;

use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, StableId};
use engine_core::time::Tick;
use serde::{Deserialize, Serialize};

/// Shortest accepted combination, in digits.
pub const MIN_COMBINATION_LEN: u8 = 2;

/// Longest accepted combination, in digits.
pub const MAX_COMBINATION_LEN: u8 = 8;

/// Most attempts a player may have between lockouts.
pub const MAX_ATTEMPTS: u8 = 20;

/// Comparison of one guessed digit against the server-held combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigitHint {
    /// The digit is correct.
    Exact,
    /// The combination's digit is higher than the guess.
    Higher,
    /// The combination's digit is lower than the guess.
    Lower,
}

/// One player's progress in a minigame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinigameProgress {
    /// Remaining attempts before a lockout.
    pub attempts_left: u8,
    /// Whether the combination was already solved.
    pub solved: bool,
    /// Tick before which no new guess is accepted, after a lockout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_until: Option<Tick>,
}

/// A server-authoritative combination lock attached to an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Minigame {
    /// Content definition this challenge was authored from.
    pub definition: DefinitionId,
    /// Number of digits in the combination.
    pub length: u8,
    /// Attempts per player between lockouts.
    pub attempts: u8,
    /// Server ticks a player must wait after exhausting the attempt budget.
    pub retry_cooldown_ticks: u64,
    /// Server-held seed used to derive each player's combination.
    ///
    /// This value is never sent to clients. It must be generated from server
    /// entropy; a seed shipped inside a client-visible content bundle would let
    /// a modified client derive the combination.
    pub seed: u64,
    /// Per-player progress, keyed by the authenticated principal.
    pub progress: BTreeMap<StableId, MinigameProgress>,
}

/// The current status of one player's challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinigameStatus {
    /// Content definition.
    pub definition: DefinitionId,
    /// Combination length in digits.
    pub length: u8,
    /// Remaining attempts.
    pub attempts_left: u8,
    /// Whether the player already solved it.
    pub solved: bool,
    /// Tick before which no guess is accepted, after a lockout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_tick: Option<Tick>,
}

/// The result of one accepted guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinigameTurn {
    /// Per-digit comparison, in combination order.
    pub hints: Vec<DigitHint>,
    /// Remaining attempts.
    pub attempts_left: u8,
    /// Whether this guess solved the challenge.
    pub solved: bool,
    /// Tick before which no guess is accepted, after a lockout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_tick: Option<Tick>,
}

impl Minigame {
    /// Create a minigame from server-authored rules.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the definition is not in the
    /// `minigame` namespace, the length is outside
    /// [`MIN_COMBINATION_LEN`]..=[`MAX_COMBINATION_LEN`], or the attempt budget
    /// is zero or above [`MAX_ATTEMPTS`].
    pub fn new(
        definition: DefinitionId,
        length: u8,
        attempts: u8,
        retry_cooldown_ticks: u64,
        seed: u64,
    ) -> Result<Self, EngineError> {
        if !definition.is_in("minigame") {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "minigame definitions must use the minigame namespace",
            )
            .with_detail("definition", definition.to_string()));
        }
        if !(MIN_COMBINATION_LEN..=MAX_COMBINATION_LEN).contains(&length) {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "minigame combination length is out of range",
            )
            .with_detail("length", length.to_string())
            .with_detail("min", MIN_COMBINATION_LEN.to_string())
            .with_detail("max", MAX_COMBINATION_LEN.to_string()));
        }
        if attempts == 0 || attempts > MAX_ATTEMPTS {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "minigame attempt budget is out of range",
            )
            .with_detail("attempts", attempts.to_string())
            .with_detail("max", MAX_ATTEMPTS.to_string()));
        }
        Ok(Self {
            definition,
            length,
            attempts,
            retry_cooldown_ticks,
            seed,
            progress: BTreeMap::new(),
        })
    }

    /// Derive the server-held combination for one player.
    ///
    /// This is deterministic for a given seed and principal so a player keeps
    /// the same combination across lockouts and reconnects. It must never be
    /// sent to a client.
    #[must_use]
    pub fn secret_for(&self, player: StableId) -> Vec<u8> {
        let random = player.random_bits();
        let low = u64::try_from(random & u128::from(u64::MAX)).unwrap_or_default();
        let high = u64::try_from(random >> 64).unwrap_or_default();
        let mut state = self.seed ^ low ^ high ^ player.timestamp_ms();
        (0..self.length)
            .map(|_| {
                state = splitmix64(state);
                (state % 10) as u8
            })
            .collect()
    }

    /// The player's current status, resetting an expired lockout.
    pub fn status(&mut self, player: StableId, tick: Tick) -> MinigameStatus {
        let definition = self.definition.clone();
        let length = self.length;
        let progress = self.ensure_progress(player, tick);
        let attempts_left = progress.attempts_left;
        let solved = progress.solved;
        let retry_after_tick = progress.locked_until.filter(|until| *until > tick);
        MinigameStatus {
            definition,
            length,
            attempts_left,
            solved,
            retry_after_tick,
        }
    }

    /// Validate and apply one guess.
    ///
    /// A guess must be exactly [`Minigame::length`] decimal digits. Invalid
    /// input and lockouts are rejected without consuming an attempt.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for a malformed guess,
    /// [`ErrorCode::InvalidState`] when the challenge is already solved, and
    /// [`ErrorCode::Cooldown`] while a lockout is active.
    pub fn play(
        &mut self,
        player: StableId,
        guess: &str,
        tick: Tick,
    ) -> Result<MinigameTurn, EngineError> {
        let secret = self.secret_for(player);
        let digits = parse_guess(guess, self.length)?;
        let attempts = self.attempts;
        let retry_cooldown_ticks = self.retry_cooldown_ticks;
        let progress = self.ensure_progress(player, tick);

        if progress.solved {
            return Err(EngineError::new(
                ErrorCode::InvalidState,
                "the minigame is already solved",
            ));
        }
        if let Some(until) = progress.locked_until
            && tick < until
        {
            return Err(
                EngineError::new(ErrorCode::Cooldown, "the minigame is locked out")
                    .with_detail("remaining_ticks", until.elapsed_since(tick).to_string()),
            );
        }
        if progress.attempts_left == 0 {
            progress.attempts_left = attempts;
        }

        let hints: Vec<DigitHint> = secret
            .iter()
            .zip(digits.iter())
            .map(|(secret, guess)| match guess.cmp(secret) {
                std::cmp::Ordering::Equal => DigitHint::Exact,
                std::cmp::Ordering::Less => DigitHint::Higher,
                std::cmp::Ordering::Greater => DigitHint::Lower,
            })
            .collect();
        let solved = hints.iter().all(|hint| *hint == DigitHint::Exact);
        if solved {
            progress.solved = true;
        } else {
            progress.attempts_left = progress.attempts_left.saturating_sub(1);
            if progress.attempts_left == 0 {
                progress.locked_until = Some(Tick(tick.0.saturating_add(retry_cooldown_ticks)));
            }
        }

        Ok(MinigameTurn {
            hints,
            attempts_left: progress.attempts_left,
            solved,
            retry_after_tick: progress.locked_until.filter(|until| *until > tick),
        })
    }

    /// Fetch progress, creating it on first use and clearing an expired lockout.
    fn ensure_progress(&mut self, player: StableId, tick: Tick) -> &mut MinigameProgress {
        let attempts = self.attempts;
        let progress = self.progress.entry(player).or_insert(MinigameProgress {
            attempts_left: attempts,
            solved: false,
            locked_until: None,
        });
        if !progress.solved
            && progress.attempts_left == 0
            && progress.locked_until.is_some_and(|until| tick >= until)
        {
            progress.attempts_left = attempts;
            progress.locked_until = None;
        }
        progress
    }
}

/// Parse a fixed-length decimal guess.
fn parse_guess(guess: &str, length: u8) -> Result<Vec<u8>, EngineError> {
    let expected = usize::from(length);
    let digits: Vec<u8> = guess.bytes().map(|byte| byte.wrapping_sub(b'0')).collect();
    if guess.len() != expected || digits.iter().any(|digit| *digit > 9) {
        return Err(EngineError::new(
            ErrorCode::InvalidRequest,
            "guess must be a fixed-length decimal code",
        )
        .with_detail("expected_length", expected.to_string())
        .with_detail("received_length", guess.len().to_string()));
    }
    Ok(digits)
}

/// A small, deterministic mixer. Not cryptographic; the seed is the secret.
const fn splitmix64(state: u64) -> u64 {
    let mut value = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::id::IdKind;

    fn id(seed: u128) -> StableId {
        StableId::from_parts(IdKind::Character, 1, seed)
    }

    fn minigame() -> Minigame {
        Minigame::new(
            DefinitionId::new("minigame.lockpick").unwrap(),
            3,
            3,
            10,
            42,
        )
        .unwrap()
    }

    #[test]
    fn secrets_are_deterministic_per_player_and_within_range() {
        let minigame = minigame();
        let alice = id(1);
        let bob = id(2);
        let secret = minigame.secret_for(alice);
        assert_eq!(secret.len(), 3);
        assert!(secret.iter().all(|digit| *digit <= 9));
        assert_eq!(minigame.secret_for(alice), secret);
        assert_ne!(minigame.secret_for(bob), secret);
    }

    #[test]
    fn guesses_reject_wrong_length_and_non_digits() {
        let mut minigame = minigame();
        let player = id(1);
        for guess in ["", "12", "1234", "12a", "１２３"] {
            let error = minigame.play(player, guess, Tick::ZERO).unwrap_err();
            assert_eq!(error.code(), ErrorCode::InvalidRequest, "{guess:?}");
        }
        assert_eq!(
            minigame.progress.get(&player).map(|p| p.attempts_left),
            None,
            "malformed input must not create progress"
        );
    }

    #[test]
    fn solving_marks_the_player_and_does_not_consume_an_attempt() {
        let mut minigame = minigame();
        let player = id(1);
        let secret: String = minigame
            .secret_for(player)
            .iter()
            .map(|digit| char::from(b'0' + digit))
            .collect();
        let turn = minigame.play(player, &secret, Tick::ZERO).unwrap();
        assert!(turn.solved);
        assert_eq!(turn.hints, vec![DigitHint::Exact; 3]);
        assert_eq!(turn.attempts_left, 3);
        assert!(minigame.status(player, Tick::ZERO).solved);

        let error = minigame.play(player, &secret, Tick::ZERO).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidState);
    }

    #[test]
    fn hints_point_toward_the_secret() {
        let mut minigame = minigame();
        let player = id(1);
        let secret = minigame.secret_for(player);
        let turn = minigame.play(player, "000", Tick::ZERO).unwrap();
        for (hint, digit) in turn.hints.iter().zip(secret.iter()) {
            match hint {
                DigitHint::Exact => assert_eq!(*digit, 0),
                DigitHint::Higher => assert!(*digit > 0),
                DigitHint::Lower => unreachable!("a digit cannot be below zero"),
            }
        }
    }

    #[test]
    fn exhausting_attempts_locks_out_until_the_cooldown_elapses() {
        let mut minigame = minigame();
        let player = id(1);
        let secret: String = minigame
            .secret_for(player)
            .iter()
            .map(|digit| char::from(b'0' + digit))
            .collect();
        let wrong = if secret == "000" { "111" } else { "000" };
        for _ in 0..3 {
            let _ = minigame.play(player, wrong, Tick::ZERO).unwrap();
        }
        let error = minigame.play(player, wrong, Tick::ZERO).unwrap_err();
        assert_eq!(error.code(), ErrorCode::Cooldown);
        assert_eq!(
            minigame.status(player, Tick::ZERO).retry_after_tick,
            Some(Tick(10))
        );

        let error = minigame.play(player, wrong, Tick(9)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::Cooldown);
        let turn = minigame.play(player, &secret, Tick(10)).unwrap();
        assert!(turn.solved, "a fresh budget accepts the correct code");
    }

    #[test]
    fn progress_is_isolated_per_player() {
        let mut minigame = minigame();
        let alice = id(1);
        let bob = id(2);
        let secret: String = minigame
            .secret_for(alice)
            .iter()
            .map(|digit| char::from(b'0' + digit))
            .collect();
        let wrong = if secret == "000" { "111" } else { "000" };
        let _ = minigame.play(alice, wrong, Tick::ZERO).unwrap();
        assert_eq!(minigame.status(alice, Tick::ZERO).attempts_left, 2);
        assert_eq!(minigame.status(bob, Tick::ZERO).attempts_left, 3);
    }
}
