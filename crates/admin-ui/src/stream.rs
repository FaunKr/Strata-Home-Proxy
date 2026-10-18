//! A live stream of the proxy in the browser (server-sent events), followed
//! while the page that opened it is shown.

use std::{cell::RefCell, rc::Rc, time::Duration};

use leptos::{
    prelude::*,
    task::spawn_local,
    wasm_bindgen::{JsCast, closure::Closure},
};
use send_wrapper::SendWrapper;
use web_sys::{EventSource, MessageEvent};

use crate::api::{Session, get_session};

const RETRY: Duration = Duration::from_secs(5);

/// The open stream with its handlers, dropped together.
struct Open {
    source: EventSource,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut()>,
}

enum Connection {
    /// Shown, with the stream if one is open.
    Shown(Option<Open>),
    /// The page is gone: no new connection, and no signals to set.
    Gone,
}

/// Browser-only state, shared with the handlers.
type Shared = Rc<RefCell<Connection>>;

/// Hands each message of `url` to `on_data`, and sets `lost` while the
/// connection is down. The browser reconnects by itself after a dropped
/// connection; after a refused one it tries again every few seconds, or
/// shows the login when the session has ended.
pub fn follow(url: &'static str, on_data: impl Fn(String) + 'static, lost: RwSignal<bool>) {
    let shared: Shared = Rc::new(RefCell::new(Connection::Shown(None)));
    let on_data: Rc<dyn Fn(String)> = Rc::new(on_data);
    open(url, on_data, lost, Rc::clone(&shared));
    let shared = SendWrapper::new(shared);
    on_cleanup(move || {
        let gone = std::mem::replace(&mut *shared.borrow_mut(), Connection::Gone);
        if let Connection::Shown(Some(o)) = gone {
            o.source.close();
        }
    });
}

fn open(url: &'static str, on_data: Rc<dyn Fn(String)>, lost: RwSignal<bool>, shared: Shared) {
    let mut slot = shared.borrow_mut();
    let Connection::Shown(current) = &mut *slot else {
        return;
    };
    if let Some(old) = current.take() {
        old.source.close();
    }
    let Ok(source) = EventSource::new(url) else {
        return;
    };
    let handler = Rc::clone(&on_data);
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |ev: MessageEvent| {
        if let Some(text) = ev.data().as_string() {
            lost.set(false);
            handler(text);
        }
    });
    source.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    let watched = source.clone();
    let again = Rc::clone(&shared);
    let on_error = Closure::<dyn FnMut()>::new(move || {
        if matches!(*again.borrow(), Connection::Gone) {
            return;
        }
        lost.set(true);
        if watched.ready_state() == EventSource::CLOSED {
            let again = Rc::clone(&again);
            let on_data = Rc::clone(&on_data);
            spawn_local(async move {
                match get_session().await {
                    Ok(Session::Anonymous | Session::MustChange) => {
                        let _ = window().location().reload();
                    }
                    _ => set_timeout(move || open(url, on_data, lost, again), RETRY),
                }
            });
        }
    });
    source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    *current = Some(Open {
        source,
        _on_message: on_message,
        _on_error: on_error,
    });
}
