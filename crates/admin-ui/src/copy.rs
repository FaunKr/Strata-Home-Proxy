//! A read-only field with a button that copies its text.

use leptos::{html::Input, prelude::*};

/// The text in a read-only field, and a "Copy" button next to it. It copies
/// by selecting the field, so it also works on a plain `http://` address,
/// where the browser offers no clipboard API.
#[component]
pub fn CopyField(id: &'static str, label: &'static str, text: String) -> impl IntoView {
    let field = NodeRef::<Input>::new();
    // Some(true) after copying, Some(false) when the browser refused.
    let copied = RwSignal::new(None::<bool>);
    let on_copy = move |_| {
        if let Some(input) = field.get() {
            input.select();
            copied.set(Some(copy_selection()));
        }
    };
    view! {
        <div>
        <label for=id>{label}</label>
        <div class="code-row">
            <input id=id class="code" type="text" readonly value=text node_ref=field />
            <button type="button" class="secondary" on:click=on_copy>
                {move || if copied.get() == Some(true) { "Copied" } else { "Copy" }}
            </button>
        </div>
        {move || {
            (copied.get() == Some(false))
                .then(|| view! { <p class="hint">"The text is selected; copy it with Ctrl+C."</p> })
        }}
        </div>
    }
}

#[cfg(feature = "hydrate")]
fn copy_selection() -> bool {
    use wasm_bindgen::JsCast;
    document()
        .dyn_into::<web_sys::HtmlDocument>()
        .ok()
        .and_then(|d| d.exec_command("copy").ok())
        .unwrap_or(false)
}

#[cfg(not(feature = "hydrate"))]
fn copy_selection() -> bool {
    false
}
