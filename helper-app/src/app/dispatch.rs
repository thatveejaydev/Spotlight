//! UI-thread callbacks that timers, hooks and worker threads reach.
//!
//! Each slot is a thread-local on the UI thread. Wiring code stores a closure
//! with [`UiSlot::set`]; a UI-thread caller runs it with [`UiSlot::call`], and
//! another thread runs it through the event loop with [`post`].

use std::{cell::RefCell, thread::LocalKey};

type Callback<A> = Box<dyn FnMut(A)>;

pub(crate) struct UiSlot<A: 'static>(RefCell<Option<Callback<A>>>);

impl<A: 'static> UiSlot<A> {
    pub(crate) const fn new() -> Self {
        Self(RefCell::new(None))
    }

    pub(crate) fn set(&self, callback: impl FnMut(A) + 'static) {
        *self.0.borrow_mut() = Some(Box::new(callback));
    }

    /// Runs the stored callback, if any.
    pub(crate) fn call(&self, arg: A) {
        if let Some(callback) = self.0.borrow_mut().as_mut() {
            callback(arg);
        }
    }

    /// Runs the stored callback once and clears the slot.
    pub(crate) fn call_once(&self, arg: A) {
        let callback = self.0.borrow_mut().take();
        if let Some(mut callback) = callback {
            callback(arg);
        }
    }
}

/// Runs `slot`'s callback on the UI thread. Returns false once the event loop is gone.
pub(crate) fn post<A: Send + 'static>(slot: &'static LocalKey<UiSlot<A>>, arg: A) -> bool {
    slint::invoke_from_event_loop(move || slot.with(|slot| slot.call(arg))).is_ok()
}

#[cfg(test)]
mod tests {
    use super::UiSlot;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn slots_run_the_latest_callback_and_call_once_clears_it() {
        let slot = UiSlot::<u32>::new();
        slot.call(1); // Nothing stored yet.
        let seen = Rc::new(Cell::new(0));
        let sink = seen.clone();
        slot.set(move |value| sink.set(sink.get() + value));
        slot.call(2);
        slot.call(3);
        assert_eq!(seen.get(), 5);
        slot.call_once(10);
        slot.call(100);
        assert_eq!(seen.get(), 15);
    }
}
