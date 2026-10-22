//! The viewer: the version's pages with an invisible, selectable text layer over each image;
//! a toolbar on the selection (Highlight · Comment · Strike · Replace); pins by a click; the
//! comments as markers on the page and as a panel with replies, resolve and filters.

use std::sync::Arc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use serde_json::{Value, json};

use crate::anchor::{self, Anchor};
use crate::api::{self, Comment, Failure, Manifest, SharePage, TextLayer};
use crate::dom;

#[derive(Clone, PartialEq)]
struct Loaded {
    token: String,
    page: SharePage,
    document: api::Document,
    version: api::Version,
    latest: bool,
    manifest: Manifest,
    text: TextLayer,
}

/// A live selection in a page.
#[derive(Clone, PartialEq)]
struct Sel {
    page: usize,
    anchor: Option<Anchor>,
    rects: Vec<[f64; 4]>,
    at: (f64, f64),
}

/// A comment being written.
#[derive(Clone, PartialEq)]
struct Draft {
    kind: &'static str,
    page: usize,
    anchor: Option<Anchor>,
    rects: Vec<[f64; 4]>,
    pin: Option<(f64, f64)>,
}

async fn load(token: String, document: String, want: Option<String>) -> Result<Arc<Loaded>, Failure> {
    let page = api::get::<SharePage>(&api::api(&token)).await?;
    let doc = page.documents.iter().find(|d| d.id == document).cloned().ok_or_else(|| Failure::Refused(404, "no such document".into()))?;
    let latest = doc.versions.last().cloned().ok_or_else(|| Failure::Refused(404, "no version yet".into()))?;
    let version = match want {
        Some(v) => doc.versions.iter().find(|x| x.id == v).cloned().ok_or_else(|| Failure::Refused(404, "no such version".into()))?,
        None => latest.clone(),
    };
    let base = format!("{}/v/{}", api::api(&token), version.id);
    let manifest = api::get::<Manifest>(&format!("{base}/manifest.json")).await?;
    let text = api::get::<TextLayer>(&format!("{base}/text.json")).await?;
    Ok(Arc::new(Loaded { token, page, document: doc, latest: version.id == latest.id, version, manifest, text }))
}

fn px(v: f64) -> String {
    format!("{v:.2}px")
}

