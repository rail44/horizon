//! Reassemble explicitly delimited provider responses before pairing repair.
//! Tools may complete while the same response is still announcing more calls.
use crate::contract::{Event, MessageRole};

pub(super) fn ordered_for_provider(events: &[Event]) -> Vec<&Event> {
    let mut output = Vec::with_capacity(events.len());
    let mut response = Vec::new();
    let mut in_response = false;
    for event in events {
        let boundary = matches!(event, Event::ProviderRequestSent(_) | Event::TurnEnded(_))
            || matches!(event, Event::MessageCommitted(message) if message.role.provider_side() == crate::contract::ProviderSide::User);
        if boundary {
            flush(&mut response, &mut output);
            in_response = matches!(event, Event::ProviderRequestSent(_));
            output.push(event);
            continue;
        }
        if in_response {
            response.push(event);
        } else {
            output.push(event);
        }
    }
    flush(&mut response, &mut output);
    output
}
fn flush<'a>(response: &mut Vec<&'a Event>, output: &mut Vec<&'a Event>) {
    // This only changes the provider projection. Audit order is untouched.
    // Stable ordering preserves both call announcement order and result order.
    for priority in 0..3 {
        output.extend(response.iter().copied().filter(|event| {
            let rank = match event {
                Event::MessageCommitted(message) if message.role == MessageRole::Assistant => 0,
                Event::ToolCallRequested(_) => 1,
                _ => 2,
            };
            rank == priority
        }));
    }
    response.clear();
}
