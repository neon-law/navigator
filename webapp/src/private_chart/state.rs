//! Page-owned calculation lifetime. Tickets carry no birth input or chart values.

use std::rc::{Rc, Weak};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Idle,
    Calculating,
    Complete,
    Refused(&'static str),
}

#[derive(Clone)]
pub(super) struct Ticket {
    project_id: Uuid,
    generation: Weak<()>,
}

pub(super) struct ChartState<D, T> {
    pub draft: D,
    pub chart: Option<T>,
    pub status: Status,
    project_id: Uuid,
    generation: Rc<()>,
}

impl<D: Default, T> ChartState<D, T> {
    pub fn new(project_id: Uuid) -> Self {
        Self {
            draft: D::default(),
            chart: None,
            status: Status::Idle,
            project_id,
            generation: Rc::new(()),
        }
    }

    pub fn begin(&mut self) -> Ticket {
        self.generation = Rc::new(());
        self.chart = None;
        self.status = Status::Calculating;
        Ticket {
            project_id: self.project_id,
            generation: Rc::downgrade(&self.generation),
        }
    }

    pub fn clear(&mut self) {
        self.generation = Rc::new(());
        self.draft = D::default();
        self.chart = None;
        self.status = Status::Idle;
    }

    pub fn edit(&mut self, update: impl FnOnce(&mut D)) {
        self.generation = Rc::new(());
        self.chart = None;
        self.status = Status::Idle;
        update(&mut self.draft);
    }

    pub fn complete(&mut self, ticket: &Ticket, result: Result<T, &'static str>) -> bool {
        if self.status != Status::Calculating
            || ticket.project_id != self.project_id
            || !ticket
                .generation
                .upgrade()
                .is_some_and(|generation| Rc::ptr_eq(&generation, &self.generation))
        {
            return false;
        }
        match result {
            Ok(chart) => {
                self.chart = Some(chart);
                self.status = Status::Complete;
            }
            Err(message) => {
                self.chart = None;
                self.status = Status::Refused(message);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_during_inflight_calculation_rejects_a_late_parsed_result() {
        let mut state = ChartState::<String, Rc<String>>::new(Uuid::now_v7());
        state.draft = "synthetic private input".into();
        let ticket = state.begin();
        state.clear();
        let result = Rc::new("synthetic calculated result".into());
        let weak = Rc::downgrade(&result);
        assert!(
            !state.complete(&ticket, Ok(result)),
            "clearing invalidates the in-flight request after parsing too"
        );
        assert!(
            weak.upgrade().is_none(),
            "a refused late result is released"
        );
        assert!(state.draft.is_empty() && state.chart.is_none());
    }

    #[test]
    fn clear_releases_completed_chart_and_original_draft() {
        let mut state = ChartState::<String, Rc<String>>::new(Uuid::now_v7());
        state.draft = "synthetic private input".into();
        let ticket = state.begin();
        let result = Rc::new("synthetic calculated result".into());
        let weak = Rc::downgrade(&result);
        assert!(
            state.complete(&ticket, Ok(result)),
            "the current request may complete"
        );
        state.clear();
        assert!(state.draft.is_empty() && state.chart.is_none());
        assert!(
            weak.upgrade().is_none(),
            "clear releases the page-owned chart"
        );
        assert!(
            ticket.generation.upgrade().is_none(),
            "clear releases its request generation"
        );
    }

    #[test]
    fn project_switch_and_remount_cannot_receive_an_earlier_result() {
        let project = Uuid::now_v7();
        let mut original = ChartState::<String, String>::new(project);
        let ticket = original.begin();
        let mut other = ChartState::<String, String>::new(Uuid::now_v7());
        other.begin();
        assert!(
            !other.complete(&ticket, Ok("late".into())),
            "another Project rejects the response"
        );
        let mut remounted = ChartState::<String, String>::new(project);
        remounted.begin();
        assert!(
            !remounted.complete(&ticket, Ok("late".into())),
            "a fresh page on the same Project rejects the old response"
        );
    }
}
