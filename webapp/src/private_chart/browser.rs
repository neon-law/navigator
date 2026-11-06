//! Browser transport and instance-owned lifecycle; no browser storage or server functions.

use dioxus::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use super::state::Ticket;
use super::PageState;

#[derive(Clone, Default)]
pub(super) struct Runtime {
    request: Rc<RefCell<Option<PendingRequest>>>,
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    lifecycle: Rc<RefCell<Option<web::Lifecycle>>>,
}

#[derive(Default)]
struct PendingRequest {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    controller: Option<web_sys::AbortController>,
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    task: Option<dioxus::core::Task>,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            if let Some(controller) = self.controller.take() {
                controller.abort();
            }
            if let Some(task) = self.task.take() {
                task.cancel();
            }
        }
    }
}

impl Runtime {
    pub fn cancel(&self) {
        self.request.borrow_mut().take();
    }

    pub fn dispose(&self) {
        self.cancel();
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        self.lifecycle.borrow_mut().take();
    }

    pub fn mounted(&self, event: &MountedEvent, state: Signal<PageState>, ready: Signal<bool>) {
        self.cancel();
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            if self.lifecycle.borrow().is_none() {
                if let Some(lifecycle) =
                    web::Lifecycle::install(event, state, ready, self.request.clone())
                {
                    *self.lifecycle.borrow_mut() = Some(lifecycle);
                }
            }
        }
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        let _ = (event, state, ready);
    }

    pub fn submit(
        &self,
        state: Signal<PageState>,
        ticket: Ticket,
        payload: serde_json::Value,
        csrf: String,
        project: uuid::Uuid,
    ) {
        self.cancel();
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        web::submit(&self.request, state, ticket, payload, csrf, project);
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let mut state = state;
            let _ = project;
            drop(payload);
            drop(csrf);
            state
                .write()
                .complete(&ticket, Err("Private calculation requires the browser."));
            drop(ticket);
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web {
    use super::{PageState, PendingRequest, Ticket};
    use crate::astrology_chart::calculated::CalculatedChart;
    use dioxus::prelude::{spawn, MountedEvent, Signal, WritableExt};
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::{closure::Closure, JsCast};
    use web_sys::{
        Event, HtmlElement, HtmlInputElement, HtmlSelectElement, PageTransitionEvent, Window,
    };

    pub(super) struct Lifecycle {
        window: Window,
        hide: Closure<dyn FnMut(Event)>,
        show: Closure<dyn FnMut(Event)>,
    }

    impl Lifecycle {
        pub fn install(
            event: &MountedEvent,
            mut state: Signal<PageState>,
            mut ready: Signal<bool>,
            request: Rc<RefCell<Option<PendingRequest>>>,
        ) -> Option<Self> {
            let root = event
                .data()
                .downcast::<web_sys::Element>()?
                .clone()
                .dyn_into::<HtmlElement>()
                .ok()?;
            let window = web_sys::window()?;
            let hide_root = root.clone();
            let hide = Closure::wrap(Box::new(move |_event: Event| {
                // A reactive render can be frozen by BFCache. Hide synchronously first.
                hide_root.set_hidden(true);
                if let Ok(controls) = hide_root.query_selector_all("[data-private-input]") {
                    for index in 0..controls.length() {
                        if let Some(control) = controls.item(index) {
                            if let Some(input) = control.dyn_ref::<HtmlInputElement>() {
                                input.set_value("");
                                input.set_checked(false);
                            } else if let Some(select) = control.dyn_ref::<HtmlSelectElement>() {
                                select.set_selected_index(0);
                            }
                        }
                    }
                }
                request.borrow_mut().take();
                if let Ok(mut current) = state.try_write() {
                    current.clear();
                }
                ready.set(false);
            }) as Box<dyn FnMut(Event)>);
            let restore_window = window.clone();
            let show = Closure::wrap(Box::new(move |event: Event| {
                if event
                    .dyn_ref::<PageTransitionEvent>()
                    .is_some_and(PageTransitionEvent::persisted)
                {
                    // Keep stale DOM hidden until a fresh authorized, blank SSR document arrives.
                    root.set_hidden(true);
                    let _ = restore_window.location().reload();
                }
            }) as Box<dyn FnMut(Event)>);
            window
                .add_event_listener_with_callback("pagehide", hide.as_ref().unchecked_ref())
                .ok()?;
            if window
                .add_event_listener_with_callback("pageshow", show.as_ref().unchecked_ref())
                .is_err()
            {
                let _ = window
                    .remove_event_listener_with_callback("pagehide", hide.as_ref().unchecked_ref());
                return None;
            }
            ready.set(true);
            Some(Self { window, hide, show })
        }
    }

    impl Drop for Lifecycle {
        fn drop(&mut self) {
            let _ = self.window.remove_event_listener_with_callback(
                "pagehide",
                self.hide.as_ref().unchecked_ref(),
            );
            let _ = self.window.remove_event_listener_with_callback(
                "pageshow",
                self.show.as_ref().unchecked_ref(),
            );
        }
    }

    pub(super) fn submit(
        request_slot: &Rc<RefCell<Option<PendingRequest>>>,
        mut state: Signal<PageState>,
        ticket: Ticket,
        payload: serde_json::Value,
        csrf: String,
        project: uuid::Uuid,
    ) {
        let Ok(controller) = web_sys::AbortController::new() else {
            state.write().complete(
                &ticket,
                Err("Private calculation is unavailable in this browser."),
            );
            return;
        };
        let signal = controller.signal();
        let request =
            gloo_net::http::Request::post(&format!("/app/api/projects/{project}/astrology-chart"))
                .header("x-csrf-token", &csrf)
                .credentials(web_sys::RequestCredentials::SameOrigin)
                .mode(web_sys::RequestMode::SameOrigin)
                .cache(web_sys::RequestCache::NoStore)
                .redirect(web_sys::RequestRedirect::Error)
                .referrer_policy(web_sys::ReferrerPolicy::NoReferrer)
                .abort_signal(Some(&signal))
                .json(&payload);
        drop(payload);
        drop(csrf);
        let Ok(request) = request else {
            state
                .write()
                .complete(&ticket, Err("Calculation input could not be prepared."));
            return;
        };
        let completion_slot = Rc::clone(request_slot);
        let task = spawn(async move {
            let result = match request.send().await {
                Ok(response) if response.ok() => response
                    .json::<CalculatedChart>()
                    .await
                    .map_err(|_| "The calculation response is unavailable."),
                Ok(response) => Err(match response.status() {
                    401 | 403 => {
                        "Calculation is not permitted in this view. Sign in again if needed."
                    }
                    404 => "This Project is unavailable.",
                    413 => "Calculation input is too large.",
                    503 => "Verified calculation sources are unavailable.",
                    400 | 422 => {
                        "Check the birth date, clock, time zone, explicit offset and location."
                    }
                    _ => "The calculation could not be completed.",
                }),
                Err(_) => Err("The calculation could not be completed."),
            };
            // Check after parsing too: clear/navigation may have happened during either await.
            let accepted = state
                .try_write()
                .is_ok_and(|mut current| current.complete(&ticket, result));
            if accepted {
                if let Some(mut finished) = completion_slot.borrow_mut().take() {
                    finished.controller = None;
                    finished.task = None;
                }
            }
        });
        *request_slot.borrow_mut() = Some(PendingRequest {
            controller: Some(controller),
            task: Some(task),
        });
    }
}
