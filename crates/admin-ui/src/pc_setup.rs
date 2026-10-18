//! The page "PC-Dienst einrichten": the one-line command for the PC.

use leptos::prelude::*;

use crate::api::{NewPcSetupCommand, error_text};

#[component]
pub fn PcSetupPage() -> impl IntoView {
    let create = ServerAction::<NewPcSetupCommand>::new();
    let shown = move || {
        create.value().get().map(|result| match result {
            Ok(c) => view! {
                <section class="panel created" role="status">
                    <h2>"Befehl für den PC"</h2>
                    <label for="pc-setup-command">"In eine PowerShell auf dem PC einfügen"</label>
                    <input id="pc-setup-command" type="text" readonly value=c.command />
                    <p class="hint">
                        {format!(
                            "Gilt {} Minuten und nur für einen PC. Windows fragt einmal nach Administratorrechten. Danach trägt der Proxy Token und Adresse des PC-Dienstes selbst ein.",
                            c.valid_minutes,
                        )}
                    </p>
                    <label for="pc-uninstall-command">"Zum Entfernen vom PC"</label>
                    <input id="pc-uninstall-command" type="text" readonly value=c.uninstall_command />
                </section>
            }
            .into_any(),
            Err(e) => view! { <p class="error" role="alert">{error_text(&e)}</p> }.into_any(),
        })
    };
    view! {
        <h1>"PC-Dienst einrichten"</h1>
        <p class="hint">
            "Der PC-Dienst legt den PC schlafen, schaltet den Gaming-Modus und startet Strata. Der Befehl lädt ihn vom NAS, richtet ihn ein und meldet ihn hier an. Läuft er schon, tauscht ein neuer Befehl das Programm aus und meldet den PC neu an."
        </p>
        {shown}
        <section class="panel">
            <ActionForm action=create>
                <button type="submit">"Befehl erzeugen"</button>
            </ActionForm>
            <p class="hint">"Ein neuer Befehl macht den vorigen ungültig."</p>
        </section>
    }
}
