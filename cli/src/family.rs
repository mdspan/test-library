//! The words a user writes for the families.

use common::time::Family;

use crate::failure::Failure;

/// The word for `family`.
pub(crate) const fn label(family: Family) -> &'static str {
    match family {
        Family::Realtime => "realtime",
        Family::Monotonic => "monotonic",
        Family::Boottime => "boottime",
        Family::Cpu => "cpu",
    }
}

/// The family `text` names, ignoring case and surrounding space.
pub(crate) fn named(text: &str) -> Result<Family, Failure> {
    let word = text.trim();
    Family::ALL
        .into_iter()
        .find(|&family| label(family).eq_ignore_ascii_case(word))
        .ok_or_else(|| {
            let known = Family::ALL.map(label).join(", ");
            Failure::Other(format!(
                "no family called {text:?}; the families are {known}"
            ))
        })
}
