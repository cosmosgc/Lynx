//! Pure, bounded movement prediction and presentation reconciliation. This
//! module does not send input or validate server movement rules; callers must
//! route only messages from the current authenticated connection and correlate
//! queue errors to requests.

/// A world-space position in metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Position {
    fn finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

/// The only data to put in a movement request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovementRequest {
    pub sequence: u64,
    pub destination: Position,
}

/// Whether a baseline belongs to the existing or a new control session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDomain {
    Resume,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidPosition,
    InvalidPredictionLimit,
    PredictionTooFar,
    AwaitingBaseline,
    InFlight,
    SequenceExhausted,
    UnexpectedMessage,
    StaleTick,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    request: MovementRequest,
    queued: bool,
}

/// Local presentation state, never an authority for simulation or collision.
/// The configurable maximum prediction distance bounds visible divergence to
/// one requested endpoint from the latest authoritative position.
#[derive(Debug)]
pub struct Reconciliation {
    max_distance: f32,
    snap_distance: f32,
    authoritative: Option<Position>,
    presentation: Option<Position>,
    tick: u64,
    last_issued: Option<u64>,
    pending: Option<Pending>,
}

impl Reconciliation {
    pub fn new(max_distance: f32) -> Result<Self, Error> {
        if !max_distance.is_finite() || max_distance < 0.0 {
            return Err(Error::InvalidPredictionLimit);
        }
        Ok(Self {
            max_distance,
            // A correction larger than the largest locally permitted endpoint
            // is never interpolated. It is a discontinuity in the authoritative
            // world (for example a teleport), not a client simulation result.
            snap_distance: max_distance,
            authoritative: None,
            presentation: None,
            tick: 0,
            last_issued: None,
            pending: None,
        })
    }

    /// Invalidate a disconnected connection immediately. No further prediction
    /// is possible until a fresh session-private baseline is installed.
    pub fn disconnect(&mut self) {
        self.authoritative = None;
        self.presentation = None;
        self.pending = None;
    }

    /// Install a snapshot baseline and discard prediction history. On resume,
    /// retain locally issued sequence numbers as well as the server watermark;
    /// on a new control session only the new baseline's watermark applies.
    pub fn baseline(
        &mut self,
        position: Position,
        tick: u64,
        last_consumed: Option<u64>,
        domain: SessionDomain,
    ) -> Result<(), Error> {
        if !position.finite() {
            return Err(Error::InvalidPosition);
        }
        self.last_issued = match domain {
            SessionDomain::Resume => self.last_issued.max(last_consumed),
            SessionDomain::New => last_consumed,
        };
        self.authoritative = Some(position);
        self.presentation = Some(position);
        self.tick = tick;
        self.pending = None;
        Ok(())
    }

    pub fn authoritative_position(&self) -> Option<Position> {
        self.authoritative
    }

    /// Presentation position; snaps to authority on rejection or correction.
    pub fn predicted_position(&self) -> Option<Position> {
        self.authoritative
            .map(|position| self.pending.map_or(position, |p| p.request.destination))
    }

    /// Position for rendering. It is deliberately separate from prediction and
    /// authoritative state: smoothing can never change a movement request or a
    /// collision decision.
    pub fn presentation_position(&self) -> Option<Position> {
        self.presentation
    }

