//! The reviewer's browser client: the share page (the documents under review, signing in) and
//! the viewer (pages with an invisible, selectable text layer; comments as markers and a panel).
//! Rust compiled to WebAssembly with Leptos; no JavaScript of its own.
#![forbid(unsafe_code)]

mod anchor;
mod api;
mod dom;
mod share;
mod viewer;

use leptos::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;

#[component]
fn App() -> impl IntoView {
    view! {
        <Router>
            <Routes fallback=|| view! { <main class="narrow"><h1>"DesignCraft Review"</h1><p>"Open the review link you were sent."</p></main> }>
                <Route path=path!("/r/:token") view=share::SharePage />
                <Route path=path!("/r/:token/d/:document") view=viewer::Viewer />
            </Routes>
        </Router>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
