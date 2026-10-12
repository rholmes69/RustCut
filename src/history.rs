//! Snapshot-based undo/redo.
//!
//! The app calls [`History::commit`] once per frame when no interaction is in
//! progress; any difference from the last committed snapshot becomes one undo step.

use crate::model::Project;

const LIMIT: usize = 200;

pub struct History {
    undo: Vec<Project>,
    redo: Vec<Project>,
    committed: Project,
}

impl History {
    pub fn new(project: &Project) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), committed: project.clone() }
    }

    pub fn reset(&mut self, project: &Project) {
        *self = Self::new(project);
    }

    pub fn commit(&mut self, current: &Project) {
        if *current != self.committed {
            let prev = std::mem::replace(&mut self.committed, current.clone());
            self.undo.push(prev);
            if self.undo.len() > LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    pub fn undo(&mut self, current: &mut Project) -> bool {
        self.commit(current);
        match self.undo.pop() {
            Some(prev) => {
                self.redo.push(std::mem::replace(&mut self.committed, prev.clone()));
                *current = prev;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, current: &mut Project) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(&mut self.committed, next.clone()));
                *current = next;
                true
            }
            None => false,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_roundtrip() {
        let mut p = Project::default();
        let mut h = History::new(&p);
        p.name = "A".into();
        h.commit(&p);
        p.name = "B".into();
        h.commit(&p);
        assert!(h.undo(&mut p));
        assert_eq!(p.name, "A");
        assert!(h.undo(&mut p));
        assert_eq!(p.name, "Untitled project");
        assert!(!h.undo(&mut p));
        assert!(h.redo(&mut p));
        assert_eq!(p.name, "A");
        // A new edit clears the redo stack.
        p.name = "C".into();
        h.commit(&p);
        assert!(!h.can_redo());
    }

    #[test]
    fn undo_includes_uncommitted_change() {
        let mut p = Project::default();
        let mut h = History::new(&p);
        p.fps = 60;
        assert!(h.undo(&mut p));
        assert_eq!(p.fps, 30);
    }
}
