//! The window onto the catalog: what's filtered, how it's sorted, the pages of it loaded so
//! far, and what's selected. A library can hold hundreds of thousands of rows, so nothing here
//! assumes more than a page or two is in memory.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use unlight_catalog::{Filter, Flag, Label, LibraryRow, Page, RAW_EXTENSIONS, Sort, VariantId};

pub const PAGE_SIZE: u64 = 256;
/// Loaded pages kept around, which comfortably covers a screen of the smallest thumbnails.
const MAX_PAGES: usize = 64;

pub fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| RAW_EXTENSIONS.contains(&extension.to_lowercase().as_str()))
}

pub fn label_name(label: Label) -> &'static str {
    match label {
        Label::None => "None",
        Label::Red => "Red",
        Label::Yellow => "Yellow",
        Label::Green => "Green",
        Label::Blue => "Blue",
        Label::Purple => "Purple",
    }
}

pub fn file_name(row: &LibraryRow) -> String {
    row.path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn title(row: &LibraryRow) -> String {
    match &row.copy_name {
        Some(copy) => format!("{} / {copy}", file_name(row)),
        None => file_name(row),
    }
}

/// How a click on a row changes the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClickKind {
    #[default]
    Replace,
    Toggle,
    Extend,
}

/// A page the store should fetch.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRequest {
    pub generation: u64,
    pub index: u64,
    pub filter: Filter,
    pub sort: Sort,
}

impl PageRequest {
    pub fn page(&self) -> Page {
        Page {
            offset: self.index * PAGE_SIZE,
            limit: PAGE_SIZE,
        }
    }
}

#[derive(Default)]
pub struct Library {
    filter: Filter,
    sort: Sort,
    /// Bumped whenever the rows might have changed, so late replies can be ignored.
    generation: u64,
    count: Option<u64>,
    pages: HashMap<u64, Vec<LibraryRow>>,
    page_use: HashMap<u64, u64>,
    use_clock: u64,
    requested: HashSet<u64>,
    wanted: Vec<u64>,
    count_wanted: bool,
    focus: Option<usize>,
    focus_id: Option<VariantId>,
    anchor: Option<usize>,
    selected: HashSet<VariantId>,
    /// A path to select as soon as its row turns up.
    pending_path: Option<PathBuf>,
}

