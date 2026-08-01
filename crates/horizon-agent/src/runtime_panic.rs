//! Panic capture shared by provider runtimes and their agentd host.
//!
//! `catch_unwind` retains the panic payload but not its source location. Rust
//! exposes that location only to the process-wide panic hook, so Horizon
//! installs one hook that records it in thread-local storage before delegating
//! to the previous hook. Each runtime boundary can then persist an actionable
//! payload plus `file:line:column` without racing with panics on other session
//! threads.

use std::{
    any::Any,
    cell::RefCell,
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Once,
};

static INSTALL_LOCATION_HOOK: Once = Once::new();

thread_local! {
    static LAST_LOCATION: RefCell<Option<PanicLocation>> = const {
        RefCell::new(None)
    };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PanicLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for PanicLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}:{}", self.file, self.line, self.column)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PanicReport {
    pub payload: String,
    pub location: Option<PanicLocation>,
}

impl PanicReport {
    fn from_payload(payload: Box<dyn Any + Send>) -> Self {
        let payload = match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(message) => (*message).to_string(),
                Err(_) => "non-string panic payload".to_string(),
            },
        };
        Self {
            payload,
            location: take_location(),
        }
    }

    pub(crate) fn message(&self, context: &str) -> String {
        match &self.location {
            Some(location) => format!("{context} at {location}: {}", self.payload),
            None => format!("{context}: {}", self.payload),
        }
    }
}

pub fn catch_runtime_panic<T>(operation: impl FnOnce() -> T) -> Result<T, PanicReport> {
    install_location_hook();
    clear_location();
    catch_unwind(AssertUnwindSafe(operation)).map_err(PanicReport::from_payload)
}

fn install_location_hook() {
    INSTALL_LOCATION_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(location) = info.location() {
                LAST_LOCATION.with(|slot| {
                    if let Ok(mut slot) = slot.try_borrow_mut() {
                        *slot = Some(PanicLocation {
                            file: location.file().to_string(),
                            line: location.line(),
                            column: location.column(),
                        });
                    }
                });
            }
            previous(info);
        }));
    });
}

fn clear_location() {
    LAST_LOCATION.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            *slot = None;
        }
    });
}

fn take_location() -> Option<PanicLocation> {
    LAST_LOCATION.with(|slot| slot.try_borrow_mut().ok().and_then(|mut slot| slot.take()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_retains_payload_and_source_location() {
        let report = catch_runtime_panic(|| panic!("runtime invariant failed"))
            .expect_err("panic must cross the boundary");

        assert_eq!(report.payload, "runtime invariant failed");
        let location = report.location.expect("source location");
        assert!(location
            .file
            .ends_with("crates/horizon-agent/src/runtime_panic.rs"));
        assert!(location.line > 0);
    }

    #[test]
    fn capture_labels_non_string_payloads() {
        let report = catch_runtime_panic(|| std::panic::panic_any(42_u8)).unwrap_err();

        assert_eq!(report.payload, "non-string panic payload");
        assert!(report.location.is_some());
    }
}
