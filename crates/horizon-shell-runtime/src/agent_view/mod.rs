//! From a session's model to what its pane draws
//! (`agent_display`): the turns' tool calls and receipts worded,
//! and the adapter that makes the display model of them. Apart from any
//! UI framework, as the model is.

pub mod adapter;
pub mod title;
pub mod turns;