impl Library {
    pub fn new() -> Self {
        Self {
            count_wanted: true,
            ..Self::default()
        }
    }

    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    pub fn sort(&self) -> Sort {
        self.sort
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn count(&self) -> Option<u64> {
        self.count
    }

    pub fn len(&self) -> usize {
        self.count.unwrap_or(0) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn set_filter(&mut self, filter: Filter) {
        if filter != self.filter {
            self.filter = filter;
            self.reload(true);
        }
    }

    pub fn set_sort(&mut self, sort: Sort) {
        if sort != self.sort {
            self.sort = sort;
            self.reload(true);
        }
    }

    /// Fetches everything again, keeping what's on screen until the new rows arrive.
    pub fn refresh(&mut self) {
        self.reload(false);
    }

    fn reload(&mut self, drop_rows: bool) {
        self.generation += 1;
        self.count_wanted = true;
        self.requested.clear();
        self.wanted = self.pages.keys().copied().collect();
        self.wanted.sort_unstable();
        if drop_rows {
            self.pages.clear();
            self.wanted.clear();
            self.count = None;
            self.focus = None;
        }
        self.need_around_focus();
    }

    /// Whether the row count should be fetched, once.
    pub fn take_count_request(&mut self) -> bool {
        std::mem::take(&mut self.count_wanted)
    }

    /// Says a row is about to be shown.
    pub fn need(&mut self, index: usize) {
        let page = index as u64 / PAGE_SIZE;
        self.use_clock += 1;
        self.page_use.insert(page, self.use_clock);
        if !self.requested.contains(&page) && !self.pages.contains_key(&page) && !self.wanted.contains(&page)
        {
            self.wanted.push(page);
        }
    }

    fn need_around_focus(&mut self) {
        let focus = self.focus.unwrap_or(0);
        self.need(focus);
        self.need((focus + PAGE_SIZE as usize / 2).min(self.len().saturating_sub(1)));
        self.need(focus.saturating_sub(PAGE_SIZE as usize / 2));
    }

    /// Pages to fetch now, nearest-to-needed last so they pop off first.
    pub fn take_page_requests(&mut self) -> Vec<PageRequest> {
        let wanted = std::mem::take(&mut self.wanted);
        self.requested.extend(wanted.iter().copied());
        wanted
            .into_iter()
            .map(|index| PageRequest {
                generation: self.generation,
                index,
                filter: self.filter.clone(),
                sort: self.sort,
            })
            .collect()
    }

    pub fn apply_count(&mut self, generation: u64, count: u64) {
        if generation != self.generation {
            return;
        }
        self.count = Some(count);
        if count == 0 {
            self.focus = None;
        } else if let Some(focus) = self.focus {
            self.focus = Some(focus.min(count as usize - 1));
        }
        self.settle_focus();
    }

    pub fn apply_page(&mut self, generation: u64, index: u64, rows: Vec<LibraryRow>) {
        if generation != self.generation {
            return;
        }
        self.requested.remove(&index);
        self.pages.insert(index, rows);
        self.evict_pages();
        self.settle_focus();
        if self.pending_path.is_some() {
            self.look_further();
        }
    }

    /// The row of a pending path may be anywhere, so keep reading until it turns up.
    fn look_further(&mut self) {
        let last = self.len().saturating_sub(1) as u64 / PAGE_SIZE;
        match (0..=last).find(|page| !self.pages.contains_key(page) && !self.requested.contains(page)) {
            Some(page) => self.need((page * PAGE_SIZE) as usize),
            None if self.requested.is_empty() && self.wanted.is_empty() => self.pending_path = None,
            None => {}
        }
    }

    fn evict_pages(&mut self) {
        while self.pages.len() > MAX_PAGES {
            let focus_page = self.focus.unwrap_or(0) as u64 / PAGE_SIZE;
            let oldest = self
                .pages
                .keys()
                .filter(|page| **page != focus_page)
                .min_by_key(|page| self.page_use.get(page).copied().unwrap_or(0))
                .copied();
            let Some(page) = oldest else {
                return;
            };
            self.pages.remove(&page);
            self.page_use.remove(&page);
        }
    }

    /// Keeps the focus on the same variant as rows move around it, and picks the first row
    /// when there's no focus at all yet.
    fn settle_focus(&mut self) {
        if let Some(path) = &self.pending_path
            && let Some(index) = self.index_where(|row| row.path == *path)
        {
            self.pending_path = None;
            self.focus_on(index);
            return;
        }
        if let Some(id) = self.focus_id
            && self
                .row(self.focus.unwrap_or(usize::MAX))
                .is_none_or(|row| row.variant != id)
            && let Some(index) = self.index_where(|row| row.variant == id)
        {
            self.focus = Some(index);
            return;
        }
        if self.focus.is_none() && self.count.is_some_and(|count| count > 0) && self.row(0).is_some() {
            self.focus_on(0);
        }
    }

    fn index_where(&self, matches: impl Fn(&LibraryRow) -> bool) -> Option<usize> {
        self.pages.iter().find_map(|(page, rows)| {
            let position = rows.iter().position(&matches)?;
            Some(*page as usize * PAGE_SIZE as usize + position)
        })
    }

    pub fn row(&self, index: usize) -> Option<&LibraryRow> {
        let page = self.pages.get(&(index as u64 / PAGE_SIZE))?;
        page.get(index % PAGE_SIZE as usize)
    }

    pub fn focus(&self) -> Option<usize> {
        self.focus
    }

    pub fn focused(&self) -> Option<&LibraryRow> {
        self.row(self.focus?)
    }

    pub fn is_selected(&self, id: VariantId) -> bool {
        self.selected.contains(&id)
    }

    /// The selected variants, or the focused one when nothing else is.
    pub fn selection(&self) -> Vec<VariantId> {
        let mut ids: Vec<VariantId> = self.selected.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn selection_len(&self) -> usize {
        self.selected.len()
    }

    /// Selected rows that are loaded, the focused one first, for the views that compare them.
    pub fn selected_rows(&self) -> Vec<LibraryRow> {
        let mut rows: Vec<LibraryRow> = self
            .pages
            .values()
            .flatten()
            .filter(|row| self.selected.contains(&row.variant))
            .cloned()
            .collect();
        rows.sort_by_key(|row| (Some(row.variant) != self.focus_id, row.capture_time, row.variant));
        rows
    }

    fn focus_on(&mut self, index: usize) {
        self.focus = Some(index);
        self.anchor = Some(index);
        self.focus_id = self.row(index).map(|row| row.variant);
        self.selected.clear();
        self.selected.extend(self.focus_id);
        self.need_around_focus();
    }

    pub fn select(&mut self, index: usize) {
        self.focus_on(index);
    }

    /// Selects a file once it's loaded, as when a path was opened from outside.
    pub fn select_path(&mut self, path: PathBuf) {
        self.pending_path = Some(path);
        self.settle_focus();
    }

    /// Selects a variant by id, when its row is already loaded. Otherwise it's kept as the
    /// focus to land on when its page arrives.
    pub fn select_variant(&mut self, id: VariantId) {
        self.focus_id = Some(id);
        self.selected.clear();
        self.selected.insert(id);
        self.settle_focus();
    }

    /// A click on a row, with the modifier keys it came with.
    pub fn click(&mut self, index: usize, kind: ClickKind) {
        let Some(id) = self.row(index).map(|row| row.variant) else {
            return;
        };
        match kind {
            ClickKind::Replace => self.focus_on(index),
            ClickKind::Toggle => {
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                    self.focus = Some(index);
                    self.focus_id = Some(id);
                }
                self.anchor = Some(index);
            }
            ClickKind::Extend => {
                let anchor = self.anchor.unwrap_or(index);
                let range = anchor.min(index)..=anchor.max(index);
                let ids: Vec<VariantId> = range
                    .filter_map(|at| self.row(at).map(|row| row.variant))
                    .collect();
                self.selected.extend(ids);
                self.focus = Some(index);
                self.focus_id = Some(id);
            }
        }
    }

    /// Replaces the selection, for a select-all that went to the catalog.
    pub fn set_selection(&mut self, ids: Vec<VariantId>) {
        self.selected = ids.into_iter().collect();
        self.selected.extend(self.focus_id);
    }

    /// Moves the focus to a selected row, leaving the selection alone.
    pub fn focus_variant(&mut self, id: VariantId) {
        self.focus_id = Some(id);
        self.focus = self.index_where(|row| row.variant == id);
    }

    /// Drops a row from the selection, passing the focus on if it was there.
    pub fn deselect(&mut self, id: VariantId) {
        self.selected.remove(&id);
        if self.focus_id == Some(id)
            && let Some(next) = self.selected.iter().next().copied()
        {
            self.focus_variant(next);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.selected.extend(self.focus_id);
    }

    /// Moves the focus, stopping at the ends. Stepping past what's loaded is fine, the focus
    /// settles on its row when that arrives.
    pub fn step(&mut self, delta: isize) {
        let Some(focus) = self.focus else {
            return;
        };
        let last = self.len().saturating_sub(1) as isize;
        let index = (focus as isize + delta).clamp(0, last) as usize;
        self.focus = Some(index);
        self.anchor = Some(index);
        self.need(index);
        self.focus_id = self.row(index).map(|row| row.variant);
        self.selected.clear();
        self.selected.extend(self.focus_id);
        self.need_around_focus();
    }

    /// Rows either side of the focus, nearest first.
    pub fn neighbours(&self, reach: usize) -> Vec<&LibraryRow> {
        let Some(focus) = self.focus else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for distance in 1..=reach {
            found.extend(self.row(focus + distance));
            found.extend(focus.checked_sub(distance).and_then(|index| self.row(index)));
        }
        found
    }

    /// Updates loaded rows ahead of the catalog, so a key press shows at once.
    pub fn update_rows(&mut self, ids: &[VariantId], change: impl Fn(&mut LibraryRow)) {
        for row in self.pages.values_mut().flatten() {
            if ids.contains(&row.variant) {
                change(row);
            }
        }
    }

    /// Whether changing a row's flag, rating or label could move it in or out of this view.
    pub fn depends_on_metadata(&self) -> bool {
        let filter = &self.filter;
        filter.flag.is_some()
            || filter.rating.is_some()
            || filter.label.is_some()
            || filter.collection.is_some()
    }
}

/// The filter's flag, stepped the way the filter bar's chip does.
pub fn next_flag_filter(current: Option<Flag>, clicked: Flag) -> Option<Flag> {
    (current != Some(clicked)).then_some(clicked)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use unlight_catalog::{Fingerprint, PhotoId, PhotoStatus};

    pub(crate) fn row(id: i64) -> LibraryRow {
        LibraryRow {
            variant: VariantId(id),
            photo: PhotoId(id),
            path: format!("/p/{id}.arw").into(),
            fingerprint: Fingerprint([id as u8; 16]),
            status: PhotoStatus::Available,
            capture_time: Some(id),
            width: 6000,
            height: 4000,
            orientation: 0,
            rating: 0,
            flag: Flag::Unflagged,
            label: Label::None,
            copy_name: None,
            edited: false,
        }
    }

    pub(crate) fn loaded(count: u64) -> Library {
        let mut library = Library::new();
        let generation = library.generation();
        library.apply_count(generation, count);
        let pages = count.div_ceil(PAGE_SIZE);
        for page in 0..pages {
            let start = page * PAGE_SIZE;
            let end = (start + PAGE_SIZE).min(count);
            let rows = (start..end).map(|id| row(id as i64)).collect();
            library.apply_page(generation, page, rows);
        }
        library
    }

    #[test]
    fn raw_files_are_recognised_case_insensitively() {
        assert!(is_raw(Path::new("a/DSC0001.ARW")));
        assert!(is_raw(Path::new("b.nef")));
        assert!(!is_raw(Path::new("c.jpg")));
        assert!(!is_raw(Path::new("noextension")));
    }

    #[test]
    fn the_first_row_takes_the_focus_when_it_arrives() {
        let library = loaded(3);
        assert_eq!(library.focus(), Some(0));
        assert_eq!(library.selection(), [VariantId(0)]);
    }

    #[test]
    fn stepping_stops_at_both_ends() {
        let mut library = loaded(3);
        library.step(-1);
        assert_eq!(library.focus(), Some(0));
        library.step(5);
        assert_eq!(library.focus(), Some(2));
    }

    #[test]
    fn stepping_replaces_the_selection() {
        let mut library = loaded(5);
        library.click(3, ClickKind::Toggle);
        library.step(1);
        assert_eq!(library.selection_len(), 1);
    }

    #[test]
    fn neighbours_come_nearest_first() {
        let mut library = loaded(5);
        library.select(2);
        let ids: Vec<_> = library.neighbours(2).iter().map(|row| row.variant.0).collect();
        assert_eq!(ids, [3, 1, 4, 0]);
    }

    #[test]
    fn replies_from_an_old_query_are_ignored() {
        let mut library = loaded(10);
        let stale = library.generation();
        library.set_filter(Filter {
            text: Some("x".into()),
            ..Filter::default()
        });
        library.apply_page(stale, 0, vec![row(99)]);
        library.apply_count(stale, 1);
        assert!(library.row(0).is_none());
        assert_eq!(library.count(), None);
    }

    #[test]
    fn a_filter_change_asks_for_its_count_and_first_page() {
        let mut library = loaded(10);
        library.take_count_request();
        library.take_page_requests();
        library.set_filter(Filter {
            text: Some("x".into()),
            ..Filter::default()
        });
        assert!(library.take_count_request());
        let requests = library.take_page_requests();
        assert_eq!(requests[0].index, 0);
        assert_eq!(requests[0].filter.text.as_deref(), Some("x"));
    }

    #[test]
    fn a_page_is_only_requested_once() {
        let mut library = Library::new();
        library.need(5);
        library.need(10);
        assert_eq!(library.take_page_requests().len(), 1);
        library.need(5);
        assert!(library.take_page_requests().is_empty());
    }

    #[test]
    fn a_deep_row_only_asks_for_its_own_page() {
        let mut library = Library::new();
        library.need(99_999);
        let requests = library.take_page_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].page().offset, 99_999 / PAGE_SIZE * PAGE_SIZE);
    }

    #[test]
    fn old_pages_are_let_go() {
        let mut library = Library::new();
        let generation = library.generation();
        library.apply_count(generation, 1_000_000);
        for page in 0..(MAX_PAGES as u64 + 20) {
            library.need((page * PAGE_SIZE) as usize);
            library.apply_page(generation, page, vec![row(page as i64)]);
        }
        assert!(library.pages.len() <= MAX_PAGES);
    }

    #[test]
    fn a_refresh_keeps_rows_until_the_new_ones_land() {
        let mut library = loaded(3);
        library.take_page_requests();
        library.refresh();
        assert!(library.row(1).is_some());
        assert_eq!(library.take_page_requests().len(), 1);
    }

    #[test]
    fn the_focus_follows_its_variant_when_rows_move() {
        let mut library = loaded(3);
        library.select(2);
        library.refresh();
        let generation = library.generation();
        library.apply_page(generation, 0, vec![row(2), row(0), row(1)]);
        assert_eq!(library.focus(), Some(0));
    }

    #[test]
    fn extending_selects_the_range_from_the_anchor() {
        let mut library = loaded(10);
        library.click(2, ClickKind::Replace);
        library.click(5, ClickKind::Extend);
        assert_eq!(library.selection().len(), 4);
        assert_eq!(library.focus(), Some(5));
    }

    #[test]
    fn toggling_adds_and_removes() {
        let mut library = loaded(5);
        library.click(1, ClickKind::Replace);
        library.click(3, ClickKind::Toggle);
        assert_eq!(library.selection_len(), 2);
        library.click(3, ClickKind::Toggle);
        assert_eq!(library.selection_len(), 1);
    }

    #[test]
    fn a_path_is_selected_when_its_row_turns_up() {
        let mut library = Library::new();
        library.select_path("/p/2.arw".into());
        let generation = library.generation();
        library.apply_count(generation, 3);
        library.apply_page(generation, 0, vec![row(0), row(1), row(2)]);
        assert_eq!(library.focus(), Some(2));
    }

    #[test]
    fn local_updates_land_on_loaded_rows() {
        let mut library = loaded(3);
        library.update_rows(&[VariantId(1)], |row| row.rating = 4);
        assert_eq!(library.row(1).map(|row| row.rating), Some(4));
        assert_eq!(library.row(0).map(|row| row.rating), Some(0));
    }

    #[test]
    fn the_rows_title_names_a_virtual_copy() {
        let mut copy = row(7);
        copy.copy_name = Some("Copy 1".into());
        assert_eq!(title(&copy), "7.arw / Copy 1");
    }

    #[test]
    fn clicking_the_active_flag_clears_the_filter() {
        assert_eq!(next_flag_filter(None, Flag::Pick), Some(Flag::Pick));
        assert_eq!(next_flag_filter(Some(Flag::Pick), Flag::Pick), None);
    }
}