    /// Move the visual position toward the latest prediction or correction.
    /// `fraction` is caller-owned frame interpolation in the inclusive range
    /// 0..=1; invalid values do not alter presentation state.
    pub fn smooth(&mut self, fraction: f32) {
        if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
            return;
        }
        let Some(target) = self.predicted_position() else {
            return;
        };
        let Some(current) = self.presentation else {
            self.presentation = Some(target);
            return;
        };
        self.presentation = Some(Position {
            x: current.x + (target.x - current.x) * fraction,
            y: current.y + (target.y - current.y) * fraction,
            z: current.z + (target.z - current.z) * fraction,
        });
    }

    /// Predict an absolute endpoint. The limit is a local visual bound, not a
    /// claim about server speed, timing, height, collision, or acceptance.
    pub fn predict(&mut self, destination: Position) -> Result<MovementRequest, Error> {
        let origin = self.authoritative.ok_or(Error::AwaitingBaseline)?;
        if self.pending.is_some() {
            return Err(Error::InFlight);
        }
        if !destination.finite() {
            return Err(Error::InvalidPosition);
        }
        let dx = f64::from(destination.x) - f64::from(origin.x);
        let dy = f64::from(destination.y) - f64::from(origin.y);
        let dz = f64::from(destination.z) - f64::from(origin.z);
        if dx * dx + dy * dy + dz * dz > f64::from(self.max_distance).powi(2) {
            return Err(Error::PredictionTooFar);
        }
        let sequence = match self.last_issued {
            Some(last) => last.checked_add(1).ok_or(Error::SequenceExhausted)?,
            None => 1,
        };
        let request = MovementRequest {
            sequence,
            destination,
        };
        self.last_issued = Some(sequence);
        self.pending = Some(Pending {
            request,
            queued: false,
        });
        // Predicting is presentation-only. The displayed endpoint can be
        // immediate; a later private server result will correct it.
        self.presentation = Some(destination);
        Ok(request)
    }

    /// Queue acceptance consumes a server sequence but is not a correction.
    pub fn queued(&mut self, sequence: u64) -> Result<(), Error> {
        let pending = self
            .pending
            .as_mut()
            .filter(|p| p.request.sequence == sequence && !p.queued)
            .ok_or(Error::UnexpectedMessage)?;
        pending.queued = true;
        Ok(())
    }

    /// Only call for a correlated, definitive queue rejection, not a timeout or
    /// transport failure. Ambiguous delivery requires disconnect and baseline.
    pub fn queue_rejected(&mut self, sequence: u64) -> Result<(), Error> {
        if !self
            .pending
            .is_some_and(|p| p.request.sequence == sequence && !p.queued)
        {
            return Err(Error::UnexpectedMessage);
        }
        self.pending = None;
        Ok(())
    }

    /// Apply the private processed result for the exact pending sequence. Both
    /// applied and rejected outcomes replace prediction with server position.
    pub fn processed(
        &mut self,
        sequence: u64,
        tick: u64,
        position: Position,
        _applied: bool,
    ) -> Result<(), Error> {
        if self
            .pending
            .is_none_or(|pending| pending.request.sequence != sequence)
        {
            return Err(Error::UnexpectedMessage);
        }
        if !position.finite() {
            return Err(Error::InvalidPosition);
        }
        if tick < self.tick {
            return Err(Error::StaleTick);
        }
        let previous_prediction = self.predicted_position();
        self.authoritative = Some(position);
        self.tick = tick;
        self.pending = None;
        let must_snap = previous_prediction
            .is_none_or(|predicted| distance(predicted, position) > self.snap_distance);
        if must_snap {
            self.presentation = Some(position);
        }
        Ok(())
    }

    /// Reset prediction for a server-declared discontinuity. Teleport, death,
    /// zone transfer, and a new session must call this with the fresh private
    /// snapshot instead of replaying inputs from the old world.
    pub fn reset(
        &mut self,
        position: Position,
        tick: u64,
        last_consumed: Option<u64>,
        domain: SessionDomain,
    ) -> Result<(), Error> {
        self.baseline(position, tick, last_consumed, domain)
    }
}

