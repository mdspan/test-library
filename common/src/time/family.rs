//! The four clock families, the atomic unit of configuration.

/// A logical grouping of clocks. Every setting lands on a whole family at once, and every clock in
/// a family reads the family's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// The wall clock.
    Realtime,
    /// Nondecreasing, stops across suspend.
    Monotonic,
    /// Nondecreasing, keeps counting across suspend.
    Boottime,
    /// Process and thread CPU time, which has no timebase and no anchor pair.
    Cpu,
}

impl Family {
    /// Every family, in the order the family table lists them.
    pub const ALL: [Self; 4] = [Self::Realtime, Self::Monotonic, Self::Boottime, Self::Cpu];

    /// The three families that carry a line over a timebase. Cpu scales its own reading instead.
    pub const WITH_A_TIMEBASE: [Self; 3] = [Self::Realtime, Self::Monotonic, Self::Boottime];

    /// Whether the family refuses a move that would take it below zero.
    #[must_use]
    pub const fn is_nondecreasing(self) -> bool {
        matches!(self, Self::Monotonic | Self::Boottime)
    }
}

impl std::fmt::Display for Family {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Realtime => "Realtime",
            Self::Monotonic => "Monotonic",
            Self::Boottime => "Boottime",
            Self::Cpu => "Cpu",
        })
    }
}
