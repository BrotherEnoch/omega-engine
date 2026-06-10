// omega-prl/src/metrics/events.rs
//! Always-sampled observability event types — §19.2

/// §19.2 — Always-sampled event types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservabilityEvent {
    /// CRITICAL — relay leak suspected.
    RelayLeakSuspected,
    /// CRITICAL — sequencer restart predicted.
    SequencerRestartPredicted,
    /// HIGH — gas war surge detected.
    GasWarSurge,
    /// HIGH — searcher cluster detected.
    SearcherClusterDetected,
    /// HIGH — pattern model reverted.
    PatternModelReverted,
}

impl ObservabilityEvent {
    #[inline]
    pub fn priority(&self) -> &'static str {
        match self {
            Self::RelayLeakSuspected | Self::SequencerRestartPredicted => "CRITICAL",
            _ => "HIGH",
        }
    }

    #[inline]
    pub fn name(&self) -> &'static str {
        match self {
            Self::RelayLeakSuspected => "RELAY_LEAK_SUSPECTED",
            Self::SequencerRestartPredicted => "SEQUENCER_RESTART_PREDICTED",
            Self::GasWarSurge => "GAS_WAR_SURGE",
            Self::SearcherClusterDetected => "SEARCHER_CLUSTER_DETECTED",
            Self::PatternModelReverted => "PATTERN_MODEL_REVERTED",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities_correct() {
        assert_eq!(
            ObservabilityEvent::RelayLeakSuspected.priority(),
            "CRITICAL"
        );
        assert_eq!(
            ObservabilityEvent::SequencerRestartPredicted.priority(),
            "CRITICAL"
        );
        assert_eq!(ObservabilityEvent::GasWarSurge.priority(), "HIGH");
        assert_eq!(
            ObservabilityEvent::SearcherClusterDetected.priority(),
            "HIGH"
        );
        assert_eq!(ObservabilityEvent::PatternModelReverted.priority(), "HIGH");
    }

    #[test]
    fn names_correct() {
        assert_eq!(
            ObservabilityEvent::RelayLeakSuspected.name(),
            "RELAY_LEAK_SUSPECTED"
        );
        assert_eq!(ObservabilityEvent::GasWarSurge.name(), "GAS_WAR_SURGE");
    }
}
