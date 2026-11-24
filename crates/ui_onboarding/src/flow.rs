//! The wizard's step state machine. Pure (no gpui) so it is unit tested.
//!
//! * `advance` moves forward and marks the step as answered (un-skipping it if the user comes
//!   back to it).
//! * `skip` ("Omitir por ahora") moves forward leaving the step's choices out of the result;
//!   on the welcome step it abandons the whole wizard.
//! * `back` and `goto` only revisit steps already reached.

use std::collections::BTreeSet;

/// The six steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Step {
    Welcome,
    Agents,
    Projects,
    Appearance,
    Terminal,
    Done,
}

impl Step {
    pub const ALL: [Step; 6] = [
        Step::Welcome,
        Step::Agents,
        Step::Projects,
        Step::Appearance,
        Step::Terminal,
        Step::Done,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(ix: usize) -> Option<Step> {
        Self::ALL.get(ix).copied()
    }

    /// 1-based number for "demo --step N" style arguments.
    pub fn from_number(n: usize) -> Option<Step> {
        n.checked_sub(1).and_then(Self::from_index)
    }

    /// Short name for the breadcrumb.
    pub const fn label(self) -> &'static str {
        match self {
            Step::Welcome => "Bienvenida",
            Step::Agents => "Agentes",
            Step::Projects => "Proyectos",
            Step::Appearance => "Apariencia",
            Step::Terminal => "Terminal",
            Step::Done => "Listo",
        }
    }

    pub fn next(self) -> Option<Step> {
        Self::from_index(self.index() + 1)
    }

    pub fn prev(self) -> Option<Step> {
        self.index().checked_sub(1).and_then(Self::from_index)
    }
}

/// What a navigation request did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    /// Now showing this step.
    Moved(Step),
    /// "Abrir Selachi" on the last step.
    Finish,
    /// "Omitir por ahora" on the welcome step: the wizard is abandoned.
    SkipAll,
    /// Nothing changed (back on the first step, skip on the last, ...).
    Stay,
}

/// Current step plus the bookkeeping the breadcrumb and the result need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flow {
    step: Step,
    furthest: Step,
    skipped: BTreeSet<Step>,
}

impl Flow {
    pub fn new(start: Step) -> Self {
        Self {
            step: start,
            furthest: start,
            skipped: BTreeSet::new(),
        }
    }

    pub fn step(&self) -> Step {
        self.step
    }

    pub fn furthest(&self) -> Step {
        self.furthest
    }

    /// Steps whose choices must not reach the config.
    pub fn skipped(&self) -> &BTreeSet<Step> {
        &self.skipped
    }

    pub fn is_skipped(&self, step: Step) -> bool {
        self.skipped.contains(&step)
    }

    /// Whether the breadcrumb shows a check for `step` (reached, passed and answered).
    pub fn is_done(&self, step: Step) -> bool {
        step < self.step && !self.skipped.contains(&step)
    }

    /// Only the steps between Agents and Terminal and the welcome screen can be skipped.
    pub fn can_skip(&self) -> bool {
        self.step != Step::Done
    }

    pub fn can_go_back(&self) -> bool {
        self.step != Step::Welcome
    }

    fn move_to(&mut self, step: Step) -> Nav {
        self.step = step;
        self.furthest = self.furthest.max(step);
        Nav::Moved(step)
    }

    /// "Continuar" / "Empezar" / "Abrir Selachi".
    pub fn advance(&mut self) -> Nav {
        self.skipped.remove(&self.step);
        match self.step.next() {
            Some(next) => self.move_to(next),
            None => Nav::Finish,
        }
    }

    /// "Omitir por ahora".
    pub fn skip(&mut self) -> Nav {
        match self.step {
            Step::Welcome => Nav::SkipAll,
            Step::Done => Nav::Stay,
            current => {
                self.skipped.insert(current);
                match current.next() {
                    Some(next) => self.move_to(next),
                    None => Nav::Stay,
                }
            }
        }
    }

    /// "Atrás".
    pub fn back(&mut self) -> Nav {
        match self.step.prev() {
            Some(prev) => self.move_to(prev),
            None => Nav::Stay,
        }
    }

    /// Jump to a step already reached (breadcrumb click).
    pub fn goto(&mut self, step: Step) -> Nav {
        if step == self.step || step > self.furthest {
            Nav::Stay
        } else {
            self.move_to(step)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_forward_to_the_end() {
        let mut f = Flow::new(Step::Welcome);
        for expected in [
            Step::Agents,
            Step::Projects,
            Step::Appearance,
            Step::Terminal,
            Step::Done,
        ] {
            assert_eq!(f.advance(), Nav::Moved(expected));
        }
        assert_eq!(f.advance(), Nav::Finish);
        assert_eq!(f.step(), Step::Done, "finishing does not move");
    }

    #[test]
    fn back_stops_at_the_welcome_step() {
        let mut f = Flow::new(Step::Projects);
        assert_eq!(f.back(), Nav::Moved(Step::Agents));
        assert_eq!(f.back(), Nav::Moved(Step::Welcome));
        assert_eq!(f.back(), Nav::Stay);
        assert!(!f.can_go_back());
    }

    #[test]
    fn skip_marks_the_step_and_moves_on() {
        let mut f = Flow::new(Step::Agents);
        assert_eq!(f.skip(), Nav::Moved(Step::Projects));
        assert!(f.is_skipped(Step::Agents));
        assert!(!f.is_done(Step::Agents), "a skipped step gets no check");
        assert_eq!(f.advance(), Nav::Moved(Step::Appearance));
        assert!(f.is_done(Step::Projects));
    }

    #[test]
    fn revisiting_and_continuing_unskips() {
        let mut f = Flow::new(Step::Agents);
        f.skip();
        assert_eq!(f.back(), Nav::Moved(Step::Agents));
        assert!(
            f.is_skipped(Step::Agents),
            "going back alone keeps the skip"
        );
        f.advance();
        assert!(!f.is_skipped(Step::Agents));
    }

    #[test]
    fn welcome_skip_abandons_and_done_cannot_be_skipped() {
        assert_eq!(Flow::new(Step::Welcome).skip(), Nav::SkipAll);
        let mut f = Flow::new(Step::Done);
        assert!(!f.can_skip());
        assert_eq!(f.skip(), Nav::Stay);
        assert!(f.skipped().is_empty());
    }

    #[test]
    fn goto_only_reaches_visited_steps() {
        let mut f = Flow::new(Step::Welcome);
        f.advance();
        f.advance(); // at Projects, furthest Projects
        assert_eq!(f.goto(Step::Terminal), Nav::Stay);
        assert_eq!(f.goto(Step::Agents), Nav::Moved(Step::Agents));
        assert_eq!(f.furthest(), Step::Projects);
        assert_eq!(f.goto(Step::Projects), Nav::Moved(Step::Projects));
    }

    #[test]
    fn step_numbers_are_one_based() {
        assert_eq!(Step::from_number(1), Some(Step::Welcome));
        assert_eq!(Step::from_number(6), Some(Step::Done));
        assert_eq!(Step::from_number(0), None);
        assert_eq!(Step::from_number(7), None);
    }
}
