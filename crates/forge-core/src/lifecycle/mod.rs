#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginState {
    Discovered,
    Connecting,
    Handshaking,
    Ready,
    Degraded,
    Draining,
    Stopped,
}

impl PluginState {
    pub fn transition(self, target: PluginState) -> Result<PluginState, InvalidTransition> {
        use PluginState::*;
        match (self, target) {
            // Normal forward progression through the lifecycle
            (Discovered, Connecting)
            | (Connecting, Handshaking)
            | (Handshaking, Ready)
            | (Ready, Degraded)
            | (Degraded, Ready) // Plugin recovered — health check passed again
            | (Ready, Draining)
            | (Degraded, Draining)
            | (Draining, Stopped)
            | (Discovered, Stopped) // Abort before connecting
            | (Connecting, Stopped) // Connection refused
            | (Handshaking, Stopped) // Version mismatch during handshake
            | (Degraded, Stopped) // Too many health check failures
            // Plugin process died — jump straight to STOPPED
            | (Ready, Stopped) => Ok(target),

            // Re-entry from Stopped means going back to Discovered — either hot-reload or operator restart
            (Stopped, Discovered) => Ok(target),

            _ => Err(InvalidTransition { from: self, to: target }),
        }
    }
}

#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub struct InvalidTransition {
    pub from: PluginState,
    pub to: PluginState,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid transition: {:?} → {:?}", self.from, self.to)
    }
}

impl std::error::Error for InvalidTransition {}

#[cfg(feature = "tonic")]
mod manager;
#[cfg(feature = "tonic")]
pub use manager::Manager;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        let s = PluginState::Discovered;
        assert_eq!(
            s.transition(PluginState::Connecting),
            Ok(PluginState::Connecting)
        );
    }

    #[test]
    fn full_lifecycle() {
        let states = [
            PluginState::Discovered,
            PluginState::Connecting,
            PluginState::Handshaking,
            PluginState::Ready,
        ];
        let mut s = PluginState::Discovered;
        for &next in &states[1..] {
            s = s.transition(next).unwrap();
        }
        assert_eq!(s, PluginState::Ready);
    }

    #[test]
    fn health_check_recovery() {
        let s = PluginState::Degraded;
        assert_eq!(s.transition(PluginState::Ready), Ok(PluginState::Ready));
    }

    #[test]
    fn illegal_stopped_to_ready() {
        let s = PluginState::Stopped;
        assert!(s.transition(PluginState::Ready).is_err());
    }

    #[test]
    fn reentry_from_stopped() {
        let s = PluginState::Stopped;
        assert_eq!(
            s.transition(PluginState::Discovered),
            Ok(PluginState::Discovered)
        );
    }

    #[test]
    fn draining_path() {
        let s = PluginState::Ready;
        let s = s.transition(PluginState::Draining).unwrap();
        let s = s.transition(PluginState::Stopped).unwrap();
        assert_eq!(s, PluginState::Stopped);
    }
}
