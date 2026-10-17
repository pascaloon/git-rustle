//! Commit selection is independent of the focused details row.
#[derive(Default)]
pub struct Commits {
    explicit: Option<Vec<gix::ObjectId>>,
}
impl Commits {
    pub fn ids(&self, focused: Option<gix::ObjectId>) -> Vec<gix::ObjectId> {
        self.explicit
            .clone()
            .unwrap_or_else(|| focused.into_iter().collect())
    }
    pub fn contains(&self, id: gix::ObjectId, focused: Option<gix::ObjectId>) -> bool {
        self.explicit
            .as_ref()
            .map_or(focused == Some(id), |ids| ids.contains(&id))
    }
    pub fn clear(&mut self) {
        self.explicit = None;
    }
    pub fn click(
        &mut self,
        id: Option<gix::ObjectId>,
        focused: Option<gix::ObjectId>,
        control: bool,
    ) {
        if !control || id.is_none() {
            self.explicit = id.map(|id| vec![id]);
            return;
        }
        let ids = self
            .explicit
            .get_or_insert_with(|| focused.into_iter().collect());
        let id = id.unwrap();
        if let Some(i) = ids.iter().position(|x| *x == id) {
            ids.remove(i);
        } else {
            ids.push(id);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::Commits;
    fn id(n: u8) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(&[n; 20])
    }
    #[test]
    fn control_click_toggles_without_replacing_previous_selection() {
        let mut s = Commits::default();
        s.click(Some(id(2)), Some(id(1)), true);
        assert_eq!(s.ids(Some(id(2))), vec![id(1), id(2)]);
        s.click(Some(id(3)), Some(id(2)), true);
        assert_eq!(s.ids(None).len(), 3);
        s.click(Some(id(2)), Some(id(3)), true);
        assert_eq!(s.ids(None), vec![id(1), id(3)]);
        s.click(Some(id(4)), None, false);
        assert_eq!(s.ids(None), vec![id(4)]);
        s.click(Some(id(4)), None, true);
        assert!(s.ids(Some(id(4))).is_empty());
    }
}
