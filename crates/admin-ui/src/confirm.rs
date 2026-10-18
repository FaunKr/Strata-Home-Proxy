//! A confirmation dialog inside the page, instead of the browser's
//! `confirm()`. One dialog serves the whole frame; a button asks through
//! [`Confirm::ask`] and its action runs only on the confirming button.
//! "Cancel", Escape and closing the dialog do nothing.

use leptos::{html::Dialog, prelude::*};

/// What the dialog asks.
#[derive(Clone)]
pub struct Question {
    pub title: String,
    pub text: String,
    /// The confirming button, e.g. "Revoke key".
    pub confirm: &'static str,
    /// The action destroys something; the button shows it in red.
    pub danger: bool,
}

/// Asks through the frame's dialog.
#[derive(Clone, Copy)]
pub struct Confirm {
    open: RwSignal<Option<(Question, Callback<()>)>>,
}

impl Confirm {
    /// Made once by the frame, for everything below it.
    pub fn provide() -> Self {
        let confirm = Self {
            open: RwSignal::new(None),
        };
        provide_context(confirm);
        confirm
    }

    /// Shows `question`; `then` runs only when it is confirmed.
    pub fn ask(&self, question: Question, then: impl Fn() + Send + Sync + 'static) {
        self.open
            .set(Some((question, Callback::new(move |()| then()))));
    }
}

/// The dialog that `confirm` opens.
#[component]
pub fn ConfirmDialog(confirm: Confirm) -> impl IntoView {
    let dialog = NodeRef::<Dialog>::new();
    Effect::new(move |_| {
        let Some(d) = dialog.get() else { return };
        if confirm.open.with(Option::is_some) {
            if !d.open() {
                let _ = d.show_modal();
            }
        } else if d.open() {
            d.close();
        }
    });
    let question = move || confirm.open.with(|o| o.as_ref().map(|(q, _)| q.clone()));
    let on_confirm = move |_| {
        if let Some((_, then)) = confirm.open.get_untracked() {
            confirm.open.set(None);
            then.run(());
        }
    };
    view! {
        <dialog
            class="confirm"
            node_ref=dialog
            aria-labelledby="confirm-title"
            aria-describedby="confirm-text"
            on:close=move |_| confirm.open.set(None)
        >
            <h2 id="confirm-title">{move || question().map(|q| q.title)}</h2>
            <p id="confirm-text" class="muted">
                {move || question().map(|q| q.text)}
            </p>
            <div class="actions">
                <button
                    type="button"
                    class="secondary"
                    autofocus
                    on:click=move |_| confirm.open.set(None)
                >
                    "Cancel"
                </button>
                <button
                    type="button"
                    class=move || {
                        if question().is_some_and(|q| q.danger) { "danger" } else { "" }
                    }
                    on:click=on_confirm
                >
                    {move || question().map(|q| q.confirm)}
                </button>
            </div>
        </dialog>
    }
}