#[component]
pub fn Viewer() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    let navigate = use_navigate();
    let token = Memo::new(move |_| params.read().get("token").unwrap_or_default());
    let doc_id = Memo::new(move |_| params.read().get("document").unwrap_or_default());
    let want_version = Memo::new(move |_| query.read().get("v"));
    let loaded = RwSignal::new(None::<Result<Arc<Loaded>, Failure>>);
    let comments = RwSignal::new(Vec::<Comment>::new());
    let zoom = RwSignal::new(1.0_f64);
    let sel = RwSignal::new(None::<Sel>);
    let draft = RwSignal::new(None::<Draft>);
    let body = RwSignal::new(String::new());
    let replacement = RwSignal::new(String::new());
    let selected = RwSignal::new(None::<String>);
    let filter = RwSignal::new("open");
    let notice = RwSignal::new(None::<String>);
    let tick = RwSignal::new(0u32);
    let bump = move || tick.update(|n| *n = n.wrapping_add(1));

    // The share page, then the version's bundle; the zoom fits the first page to the window.
    Effect::new(move |_| {
        let (t, d, v) = (token.get(), doc_id.get(), want_version.get());
        loaded.set(None);
        spawn_local(async move {
            let r = load(t, d, v).await;
            if let Ok(l) = &r {
                let width = dom::inner_width();
                let room = if width > 900.0 { width - 440.0 } else { width - 32.0 };
                let fit = (room / l.manifest.pages.first().map_or(612.0, |p| p.width)).clamp(0.4, 2.0);
                zoom.set((fit * 100.0).round() / 100.0);
            }
            loaded.set(Some(r));
        });
    });
    // The comments: when the bundle is there, after every write, and on every event.
    Effect::new(move |_| {
        tick.track();
        let Some((t, d)) = loaded.with(|l| l.as_ref().and_then(|r| r.as_ref().ok()).map(|l| (l.token.clone(), l.document.id.clone()))) else {
            return;
        };
        spawn_local(async move {
            if let Ok(list) = api::get::<Vec<Comment>>(&format!("{}/comments?document={d}", api::api(&t))).await {
                comments.set(list);
            }
        });
    });
    // Live: other reviewers' comments arrive as events.
    Effect::new(move |_| {
        let t = token.get();
        let es = StoredValue::new_local(dom::events(
            &format!("{}/events", api::api(&t)),
            &["comment.created", "comment.replied", "comment.updated", "version.uploaded"],
            bump,
        ));
        on_cleanup(move || es.update_value(|e| drop(e.take())));
    });
    // The text the reviewer selects, with its address.
    let listener = StoredValue::new_local(dom::on_document("selectionchange", move |_| {
        let Some(raw) = dom::read_selection(zoom.get_untracked()) else {
            if sel.with_untracked(Option::is_some) {
                sel.set(None);
            }
            return;
        };
        let anchor = loaded.with_untracked(|l| {
            let l = l.as_ref()?.as_ref().ok()?;
            let lines = &l.text.pages.iter().find(|p| p.index == raw.page)?.lines;
            let slice = lines.get(raw.a_line..=raw.b_line)?;
            anchor::anchor(slice, raw.a_char, raw.b_char)
        });
        sel.set(Some(Sel { page: raw.page, anchor, rects: raw.rects, at: raw.below }));
    }));
    on_cleanup(move || listener.update_value(|l| drop(l.take())));
    // Fit the invisible text to the page whenever the pages or the zoom change.
    Effect::new(move |_| {
        zoom.track();
        loaded.track();
        dom::next_frame(dom::fit_overlays);
    });
    // Comments written while offline go when the connection is back.
    spawn_local(async move {
        if api::flush().await > 0 {
            bump();
        }
    });
    let online = StoredValue::new_local(dom::on_window("online", move |_| {
        spawn_local(async move {
            if api::flush().await > 0 {
                notice.set(Some("Your saved comments were sent.".into()));
                bump();
            }
        });
    }));
    on_cleanup(move || online.update_value(|l| drop(l.take())));

    // Sending a comment, a reply or a status; what can't be sent is kept.
    let write = move |method: &'static str, url: String, payload: Value, done: String| {
        spawn_local(async move {
            match api::send::<Value>(method, &url, &payload).await {
                Ok(_) => {
                    if !done.is_empty() {
                        notice.set(Some(done));
                    }
                }
                Err(Failure::Offline) => {
                    api::enqueue(api::Pending { method: method.into(), url, body: payload });
                    notice.set(Some(Failure::Offline.to_string()));
                }
                Err(e) => notice.set(Some(e.to_string())),
            }
            bump();
        });
    };
    let submit_draft = move |d: Draft, text: String, repl: Option<String>| {
        let Some((t, doc, ver)) = loaded
            .with_untracked(|l| l.as_ref().and_then(|r| r.as_ref().ok()).map(|l| (l.token.clone(), l.document.id.clone(), l.version.id.clone())))
        else {
            return;
        };
        let payload = api::comment_body(&api::new_id(), &doc, &ver, d.kind, d.page, d.anchor.as_ref(), &d.rects, d.pin, &text, repl.as_deref());
        write("POST", format!("{}/comments", api::api(&t)), payload, String::new());
        draft.set(None);
        body.set(String::new());
        replacement.set(String::new());
        dom::clear_selection();
        sel.set(None);
    };
    let start_draft = move |kind: &'static str| {
        let Some(s) = sel.get_untracked() else { return };
        let d = Draft { kind, page: s.page, anchor: s.anchor.clone(), rects: s.rects.clone(), pin: None };
        if kind == "highlight" || kind == "strike" {
            submit_draft(d, String::new(), None);
        } else {
            draft.set(Some(d));
        }
    };

    let can_comment = move || {
        loaded.with(|l| l.as_ref().and_then(|r| r.as_ref().ok()).is_some_and(|l| l.latest && l.page.me.as_ref().is_some_and(|m| m.role != "viewer")))
    };
    let can_resolve = move || {
        loaded.with(|l| {
            l.as_ref().and_then(|r| r.as_ref().ok()).is_some_and(|l| l.page.me.as_ref().is_some_and(|m| m.role != "viewer" && m.role != "guest"))
        })
    };

    view! {
        <div class="viewer">
            {move || match loaded.get() {
                None => view! { <p class="muted pad">"Loading…"</p> }.into_any(),
                Some(Err(Failure::Login(_))) => {
                    let t = token.get();
                    view! { <p class="pad">"Please " <a href=format!("/r/{t}")>"sign in"</a> " first."</p> }.into_any()
                }
                Some(Err(e)) => view! { <p class="error pad">{e.to_string()}</p> }.into_any(),
                Some(Ok(l)) => {
                    let l2 = l.clone();
                    let nav = navigate.clone();
                    let on_version = move |ev: leptos::ev::Event| {
                        let v = event_target_value(&ev);
                        nav(&format!("/r/{}/d/{}?v={v}", l2.token, l2.document.id), Default::default());
                    };
                    let header = view! {
                        <header class="bar">
                            <a class="back" href=format!("/r/{}", l.token)>"‹ " {l.page.project.name.clone()}</a>
                            <h1>{l.document.title.clone()}</h1>
                            <select on:change=on_version>
                                {l.document.versions.iter().rev().map(|v| view! { <option value=v.id.clone() selected={v.id == l.version.id}>{if v.label.is_empty() { format!("Version {}", v.n) } else { v.label.clone() }}</option> }).collect_view()}
                            </select>
                            <span class="zoom">
                                <button on:click=move |_| zoom.update(|z| *z = (*z - 0.1).max(0.3)) title="Zoom out">"−"</button>
                                <span>{move || format!("{}%", (zoom.get() * 100.0).round())}</span>
                                <button on:click=move |_| zoom.update(|z| *z = (*z + 0.1).min(3.0)) title="Zoom in">"+"</button>
                            </span>
                            {l.page.allow_download.then(|| view! { <a class="button" href=format!("{}/v/{}/document.pdf", api::api(&l.token), l.version.id) download="">"PDF"</a> })}
                        </header>
                        {(!l.latest).then(|| view! { <p class="banner">"This is an earlier version. Comments go on the latest one: " <a href=format!("/r/{}/d/{}", l.token, l.document.id)>"open it"</a>.</p> })}
                        {(l.page.me.is_none()).then(|| view! { <p class="banner">"You can read; to comment, " <a href=format!("/r/{}", l.token)>"join on the review page"</a>.</p> })}
                    };
                    let pages = pages_view(l.clone(), zoom, comments, sel, draft, selected, can_comment, start_draft);
                    view! {
                        {header}
                        <div class="layout">
                            <div class="pages" id="pages">{pages}</div>
                            <aside class="panel">
                                {move || notice.get().map(|n| view! { <p class="notice" on:click=move |_| notice.set(None)>{n}</p> })}
                                {move || draft.get().map(|d| {
                                    let d2 = d.clone();
                                    let submit = move |ev: leptos::ev::SubmitEvent| {
                                        ev.prevent_default();
                                        let r = (d2.kind == "replace").then(|| replacement.get());
                                        submit_draft(d2.clone(), body.get(), r);
                                    };
                                    view! {
                                        <form class="composer card" on:submit=submit>
                                            <h3>{match d.kind { "replace" => "Suggest a replacement", "pin" => "Pin a comment", _ => "Comment on the text" }}</h3>
                                            {d.anchor.as_ref().map(|a| view! { <blockquote>{a.exact.clone()}</blockquote> })}
                                            {(d.kind == "replace").then(|| view! { <label>"Replace with" <input required=true prop:value=move || replacement.get() on:input=move |ev| replacement.set(event_target_value(&ev)) /></label> })}
                                            <label>"Comment" <textarea rows=3 prop:value=move || body.get() on:input=move |ev| body.set(event_target_value(&ev))></textarea></label>
                                            <div class="row">
                                                <button type="submit">"Send"</button>
                                                <button type="button" class="quiet" on:click=move |_| { draft.set(None); body.set(String::new()); replacement.set(String::new()); }>"Cancel"</button>
                                            </div>
                                        </form>
                                    }
                                })}
                                <div class="filters">
                                    {["open", "resolved", "mine", "all"].map(|f| view! { <button class:on=move || filter.get() == f on:click=move |_| filter.set(f)>{f}</button> })}
                                </div>
                                <CommentList l=l.clone() comments=comments filter=filter selected=selected can_comment=can_comment can_resolve=can_resolve write=write />
                            </aside>
                        </div>
                    }.into_any()
                }
            }}
        </div>
    }
}

