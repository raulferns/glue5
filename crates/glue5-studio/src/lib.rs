//! GLUE 5 Studio: the mods' IDE, served by the game. Leptos for the shell,
//! CodeMirror for the code (editor.rs), one WebSocket to the game for
//! files, mods, logs and eval (`/studio`) and one for the type checker
//! (`/lsp`, from the editor).

mod actions;
mod chrome;
mod editor;
mod icons;
mod menus;
mod net;
mod panel;
mod sidebar;
mod state;
mod workspace;

use std::collections::HashMap;

use leptos::prelude::*;
use serde_json::Value;
use wasm_bindgen::prelude::*;

use chrome::{Activity, PhoneNav, StatusBar, TitleBar};
use editor::StudioEditor;
use actions::save;
use net::connect;
use panel::BottomPanel;
use sidebar::{ModsView, NativesView};
use state::*;
use workspace::{Tabs, Toolbar, Welcome};

#[wasm_bindgen(start)]
pub fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let s = S {
        pairing: RwSignal::new(Pairing::Connecting),
        connected: RwSignal::new(false),
        hello: RwSignal::new(Value::Null),
        mods: RwSignal::new(Vec::new()),
        menus: RwSignal::new(Vec::new()),
        rows: RwSignal::new(Vec::new()),
        tabs: RwSignal::new(Vec::new()),
        active: RwSignal::new(None),
        expanded: RwSignal::new(Vec::new()),
        side: RwSignal::new(Side::Mods),
        panel: RwSignal::new(Panel::Console),
        screen: RwSignal::new(Screen::Mods),
        drawer: RwSignal::new(false),
        diags: RwSignal::new(HashMap::new()),
        cursor: RwSignal::new((1, 1)),
        restart_on_save: RwSignal::new(storage().and_then(|s| s.get_item("glue5-restart").ok().flatten()).as_deref() != Some("0")),
        filter: RwSignal::new(Filter::All),
        query: RwSignal::new(String::new()),
        found: RwSignal::new(Vec::new()),
        palette: RwSignal::new(false),
        notice: RwSignal::new(None),
    };
    // The code: from the address (the game's QR code), else remembered.
    let page = web_sys::window().map(|w| w.location());
    let from_url = page
        .as_ref()
        .and_then(|l| l.search().ok())
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("token"));
    let token = from_url.clone().or_else(|| storage().and_then(|s| s.get_item("glue5-token").ok().flatten()));
    TOKEN.with(|t| *t.borrow_mut() = token.unwrap_or_default());
    if from_url.is_some() {
        // Out of the address bar (and screenshots of it).
        if let Some(h) = web_sys::window().and_then(|w| w.history().ok()) {
            let _ = h.replace_state_with_url(&JsValue::NULL, "", Some("/"));
        }
    }
    connect(s);
    Effect::new(move |_| {
        let on = s.restart_on_save.get();
        if let Some(st) = storage() {
            let _ = st.set_item("glue5-restart", if on { "1" } else { "0" });
        }
    });
    Effect::new(move |_| {
        if let Some(n) = s.notice.get() {
            let _ = n;
            set_timeout(move || s.notice.set(None), std::time::Duration::from_secs(5));
        }
    });

    view! {
        <div class="backdrop"></div>
        {move || match s.pairing.get() {
            Pairing::Connecting => view! { <div class="splash"><img src="/logo.png" alt=""/></div> }.into_any(),
            Pairing::Needed => view! { <Pair s=s/> }.into_any(),
            Pairing::Paired => view! { <Shell s=s/> }.into_any(),
        }}
    }
}

#[component]
fn Pair(s: S) -> impl IntoView {
    let code = RwSignal::new(String::new());
    let submit = move || {
        let c: String = code.get_untracked().chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
        if c.is_empty() {
            return;
        }
        TOKEN.with(|t| *t.borrow_mut() = c);
        s.pairing.set(Pairing::Connecting);
        connect(s);
    };
    view! {
        <div class="pair">
            <div class="pair-card">
                <div class="brand big"><img src="/logo.png" alt=""/><b>"GLUE 5"</b><span>"STUDIO"</span></div>
                <p>"Enter the code shown in the game, under Mods › Studio."</p>
                <input class="code-input" placeholder="ABCD-1234" autocomplete="off" autocapitalize="characters" spellcheck="false"
                    prop:value=move || code.get()
                    on:input=move |ev| code.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" { submit() } />
                <button class="btn primary wide" on:click=move |_| submit()>"Connect"</button>
            </div>
        </div>
    }
}

#[component]
fn Shell(s: S) -> impl IntoView {
    let host_ref = NodeRef::<leptos::html::Div>::new();
    // The editor, made once its element is in the page; after pairing again
    // (the game started anew, with a new code) the page is made again and
    // the same editor moves into it.
    Effect::new(move |_| {
        let Some(el) = host_ref.get() else { return };
        let token = TOKEN.with(|t| t.borrow().clone());
        let lsp = format!("ws://{}/lsp?token={token}", host());
        if EDITOR.with(|e| e.borrow().is_some()) {
            with_editor(|e| e.attach(&el, &lsp));
            return;
        }
        let editor = StudioEditor::new(&el, &lsp);
        let on_change = Closure::<dyn FnMut(String, bool)>::new(move |path: String, dirty: bool| {
            s.tabs.update(|t| {
                if let Some(t) = t.iter_mut().find(|t| t.path == path) {
                    t.dirty = dirty;
                }
            });
        });
        editor.on_change(&on_change);
        on_change.forget();
        let on_save = Closure::<dyn FnMut(String)>::new(move |path: String| save(s, path));
        editor.on_save(&on_save);
        on_save.forget();
        let on_diags = Closure::<dyn FnMut(String, String)>::new(move |path: String, json: String| {
            let list: Vec<Diag> = serde_json::from_str(&json).unwrap_or_default();
            s.diags.update(|d| {
                d.insert(path, list);
            });
        });
        editor.on_diagnostics(&on_diags);
        on_diags.forget();
        let on_cursor = Closure::<dyn FnMut(u32, u32)>::new(move |l, c| s.cursor.set((l, c)));
        editor.on_cursor(&on_cursor);
        on_cursor.forget();
        EDITOR.with(|e| *e.borrow_mut() = Some(editor));
    });
    let screen_class = move || {
        let screen = match s.screen.get() {
            Screen::Mods => "screen-mods",
            Screen::Editor => "screen-editor",
            Screen::Console => "screen-console",
            Screen::Problems => "screen-problems",
            Screen::Menus => "screen-menus",
        };
        format!("app {screen}{}", if s.drawer.get() { " drawer" } else { "" })
    };
    view! {
        <div class=screen_class>
            <TitleBar s=s/>
            <div class="main">
                <Activity s=s/>
                <aside class="sidebar">
                    {move || match s.side.get() {
                        Side::Mods => view! { <ModsView s=s/> }.into_any(),
                        Side::Natives => view! { <NativesView s=s/> }.into_any(),
                    }}
                </aside>
                <section class="work">
                    <Tabs s=s/>
                    <Toolbar s=s/>
                    <div class="code">
                        <div class="editor-host" node_ref=host_ref></div>
                        <Show when=move || s.tabs.with(|t| t.is_empty())>
                            <Welcome s=s/>
                        </Show>
                    </div>
                    <BottomPanel s=s/>
                </section>
            </div>
            <StatusBar s=s/>
            <PhoneNav s=s/>
            {move || s.notice.get().map(|n| view! { <div class="toast">{n}</div> })}
        </div>
    }
}
