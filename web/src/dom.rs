//! The DOM calls the viewer needs: reading a selection, fitting the invisible text to the page
//! image, event listeners, the event stream.

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;
use web_sys::{Element, Event, EventSource, MessageEvent, Node};

pub fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

/// A listener that is removed when dropped.
pub struct Listener {
    target: web_sys::EventTarget,
    name: String,
    closure: Closure<dyn FnMut(Event)>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback(&self.name, self.closure.as_ref().unchecked_ref());
    }
}

fn listen(target: Option<web_sys::EventTarget>, name: &str, f: impl FnMut(Event) + 'static) -> Option<Listener> {
    let target = target?;
    let closure = Closure::<dyn FnMut(Event)>::new(f);
    target.add_event_listener_with_callback(name, closure.as_ref().unchecked_ref()).ok()?;
    Some(Listener { target, name: name.into(), closure })
}

pub fn on_document(name: &str, f: impl FnMut(Event) + 'static) -> Option<Listener> {
    listen(document().map(Into::into), name, f)
}

pub fn on_window(name: &str, f: impl FnMut(Event) + 'static) -> Option<Listener> {
    listen(web_sys::window().map(Into::into), name, f)
}

/// A selection inside one page's text overlay: from character `a_char` of line `a_line` to
/// character `b_char` of line `b_line`, with its rectangles in page points and a spot under it
/// (page pixels) for the toolbar.
#[derive(Clone, Debug, PartialEq)]
pub struct RawSel {
    pub page: usize,
    pub a_line: usize,
    pub a_char: usize,
    pub b_line: usize,
    pub b_char: usize,
    pub rects: Vec<[f64; 4]>,
    pub below: (f64, f64),
}

fn line_of(node: &Node) -> Option<(usize, usize, String, Element)> {
    let el = node.dyn_ref::<Element>().cloned().or_else(|| node.parent_element())?;
    let span = el.closest(".tl").ok().flatten()?;
    let page = span.get_attribute("data-page")?.parse().ok()?;
    let line = span.get_attribute("data-line")?.parse().ok()?;
    Some((page, line, span.text_content().unwrap_or_default(), span))
}

pub fn read_selection(zoom: f64) -> Option<RawSel> {
    let sel = web_sys::window()?.get_selection().ok().flatten()?;
    if sel.is_collapsed() || sel.range_count() == 0 {
        return None;
    }
    let range = sel.get_range_at(0).ok()?;
    let (pa, la, ta, span_a) = line_of(&range.start_container().ok()?)?;
    let (pb, lb, tb, _) = line_of(&range.end_container().ok()?)?;
    if pa != pb || zoom <= 0.0 {
        return None;
    }
    let a = crate::anchor::char_at_utf16(&ta, range.start_offset().ok()? as usize);
    let b = crate::anchor::char_at_utf16(&tb, range.end_offset().ok()? as usize);
    let page_el = span_a.closest(".page").ok().flatten()?;
    let pr = page_el.get_bounding_client_rect();
    let list = range.get_client_rects()?;
    let mut rects = Vec::new();
    for i in 0..list.length() {
        if let Some(r) = list.get(i)
            && r.width() > 0.0
        {
            rects.push([(r.left() - pr.left()) / zoom, (r.top() - pr.top()) / zoom, (r.right() - pr.left()) / zoom, (r.bottom() - pr.top()) / zoom]);
        }
    }
    let br = range.get_bounding_client_rect();
    Some(RawSel { page: pa, a_line: la, a_char: a, b_line: lb, b_char: b, rects, below: (br.left() - pr.left(), br.bottom() - pr.top()) })
}

pub fn clear_selection() {
    if let Some(s) = web_sys::window().and_then(|w| w.get_selection().ok().flatten()) {
        let _ = s.remove_all_ranges();
    }
}

/// Stretch each line's invisible text to the width its glyphs have on the page image, so a
/// selection lands where the eye says it does.
pub fn fit_overlays() {
    let Some(list) = document().and_then(|d| d.query_selector_all(".tl").ok()) else { return };
    for i in 0..list.length() {
        let Some(el) = list.get(i).and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) else { continue };
        let want: f64 = el.get_attribute("data-w").and_then(|w| w.parse().ok()).unwrap_or(0.0);
        let _ = el.style().set_property("transform", "none");
        let have = el.get_bounding_client_rect().width();
        if want > 0.0 && have > 0.0 {
            let _ = el.style().set_property("transform", &format!("scaleX({:.4})", want / have));
        }
    }
}

/// Call `f` on the next animation frame (after the DOM has caught up).
pub fn next_frame(f: impl FnOnce() + 'static) {
    let c = Closure::once_into_js(f);
    if let Some(w) = web_sys::window() {
        let _ = w.request_animation_frame(c.unchecked_ref());
    }
}

/// Listen to named server-sent events; the source closes when dropped.
pub struct Events(EventSource);

impl Drop for Events {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub fn events(url: &str, kinds: &[&str], f: impl Fn() + 'static) -> Option<Events> {
    let es = EventSource::new(url).ok()?;
    let f = std::rc::Rc::new(f);
    for kind in kinds {
        let f = f.clone();
        let c = Closure::<dyn FnMut(MessageEvent)>::new(move |_| f());
        let _ = es.add_event_listener_with_callback(kind, c.as_ref().unchecked_ref());
        c.forget();
    }
    Some(Events(es))
}

pub fn scroll_to(id: &str) {
    if let Some(el) = document().and_then(|d| d.get_element_by_id(id)) {
        el.scroll_into_view_with_bool(true);
    }
}

pub fn inner_width() -> f64 {
    web_sys::window().and_then(|w| w.inner_width().ok()).and_then(|v| v.as_f64()).unwrap_or(1024.0)
}

/// `created` seconds as the browser shows dates.
pub fn date(secs: i64) -> String {
    js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(secs as f64 * 1000.0)).to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED).into()
}
