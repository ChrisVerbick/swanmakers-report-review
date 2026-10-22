//! The share page: who you are, the documents under review and their versions, and the ways
//! in (a sign-in link by mail for invitees, a name and password for guests of a public link).

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;
use serde_json::{Value, json};

use crate::api::{self, Failure, SharePage as Page};
use crate::dom;

#[component]
pub fn SharePage() -> impl IntoView {
    let params = use_params_map();
    let token = Memo::new(move |_| params.read().get("token").unwrap_or_default());
    let page = RwSignal::new(None::<Result<Page, Failure>>);
    let reload = move || {
        let t = token.get_untracked();
        spawn_local(async move {
            page.set(Some(api::get::<Page>(&api::api(&t)).await));
        });
    };
    Effect::new(move |_| {
        token.track();
        reload();
    });
    view! {
        <main class="narrow">
            {move || match page.get() {
                None => view! { <p class="muted">"Loading…"</p> }.into_any(),
                Some(Ok(p)) => view! { <Documents token=token.get() page=p reload=reload /> }.into_any(),
                Some(Err(Failure::Login(l))) => view! { <h1>{l.project}</h1><RequestLink token=token.get() /> }.into_any(),
                Some(Err(e)) => view! { <h1>"Review"</h1><p class="error">{e.to_string()}</p> }.into_any(),
            }}
        </main>
    }
}

#[component]
fn Documents(token: String, page: Page, reload: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let t = token.clone();
    let signout = move |_| {
        spawn_local(async move {
            let _ = api::send::<Value>("POST", "/api/auth/logout", &json!({})).await;
            reload();
        });
    };
    let me = page.me.clone();
    let join = (page.mode == "public" && me.is_none()).then(|| view! { <Join token=t.clone() password=page.password reload=reload /> });
    view! {
        <header class="title">
            <p class="kicker">{page.project.client.clone()}</p>
            <h1>{page.project.name.clone()}</h1>
            {me.as_ref().map(|m| view! { <p class="muted">{m.name.clone()} " · " {m.email.clone()} " · " <button class="link" on:click=signout>"Sign out"</button></p> })}
        </header>
        {join}
        <section>
            <h2>"Documents"</h2>
            {page.documents.iter().map(|d| {
                let latest = d.versions.last().cloned();
                let t = token.clone();
                let download = page.allow_download;
                view! {
                    <article class="doc">
                        <h3>{d.title.clone()}</h3>
                        <ul class="versions">
                            {d.versions.iter().rev().map(|v| {
                                let is_latest = latest.as_ref().is_some_and(|l| l.id == v.id);
                                let href = format!("/r/{t}/d/{}?v={}", d.id, v.id);
                                let pdf = format!("/api/r/{t}/v/{}/document.pdf", v.id);
                                view! {
                                    <li>
                                        <a href=href class:latest=is_latest>{if v.label.is_empty() { format!("Version {}", v.n) } else { v.label.clone() }}</a>
                                        <span class="muted">" · " {v.pages} " pages · " {dom::date(v.uploaded)} {if is_latest { " · latest" } else { "" }}</span>
                                        {download.then(|| view! { <a class="muted" href=pdf download="">" · PDF"</a> })}
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    </article>
                }
            }).collect_view()}
            {page.documents.is_empty().then(|| view! { <p class="muted">"Nothing to review yet."</p> })}
        </section>
    }
}

/// Invite-only: ask for a sign-in link by mail.
#[component]
fn RequestLink(token: String) -> impl IntoView {
    let token = StoredValue::new(token);
    let email = RwSignal::new(String::new());
    let state = RwSignal::new(None::<Result<(), String>>);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let (t, e) = (token.get_value(), email.get());
        spawn_local(async move {
            let r = api::send::<Value>("POST", "/api/auth/request", &json!({"email": e, "link": t})).await.map(|_| ()).map_err(|e| e.to_string());
            state.set(Some(r));
        });
    };
    view! {
        <section class="card">
            <h2>"Sign in"</h2>
            {move || match state.get() {
                Some(Ok(())) => view! { <p>"If that address is on the review, a sign-in link is on its way. It works once and expires in 15 minutes."</p> }.into_any(),
                other => view! {
                    <p>"This review is by invitation. Enter the address you were invited with and we'll mail you a link."</p>
                    <form on:submit=submit>
                        <label>"E-mail" <input type="email" required=true autocomplete="email" prop:value=move || email.get() on:input=move |ev| email.set(event_target_value(&ev)) /></label>
                        <button type="submit">"Send me a link"</button>
                        {other.and_then(|r| r.err()).map(|e| view! { <p class="error">{e}</p> })}
                    </form>
                }.into_any(),
            }}
        </section>
    }
}

/// Public link: a name to sign comments with, and the link's password when it has one.
#[component]
fn Join(token: String, password: bool, reload: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let token = StoredValue::new(token);
    let name = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let pw = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let t = token.get_value();
        let body = json!({"link": t, "name": name.get(), "email": email.get(), "password": if password { Some(pw.get()) } else { None }});
        spawn_local(async move {
            match api::send::<Value>("POST", "/api/auth/guest", &body).await {
                Ok(_) => reload(),
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };
    view! {
        <section class="card">
            <h2>"Join to comment"</h2>
            <form on:submit=submit>
                <label>"Name" <input required=true autocomplete="name" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) /></label>
                <label>"E-mail" <input type="email" required=true autocomplete="email" prop:value=move || email.get() on:input=move |ev| email.set(event_target_value(&ev)) /></label>
                {password.then(|| view! { <label>"Password" <input type="password" required=true prop:value=move || pw.get() on:input=move |ev| pw.set(event_target_value(&ev)) /></label> })}
                <button type="submit">"Join"</button>
                {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
            </form>
        </section>
    }
}
