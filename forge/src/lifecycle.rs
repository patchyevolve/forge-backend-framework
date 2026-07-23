/// Tracks where a plugin is in its lifecycle.
///
/// Plugins move through a mostly-linear progression:
/// `Discovered → Connecting → Handshaking → Ready`, then optionally
/// `Degraded` (health failure), `Draining` (graceful shutdown), or
/// `Stopped`. A stopped plugin can re-enter at `Discovered` to restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginState {
    /// Config loaded, process not yet started.
    Discovered,
    /// Dialing the plugin's RPC endpoint.
    Connecting,
    /// Connection established, performing the Register handshake.
    Handshaking,
    /// Healthy, capabilities registered, ready to serve.
    Ready,
    /// Health checks failing — plugin is running but degraded.
    Degraded,
    /// Drain RPC sent; waiting for graceful shutdown.
    Draining,
    /// Exited or killed. May restart via [`Discovered`](PluginState::Discovered).
    Stopped,
}

impl PluginState {
    /// Attempt a state transition.
    ///
    /// Returns `Ok(target)` if the move is permitted by the lifecycle
    /// state machine, or [`InvalidTransition`] describing the illegal
    /// from → to pair.
    ///
    /// # Example
    ///
    /// ```rust
    /// use forgecore_backend_framework_daemon::lifecycle::PluginState;
    ///
    /// let next = PluginState::Discovered.transition(PluginState::Connecting);
    /// assert_eq!(next, Ok(PluginState::Connecting));
    /// ```
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
            // Retry: allow restarting from Connecting back to Discovered so the lifecycle picks it up again
            (Connecting, Discovered) => Ok(target),

            _ => Err(InvalidTransition { from: self, to: target }),
        }
    }
}

/// Returned by [`PluginState::transition`] when the requested move isn't
/// allowed by the lifecycle state machine.
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub struct InvalidTransition {
    /// The state we tried to transition from.
    pub from: PluginState,
    /// The state we tried to transition to.
    pub to: PluginState,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid transition: {:?} → {:?}", self.from, self.to)
    }
}

impl std::error::Error for InvalidTransition {}

#[cfg(feature = "gateway")]
mod manager;
#[cfg(feature = "gateway")]
#[doc(inline)]
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
