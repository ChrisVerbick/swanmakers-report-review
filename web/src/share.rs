//! The share page: who you are, the documents under review and their versions, and the ways
//! in (a sign-in link by mail for invitees, a name and password for guests of a public link).

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_params_map;
use serde_json::{Value, json};

use crate::api::{self, Failure, Round, SharePage as Page};
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
                Some(Err(Failure::Login(l))) => view! { <h1>{l.project}</h1><RequestLink token=token.get() turnstile=l.turnstile /> }.into_any(),
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
    let join = (page.mode == "public" && me.is_none())
        .then(|| view! { <Join token=t.clone() password=page.password turnstile=page.turnstile.clone() reload=reload /> });
    view! {
        <header class="title">
            <p class="kicker">{page.project.client.clone()}</p>
            <h1>{page.project.name.clone()}</h1>
            {me.as_ref().map(|m| view! { <p class="muted">{m.name.clone()} " · " {m.email.clone()} " · " <button class="link" on:click=signout>"Sign out"</button></p> })}
        </header>
        {join}
        {(!page.rounds.is_empty()).then(|| view! {
            <section>
                <h2>"Rounds"</h2>
                {page.rounds.iter().rev().map(|r| {
                    let t = token.clone();
                    let signed_in = me.is_some();
                    view! {
                        <article class="doc">
                            <h3>{round_title(r)}</h3>
                            <p class="muted">{round_status(r)}</p>
                            {(!r.note.is_empty()).then(|| view! { <p>{r.note.clone()}</p> })}
                            <ul class="versions">
                                {r.documents.iter().map(|d| {
                                    let href = format!("/r/{t}/d/{}?v={}", d.document, d.version);
                                    let decided: Vec<String> = r.decisions.iter().filter(|x| x.document == d.document).map(|x| format!("{}{}: {}", x.name, if x.role == "guest" { " (guest)" } else { "" }, decision_word(&x.value))).collect();
                                    view! {
                                        <li>
                                            <a href=href>{d.title.clone()}</a>
                                            <span class="muted">" · " {if d.label.is_empty() { format!("Version {}", d.n) } else { d.label.clone() }}</span>
                                            {(!decided.is_empty()).then(|| view! { <span class="muted">" · " {decided.join(" · ")}</span> })}
                                            {(signed_in && r.locked.is_none()).then(|| view! { <Decide token=t.clone() round=r.clone() document=d.document.clone() reload=reload /> })}
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                        </article>
                    }
                }).collect_view()}
            </section>
        })}
        <section>
            <h2>{if page.rounds.is_empty() { "Documents" } else { "All versions" }}</h2>
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

pub fn round_title(r: &Round) -> String {
    if r.name == format!("Round {}", r.n) { r.name.clone() } else { format!("Round {} · {}", r.n, r.name) }
}

/// "due 2026-10-30", "locked 2026-11-02", or "open".
pub fn round_status(r: &Round) -> String {
    match (r.locked, r.due) {
        (Some(at), _) => format!("Locked {}", dom::date(at)),
        (None, Some(due)) => format!("Open · due {}", dom::date(due)),
        (None, None) => "Open".into(),
    }
}

pub fn decision_word(value: &str) -> &'static str {
    if value == "approved" { "approved" } else { "changes wanted" }
}

/// Approve, or ask for changes with a note; shown as my decision once made (it can be changed
/// while the round is open).
#[component]
pub fn Decide(token: String, round: Round, document: String, reload: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let token = StoredValue::new(token);
    let round_id = StoredValue::new(round.id.clone());
    let document = StoredValue::new(document.clone());
    let mine = round.mine.get(&document.get_value()).cloned();
    let asking = RwSignal::new(false);
    let note = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let send = move |value: &'static str| {
        let body = json!({"round": round_id.get_value(), "document": document.get_value(), "value": value, "note": note.get()});
        let t = token.get_value();
        spawn_local(async move {
            match api::send::<Value>("POST", &format!("{}/decisions", api::api(&t)), &body).await {
                Ok(_) => reload(),
                Err(e) => error.set(Some(e.to_string())),
            }
        });
    };
    view! {
        <span class="decide">
            {mine.as_ref().map(|m| view! { <span class="mine">" · you: " {decision_word(m)}</span> })}
            {move || if asking.get() {
                view! {
                    <input placeholder="What should change?" prop:value=move || note.get() on:input=move |ev| note.set(event_target_value(&ev)) />
                    <button on:click=move |_| send("changes")>"Send"</button>
                    <button class="link" on:click=move |_| asking.set(false)>"Cancel"</button>
                }.into_any()
            } else {
                view! {
                    {(mine.as_deref() != Some("approved")).then(|| view! { <button class="small" on:click=move |_| send("approved")>"Approve"</button> })}
                    {(mine.as_deref() != Some("changes")).then(|| view! { <button class="small" on:click=move |_| asking.set(true)>"Request changes"</button> })}
                }.into_any()
            }}
            {move || error.get().map(|e| view! { <span class="error">" " {e}</span> })}
        </span>
    }
}

/// Invite-only: ask for a sign-in link by mail.
#[component]
fn RequestLink(token: String, turnstile: Option<String>) -> impl IntoView {
    let token = StoredValue::new(token);
    let email = RwSignal::new(String::new());
    let state = RwSignal::new(None::<Result<(), String>>);
    let challenge = RwSignal::new(String::new());
    let guarded = turnstile.is_some();
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let (t, e) = (token.get_value(), email.get());
        let mut body = json!({"email": e, "link": t});
        if guarded {
            body["turnstile"] = json!(challenge.get());
        }
        spawn_local(async move {
            let r = api::send::<Value>("POST", "/api/auth/request", &body).await.map(|_| ()).map_err(|e| e.to_string());
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
                        {turnstile.clone().map(|k| view! { <Turnstile site_key=k token=challenge /> })}
                        <button type="submit" class="primary">"Send me a link"</button>
                        {other.and_then(|r| r.err()).map(|e| view! { <p class="error">{e}</p> })}
                    </form>
                }.into_any(),
            }}
        </section>
    }
}

/// Public link: a name to sign comments with, and the link's password when it has one.
#[component]
fn Join(token: String, password: bool, turnstile: Option<String>, reload: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let token = StoredValue::new(token);
    let name = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let pw = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let challenge = RwSignal::new(String::new());
    let guarded = turnstile.is_some();
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let t = token.get_value();
        let mut body = json!({"link": t, "name": name.get(), "email": email.get(), "password": if password { Some(pw.get()) } else { None }});
        if guarded {
            body["turnstile"] = json!(challenge.get());
        }
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
                {turnstile.clone().map(|k| view! { <Turnstile site_key=k token=challenge /> })}
                <button type="submit" class="primary">"Join"</button>
                {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
            </form>
        </section>
    }
}

/// Cloudflare's Turnstile widget: its script renders into the div and hands a token to `token`.
#[component]
pub fn Turnstile(site_key: String, token: RwSignal<String>) -> impl IntoView {
    let node = NodeRef::<leptos::html::Div>::new();
    let key = StoredValue::new(site_key);
    Effect::new(move |_| {
        if let Some(el) = node.get() {
            dom::turnstile_render(&el, &key.get_value(), move |t| token.set(t));
        }
    });
    view! { <div class="turnstile" node_ref=node></div> }
}
