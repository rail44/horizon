//! A frame: the effects of what was written, a layout, the work that
//! follows a layout, and again while that work asks for it.

use std::rc::Rc;

use any_spawner::Executor;
use blitz_dom::BaseDocument;

use crate::borders::restore_thin_borders;
use crate::input_method::place_input_method;
use crate::shared::{Shared, current, enter, shared};

/// How many times a frame lays out before it is painted as it is.
const LAYOUTS_A_FRAME: usize = 8;

/// Runs the effects of the signals written so far, and the changes to the
/// document that were left for when it is free.
pub(crate) fn run_effects(shared: &Rc<Shared>) {
    enter(shared);
    poll_effects(shared);
    loop {
        let later = std::mem::take(&mut *shared.later.borrow_mut());
        if later.is_empty() {
            break;
        }
        for work in later {
            work(&mut shared.document.borrow_mut());
        }
        poll_effects(shared);
    }
}

/// This thread's effects, and those run alongside them.
fn poll_effects(shared: &Shared) {
    Executor::poll_local();
    // Taken out while it runs: work may register more, or retire some.
    let mut work = std::mem::take(&mut *shared.effects.borrow_mut());
    work.retain(|(id, _)| !shared.retired.borrow_mut().remove(id));
    for (_, work) in &mut work {
        work();
    }
    work.append(&mut shared.effects.borrow_mut());
    *shared.effects.borrow_mut() = work;
}

/// Runs `work` whenever the signals' effects run, with the document free
/// to change, for as long as what it was made under lasts: how a graph
/// that is not this thread's (a plugin's) has its effects run at the
/// same times.
pub fn each_effects(work: impl FnMut() + 'static) {
    let id = crate::shared::fresh_id();
    shared().effects.borrow_mut().push((id, Box::new(work)));
    if reactive_graph::owner::Owner::current().is_some() {
        reactive_graph::owner::on_cleanup(move || {
            if let Some(shared) = current() {
                shared.retired.borrow_mut().insert(id);
            }
        });
    }
}

/// From [`each_layout`](crate::each_layout) work: what it wrote changes
/// the layout it has just read, and the document is laid out again before
/// it is painted.
pub fn relayout() {
    shared().relayout.set(true);
}

/// Lays the current app's document out for a frame at the animation clock
/// `seconds`: what a shell calls before it paints, in place of resolving
/// the document itself.
pub fn frame(seconds: f64) {
    if let Some(shared) = current() {
        frame_of(&shared, seconds);
    }
}

pub(crate) fn frame_of(shared: &Rc<Shared>, seconds: f64) {
    enter(shared);
    for _ in 0..LAYOUTS_A_FRAME {
        {
            let mut document = shared.document.borrow_mut();
            document.resolve(seconds);
            restore_thin_borders(&mut document);
            after_layout(shared, &mut document);
        }
        if !shared.relayout.take() {
            break;
        }
        run_effects(shared);
    }
    before_paint(shared, &mut shared.document.borrow_mut());
}

/// The work that follows a layout, and the changes it asked for.
fn after_layout(shared: &Shared, document: &mut BaseDocument) {
    // Taken out while it runs: work may register more, or retire some.
    let mut work = std::mem::take(&mut *shared.laid_out.borrow_mut());
    work.retain(|(id, _)| !shared.retired.borrow_mut().remove(id));
    for (_, work) in &mut work {
        work(document);
    }
    work.append(&mut shared.laid_out.borrow_mut());
    *shared.laid_out.borrow_mut() = work;
    // What that work asked to change (a scroll): nothing an effect waits
    // on, and not something to leave for the next frame.
    let later = std::mem::take(&mut *shared.later.borrow_mut());
    for change in later {
        change(document);
    }
}

/// What follows the last layout before a paint.
fn before_paint(shared: &Shared, document: &mut BaseDocument) {
    if let Some((element, within)) = shared.focus.take() {
        let mut pending = vec![element];
        while let Some(id) = pending.pop() {
            let Some(node) = document.get_node(id) else {
                continue;
            };
            let field =
                (node.element_data()).is_some_and(|element| element.text_input_data().is_some());
            if !within || field || node.attr(blitz_dom::local_name!("tabindex")).is_some() {
                document.set_focus_to(id);
                break;
            }
            pending.extend(node.children.iter().rev().copied());
        }
    }
    place_input_method(shared, document);
}