/// The pages: image, text layer, markers, the toolbar on a selection.
#[allow(clippy::too_many_arguments)]
fn pages_view(
    l: Arc<Loaded>,
    zoom: RwSignal<f64>,
    comments: RwSignal<Vec<Comment>>,
    sel: RwSignal<Option<Sel>>,
    draft: RwSignal<Option<Draft>>,
    selected: RwSignal<Option<String>>,
    can_comment: impl Fn() -> bool + Copy + Send + Sync + 'static,
    start_draft: impl Fn(&'static str) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let base = format!("{}/v/{}", api::api(&l.token), l.version.id);
    let version = l.version.id.clone();
    l.manifest
        .pages
        .iter()
        .map(|p| {
            let (w, h, index) = (p.width, p.height, p.index);
            let src = format!("{base}/{}", p.image);
            let lines = l.text.pages.iter().find(|t| t.index == index).map(|t| t.lines.clone()).unwrap_or_default();
            let version = version.clone();
            let pin_here = move |ev: leptos::ev::MouseEvent| {
                if !can_comment() {
                    return;
                }
                let on_text = ev.target().and_then(|t| wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(t).ok()).and_then(|e| e.closest(".tl, .pin, .toolbar").ok().flatten()).is_some();
                let selecting = web_sys::window().and_then(|w| w.get_selection().ok().flatten()).is_some_and(|s| !s.is_collapsed());
                if on_text || selecting {
                    return;
                }
                // Leptos delegates clicks to the document, so the page is found from the target.
                let Some(page_el) = ev.target().and_then(|t| wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(t).ok()).and_then(|e| e.closest(".page").ok().flatten()) else { return };
                let r = page_el.get_bounding_client_rect();
                let z = zoom.get_untracked();
                let (x, y) = ((f64::from(ev.client_x()) - r.left()) / z, (f64::from(ev.client_y()) - r.top()) / z);
                draft.set(Some(Draft { kind: "pin", page: index, anchor: None, rects: vec![], pin: Some((x, y)) }));
            };
            view! {
                <section class="page" id=format!("page-{index}") style:width=move || px(w * zoom.get()) style:height=move || px(h * zoom.get()) on:click=pin_here>
                    <img class="page-img" src=src alt=format!("Page {}", p.label) draggable="false" />
                    <div class="markers" aria-hidden="true">
                        {move || {
                            let z = zoom.get();
                            let sel_id = selected.get();
                            let mut n = 0;
                            comments.get().into_iter().filter(|c| c.version == version && c.page as usize == index && c.in_reply_to.is_none()).map(|c| {
                                let is_sel = sel_id.as_deref() == Some(c.id.as_str());
                                let id = c.id.clone();
                                if let (Some(x), Some(y)) = (c.x, c.y) {
                                    n += 1;
                                    view! { <button class="pin" class:selected=is_sel class:resolved=(c.status == "resolved") style:left=px(x * z) style:top=px(y * z) on:click=move |_| { selected.set(Some(id.clone())); dom::scroll_to(&format!("c-{}", id)); }>{n}</button> }.into_any()
                                } else {
                                    c.rects.clone().unwrap_or_default().into_iter().map(|r| view! {
                                        <div class=format!("marker {}", c.kind) class:selected=is_sel class:resolved=(c.status == "resolved") style:left=px(r[0] * z) style:top=px(r[1] * z) style:width=px((r[2] - r[0]) * z) style:height=px((r[3] - r[1]) * z)></div>
                                    }).collect_view().into_any()
                                }
                            }).collect_view()
                        }}
                        {move || draft.get().filter(|d| d.page == index).map(|d| {
                            let z = zoom.get();
                            d.rects.iter().map(|r| view! { <div class="marker draft" style:left=px(r[0] * z) style:top=px(r[1] * z) style:width=px((r[2] - r[0]) * z) style:height=px((r[3] - r[1]) * z)></div> }).collect_view()
                        })}
                    </div>
                    <div class="overlay">
                        {move || {
                            let z = zoom.get();
                            lines.iter().enumerate().map(|(i, line)| {
                                let r = line.rect;
                                let (lw, lh) = ((r[2] - r[0]) * z, (r[3] - r[1]) * z);
                                view! {
                                    <span class="tl" data-page=index data-line=i data-w=format!("{lw:.2}") dir=if line.rtl { "rtl" } else { "ltr" }
                                        style:left=px(r[0] * z) style:top=px(r[1] * z) style:height=px(lh) style:font-size=px(lh * 0.82) style:line-height=px(lh)>
                                        {line.text.clone()}
                                    </span>
                                }
                            }).collect_view()
                        }}
                    </div>
                    {move || sel.get().filter(|s| s.page == index && can_comment()).map(|s| view! {
                        <div class="toolbar" style:left=px(s.at.0) style:top=px(s.at.1 + 6.0)>
                            {match s.anchor {
                                Some(_) => view! {
                                    <button on:click=move |_| start_draft("highlight")>"Highlight"</button>
                                    <button on:click=move |_| start_draft("comment")>"Comment"</button>
                                    <button on:click=move |_| start_draft("strike")>"Strike"</button>
                                    <button on:click=move |_| start_draft("replace")>"Replace"</button>
                                }.into_any(),
                                None => view! { <span class="muted">"Select within one text"</span> }.into_any(),
                            }}
                        </div>
                    })}
                </section>
            }
        })
        .collect_view()
}

#[component]
fn CommentList(
    l: Arc<Loaded>,
    comments: RwSignal<Vec<Comment>>,
    filter: RwSignal<&'static str>,
    selected: RwSignal<Option<String>>,
    can_comment: impl Fn() -> bool + Copy + Send + Sync + 'static,
    can_resolve: impl Fn() -> bool + Copy + Send + Sync + 'static,
    write: impl Fn(&'static str, String, Value, String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let me = StoredValue::new(l.page.me.as_ref().map(|m| m.id.clone()).unwrap_or_default());
    let token = l.token.clone();
    let version = StoredValue::new(l.version.id.clone());
    let shown = move || {
        let (f, all) = (filter.get(), comments.get());
        let mut top: Vec<Comment> = all
            .iter()
            .filter(|c| version.with_value(|v| c.version == *v) && c.in_reply_to.is_none())
            .filter(|c| match f {
                "open" => c.status == "open",
                "resolved" => c.status != "open",
                "mine" => me.with_value(|m| c.author.id == *m),
                _ => true,
            })
            .cloned()
            .collect();
        top.sort_by_key(|c| (c.page, c.created));
        top
    };
    view! {
        <div class="comments">
            {move || shown().is_empty().then(|| view! { <p class="muted">"No comments here yet. Select text on a page, or click where a note belongs."</p> })}
            <For each=shown key=|c| (c.id.clone(), c.modified, c.status.clone()) children=move |c: Comment| {
                let id = c.id.clone();
                let reply = RwSignal::new(String::new());
                let replies = { let id = id.clone(); move || comments.get().into_iter().filter(|r| r.in_reply_to.as_deref() == Some(id.as_str())).collect::<Vec<_>>() };
                let (t1, t2) = (token.clone(), token.clone());
                let (id1, id2, id3, id4) = (id.clone(), id.clone(), id.clone(), id.clone());
                let send_reply = move |ev: leptos::ev::SubmitEvent| {
                    ev.prevent_default();
                    let text = reply.get();
                    if text.trim().is_empty() {
                        return;
                    }
                    write("POST", format!("{}/comments/{}/replies", api::api(&t1), id1), json!({"id": api::new_id(), "body": text}), String::new());
                    reply.set(String::new());
                };
                let status = c.status.clone();
                let toggle = move |_| {
                    let next = if status == "open" { "resolved" } else { "open" };
                    write("PATCH", format!("{}/comments/{}", api::api(&t2), id2), json!({"status": next}), String::new());
                };
                let show = move |_| {
                    selected.set(Some(id3.clone()));
                    dom::scroll_to(&format!("page-{}", c.page));
                };
                let settled = c.status != "open";
                view! {
                    <article class="comment" id=format!("c-{}", c.id) class:selected=move || selected.get().as_deref() == Some(id4.as_str()) class:settled=settled>
                        <header>
                            <b>{c.author.name.clone()}</b>
                            <span class=format!("kind {}", c.kind)>{c.kind.clone()}</span>
                            <span class="muted">"p. " {c.page + 1} " · " {dom::date(c.created)}</span>
                        </header>
                        {c.quote.exact.clone().filter(|q| !q.is_empty()).map(|q| view! { <blockquote>{q}</blockquote> })}
                        {(!c.body.is_empty()).then(|| view! { <p>{c.body.clone()}</p> })}
                        {c.replacement.clone().map(|r| view! { <p class="replacement">"→ " {r}</p> })}
                        <div class="replies">
                            {move || replies().into_iter().map(|r| view! { <p class="reply"><b>{r.author.name.clone()}</b> " " {r.body.clone()}</p> }).collect_view()}
                        </div>
                        {can_comment().then(|| view! {
                            <form class="row" on:submit=send_reply>
                                <input placeholder="Reply…" prop:value=move || reply.get() on:input=move |ev| reply.set(event_target_value(&ev)) />
                                <button type="submit" class="quiet">"Reply"</button>
                            </form>
                        })}
                        <footer class="row">
                            <span class=format!("status {}", c.status)>{c.status.clone()}</span>
                            <button class="quiet" on:click=show>"Show"</button>
                            {(can_resolve() && (c.status == "open" || c.status == "resolved")).then(|| view! { <button class="quiet" on:click=toggle>{if settled { "Reopen" } else { "Resolve" }}</button> })}
                        </footer>
                    </article>
                }
            } />
        </div>
    }
}