fn distance(a: Position, b: Position) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    (dx.mul_add(dx, dy.mul_add(dy, dz * dz))).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: f32) -> Position {
        Position { x, y: 0.0, z: 0.0 }
    }
    fn client() -> Reconciliation {
        let mut c = Reconciliation::new(1.0).unwrap();
        c.baseline(pos(0.0), 10, None, SessionDomain::New).unwrap();
        c
    }

    #[test]
    fn queued_is_not_processed_and_correction_snaps() {
        let mut c = client();
        let r = c.predict(pos(1.0)).unwrap();
        c.queued(r.sequence).unwrap();
        assert_eq!(c.authoritative_position(), Some(pos(0.0)));
        assert_eq!(c.predicted_position(), Some(pos(1.0)));
        assert_eq!(c.predict(pos(0.5)), Err(Error::InFlight));
        c.processed(r.sequence, 11, pos(0.25), false).unwrap();
        assert_eq!(c.predicted_position(), Some(pos(0.25)));
        assert_eq!(c.predict(pos(1.25)).unwrap().sequence, 2);
    }

    #[test]
    fn rejection_and_unexpected_messages_do_not_grant_authority() {
        let mut c = client();
        let r = c.predict(pos(1.0)).unwrap();
        assert_eq!(
            c.processed(99, 11, pos(100.0), true),
            Err(Error::UnexpectedMessage)
        );
        assert_eq!(
            c.processed(r.sequence, 9, pos(100.0), true),
            Err(Error::StaleTick)
        );
        c.queue_rejected(r.sequence).unwrap();
        assert_eq!(c.predicted_position(), Some(pos(0.0)));
        assert_eq!(c.predict(pos(0.5)).unwrap().sequence, 2);
    }

    #[test]
    fn disconnect_resume_and_new_session_reset() {
        let mut c = client();
        let r = c.predict(pos(1.0)).unwrap();
        c.queued(r.sequence).unwrap();
        c.disconnect();
        assert_eq!(c.predict(pos(0.0)), Err(Error::AwaitingBaseline));
        c.baseline(pos(0.2), 12, Some(3), SessionDomain::Resume)
            .unwrap();
        assert_eq!(
            c.processed(r.sequence, 11, pos(1.0), true),
            Err(Error::UnexpectedMessage)
        );
        assert_eq!(c.predict(pos(0.3)).unwrap().sequence, 4);
        c.disconnect();
        c.baseline(pos(5.0), 1, None, SessionDomain::New).unwrap();
        assert_eq!(c.predict(pos(5.5)).unwrap().sequence, 1);
    }

    #[test]
    fn bounded_invalid_and_exhausted_inputs() {
        let mut c = client();
        assert_eq!(c.predict(pos(1.01)), Err(Error::PredictionTooFar));
        assert_eq!(c.predict(pos(f32::NAN)), Err(Error::InvalidPosition));
        c.baseline(pos(0.0), 10, Some(u64::MAX), SessionDomain::Resume)
            .unwrap();
        assert_eq!(c.predict(pos(0.0)), Err(Error::SequenceExhausted));
        assert_eq!(
            c.baseline(pos(f32::INFINITY), 11, None, SessionDomain::New),
            Err(Error::InvalidPosition)
        );
        assert_eq!(c.authoritative_position(), Some(pos(0.0)));
    }

    #[test]
    fn latency_and_loss_remain_bounded_until_baseline() {
        let mut c = client();
        let r = c.predict(pos(1.0)).unwrap();
        // Missing queue/result messages cannot create more predictions.
        for _ in 0..100 {
            assert_eq!(c.predict(pos(2.0)), Err(Error::InFlight));
            assert_eq!(c.predicted_position(), Some(pos(1.0)));
        }
        c.disconnect();
        c.baseline(pos(0.4), 15, Some(r.sequence), SessionDomain::Resume)
            .unwrap();
        assert_eq!(c.predicted_position(), Some(pos(0.4)));
    }

    #[test]
    fn latency_loss_and_forced_correction_are_bounded_and_recover() {
        let mut c = client();
        // A delayed queue acknowledgement leaves exactly one bounded local
        // prediction; dropping it cannot manufacture additional movement.
        let request = c.predict(pos(1.0)).unwrap();
        for _ in 0..32 {
            c.smooth(0.25);
            assert_eq!(c.predicted_position(), Some(pos(1.0)));
            assert_eq!(c.predict(pos(2.0)), Err(Error::InFlight));
        }
        c.queued(request.sequence).unwrap();
        // A forced authoritative correction does not preserve a client outcome.
        c.processed(request.sequence, 11, pos(0.25), false).unwrap();
        c.smooth(0.5);
        assert_eq!(c.authoritative_position(), Some(pos(0.25)));
        assert!(c.presentation_position().unwrap().x > 0.25);
        c.smooth(1.0);
        assert_eq!(c.presentation_position(), Some(pos(0.25)));
        assert_eq!(c.predict(pos(1.25)).unwrap().sequence, 2);
    }

    #[test]
    fn discontinuities_reset_history_and_snap_presentation() {
        let mut c = client();
        let request = c.predict(pos(1.0)).unwrap();
        c.queued(request.sequence).unwrap();
        c.reset(pos(50.0), 20, None, SessionDomain::New).unwrap();
        assert_eq!(c.authoritative_position(), Some(pos(50.0)));
        assert_eq!(c.presentation_position(), Some(pos(50.0)));
        assert_eq!(c.predict(pos(50.5)).unwrap().sequence, 1);
    }
}
