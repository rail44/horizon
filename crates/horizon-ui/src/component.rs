//! A component: what a view is made of, and what a view is.
//!
//! A component is a struct of what it is given (its props) that builds
//! its element once, by value: `view` takes the props and they are gone.
//! A prop is a plain value, or a signal where it changes. Which way a
//! signal goes is its type: one the component reads is a `Signal`, one it
//! writes for whoever placed it to read is a `SignalSetter`, and one both
//! write is an `RwSignal`. What happens once (a press, a notification) is
//! told through a [`Callback`] prop.
//!
//! ```ignore
//! parent.child(Button { label: "Save", kind: ButtonKind::Primary, ..Default::default() })
//! ```
//!
//! What whoever places a component can do to its element is give it a
//! class of their own, for where it sits ([`Component::class`]). The rest
//! is the component's to offer, as props. `view` is the way to the
//! element itself, for what a component does not offer.
//!
//! An element is the component that is itself.

use crate::{Classes, El};

/// A callback of the UI thread's: a prop for what happens once.
pub type Callback<In> = reactive_graph::callback::UnsyncCallback<In>;

pub use reactive_graph::callback::Callable;

/// See the module's documentation.
pub trait Component {
    /// Builds the component's element from its props.
    fn view(self) -> El;

    /// Builds the component with classes of whoever places it on its
    /// element: where it sits among what is beside it.
    fn class(self, classes: impl Classes) -> El
    where
        Self: Sized,
    {
        self.view().class(classes)
    }
}

impl Component for El {
    fn view(self) -> El {
        self
    }
}
