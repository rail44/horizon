//! A modal's list, target, subscriptions and pending replies share one lifetime.

use gpui::{AnyWindowHandle, App, Context, Entity, Subscription, Task, Window};
use gpui_component::list::{ListDelegate, ListState};
use std::future::Future;

pub(in crate::workspace) struct ListModal<D: ListDelegate, T = ()> {
    pub(in crate::workspace) list: Entity<ListState<D>>,
    pub(in crate::workspace) target: T,
    _subscription: Subscription,
    requests: Vec<Task<()>>,
}

impl<D: ListDelegate, T> ListModal<D, T> {
    pub(in crate::workspace) fn new(
        list: Entity<ListState<D>>,
        target: T,
        subscription: Subscription,
    ) -> Self {
        Self {
            list,
            target,
            _subscription: subscription,
            requests: Vec::new(),
        }
    }
    /// Bind delivery to this list and retain the continuation until it finishes
    /// or the modal closes. Tasks are cancelled on drop; they are never detached.
    pub(in crate::workspace) fn receive<R: 'static>(
        &mut self,
        window: AnyWindowHandle,
        reply: impl Future<Output = R> + 'static,
        apply: impl FnOnce(R, &mut ListState<D>, &mut Window, &mut Context<ListState<D>>) + 'static,
        cx: &mut App,
    ) {
        let task = self.list.update(cx, |_, cx| {
            cx.spawn(async move |list, cx| {
                let reply = reply.await;
                let _ = window.update(cx, |_, window, cx| {
                    let _ = list.update(cx, |list, cx| apply(reply, list, window, cx));
                });
            })
        });
        self.requests.push(task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_picker::ModelPickerDelegate;
    use crate::workspace::modals::ModelPickerReply;
    use futures::channel::oneshot;
    use gpui::{AppContext, Empty, TestAppContext};
    use horizon_agent::wire::ProviderSummary;
    use std::cell::Cell;
    use std::rc::Rc;

    #[gpui::test]
    fn closing_and_reopening_cancels_old_delivery_and_subscription(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|_, _| Empty);
        let subscriptions_dropped = Rc::new(Cell::new(0));
        let open = |cx: &mut TestAppContext| {
            let dropped = subscriptions_dropped.clone();
            window
                .update(cx, |_, window, cx| {
                    let list = cx.new(|cx| ListState::new(ModelPickerDelegate::new(), window, cx));
                    ListModal::new(
                        list,
                        (),
                        Subscription::new(move || dropped.set(dropped.get() + 1)),
                    )
                })
                .unwrap()
        };
        let (old_tx, old_rx) = oneshot::channel();
        let mut old = open(cx);
        let old_list = old.list.clone(); // A previous rendered frame can retain the entity.
        cx.update(|cx| {
            old.receive(
                window.into(),
                old_rx,
                |reply, list, _, _| {
                    ModelPickerReply::Providers(reply.unwrap())
                        .apply(list.delegate_mut().state_mut());
                },
                cx,
            )
        });
        cx.executor().run_until_parked();
        drop(old);
        assert_eq!(subscriptions_dropped.get(), 1);
        let (current_tx, current_rx) = oneshot::channel();
        let mut current = open(cx);
        cx.update(|cx| {
            current.receive(
                window.into(),
                current_rx,
                |reply, list, _, _| {
                    ModelPickerReply::Providers(reply.unwrap())
                        .apply(list.delegate_mut().state_mut());
                },
                cx,
            )
        });
        cx.executor().run_until_parked();
        let providers = |name: &str| {
            vec![ProviderSummary {
                name: name.into(),
                base_url: None,
                api_key_env: String::new(),
                default_model: None,
                available: true,
                default: true,
            }]
        };
        old_tx.send(providers("stale")).ok();
        current_tx.send(providers("current")).unwrap();
        cx.executor().run_until_parked();
        old_list.read_with(cx, |list, _| assert!(list.delegate().state().is_loading()));
        current.list.read_with(cx, |list, _| {
            assert_eq!(list.delegate().state().providers()[0].name, "current");
        });
        drop(current);
        assert_eq!(subscriptions_dropped.get(), 2);
    }
}
