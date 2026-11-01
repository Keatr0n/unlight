mod common;

use common::{SyntheticRaw, TestCatalog};
use rusqlite::Connection;
use std::{sync::Arc, thread};
use unlight_catalog::{
    Catalog, Comparison, DateRange, Error, Filter, Flag, FolderFilter, HISTORY_CAP, KeywordId,
    Label, LibraryRow, MatchMode, Page, PartialRecipe, PhotoId, PhotoStatus, RatingFilter, Rule,
    SmartRules, Sort, SortKey, TextOp, VariantId,
};
use unlight_recipe::Recipe;

const ALL: Page = Page { offset: 0, limit: 10_000 };

/// Five photos across two cameras, three lenses and two folders.
struct Library {
    test: TestCatalog,
    one: VariantId,
    two: VariantId,
    three: VariantId,
    four: VariantId,
    five: VariantId,
}

fn raw(make: &str, model: &str, lens: &str, captured: &str, iso: u16) -> SyntheticRaw {
    SyntheticRaw {
        make: make.into(),
        model: model.into(),
        lens: lens.into(),
        captured: captured.into(),
        iso,
        seed: iso as u32,
        ..SyntheticRaw::default()
    }
}

impl Library {
    fn new() -> Self {
        let test = TestCatalog::new();
        raw("Acme", "One", "Wide 24mm", "2024:01:10 12:00:00", 100)
            .write(&test.path("trip/IMG_0001.dng"));
        raw("Acme", "One", "Tele 200mm", "2024:02:10 12:00:00", 400)
            .write(&test.path("trip/IMG_0002.dng"));
        raw("Acme", "One", "Wide 24mm", "2024:03:10 12:00:00", 800)
            .write(&test.path("trip/day2/IMG_0003.dng"));
        raw("Zenith", "Two", "Tele 200mm", "2023:12:25 12:00:00", 1600)
            .write(&test.path("home/dsc_0004.dng"));
        raw("Zenith", "Two", "Macro 100mm", "2024:06:01 12:00:00", 6400)
            .write(&test.path("home/dsc_0005.dng"));
        assert_eq!(test.scan().added, 5);
        let by_name = |name: &str| -> VariantId {
            let rows = test.catalog.query(&Filter::default(), Sort::default(), ALL).unwrap();
            rows.iter().find(|row| row.path.ends_with(name)).unwrap().variant
        };
        Self {
            one: by_name("IMG_0001.dng"),
            two: by_name("IMG_0002.dng"),
            three: by_name("IMG_0003.dng"),
            four: by_name("dsc_0004.dng"),
            five: by_name("dsc_0005.dng"),
            test,
        }
    }

    fn catalog(&self) -> &Catalog {
        &self.test.catalog
    }

    fn matching(&self, filter: &Filter) -> Vec<VariantId> {
        let mut ids: Vec<_> = self
            .catalog()
            .query(filter, Sort::default(), ALL)
            .unwrap()
            .into_iter()
            .map(|row| row.variant)
            .collect();
        ids.sort();
        assert_eq!(
            self.catalog().count(filter).unwrap(),
            ids.len() as u64,
            "count agrees with query"
        );
        ids
    }

    fn sorted(&self, ids: &[VariantId]) -> Vec<VariantId> {
        let mut ids = ids.to_vec();
        ids.sort();
        ids
    }

    fn folder_id(&self, name: &str) -> unlight_catalog::FolderId {
        let folders = self.catalog().folders().unwrap();
        folders.iter().find(|folder| folder.path.ends_with(name)).unwrap().id
    }

    fn smart(&self, rules: SmartRules) -> Vec<VariantId> {
        let collection = self.catalog().create_smart_collection("rules", &rules).unwrap();
        let ids = self.matching(&Filter { collection: Some(collection), ..Filter::default() });
        self.catalog().delete_collection(collection).unwrap();
        ids
    }

    fn rule(&self, rule: Rule) -> Vec<VariantId> {
        self.smart(SmartRules { mode: MatchMode::All, rules: vec![rule] })
    }
}

fn rating(comparison: Comparison, value: u8) -> Filter {
    Filter { rating: Some(RatingFilter { comparison, value }), ..Filter::default() }
}

#[test]
fn no_filter_matches_everything() {
    let library = Library::new();
    assert_eq!(library.matching(&Filter::default()).len(), 5);
}

#[test]
fn rating_comparisons() {
    let l = Library::new();
    l.catalog().set_rating(&[l.one], 2).unwrap();
    l.catalog().set_rating(&[l.two], 3).unwrap();
    l.catalog().set_rating(&[l.three], 5).unwrap();

    assert_eq!(l.matching(&rating(Comparison::Equal, 3)), l.sorted(&[l.two]));
    assert_eq!(l.matching(&rating(Comparison::GreaterOrEqual, 3)), l.sorted(&[l.two, l.three]));
    assert_eq!(l.matching(&rating(Comparison::Greater, 3)), l.sorted(&[l.three]));
    assert_eq!(l.matching(&rating(Comparison::LessOrEqual, 2)), l.sorted(&[l.one, l.four, l.five]));
    assert_eq!(l.matching(&rating(Comparison::Less, 2)), l.sorted(&[l.four, l.five]));
    assert_eq!(l.matching(&rating(Comparison::NotEqual, 0)), l.sorted(&[l.one, l.two, l.three]));
}

#[test]
fn flag_filter() {
    let l = Library::new();
    l.catalog().set_flag(&[l.one, l.two], Flag::Pick).unwrap();
    l.catalog().set_flag(&[l.three], Flag::Reject).unwrap();
    let flag = |flag| Filter { flag: Some(flag), ..Filter::default() };
    assert_eq!(l.matching(&flag(Flag::Pick)), l.sorted(&[l.one, l.two]));
    assert_eq!(l.matching(&flag(Flag::Reject)), l.sorted(&[l.three]));
    assert_eq!(l.matching(&flag(Flag::Unflagged)), l.sorted(&[l.four, l.five]));
}

#[test]
fn label_filter() {
    let l = Library::new();
    l.catalog().set_label(&[l.four], Label::Purple).unwrap();
    let label = |label| Filter { label: Some(label), ..Filter::default() };
    assert_eq!(l.matching(&label(Label::Purple)), l.sorted(&[l.four]));
    assert_eq!(l.matching(&label(Label::Red)), []);
    assert_eq!(l.matching(&label(Label::None)).len(), 4);
}

#[test]
fn camera_and_lens_filters_and_their_menus() {
    let l = Library::new();
    assert_eq!(l.catalog().cameras().unwrap(), ["Acme One", "Zenith Two"]);
    assert_eq!(l.catalog().lenses().unwrap(), ["Macro 100mm", "Tele 200mm", "Wide 24mm"]);
    let camera = Filter { camera: Some("Zenith Two".into()), ..Filter::default() };
    assert_eq!(l.matching(&camera), l.sorted(&[l.four, l.five]));
    let lens = Filter { lens: Some("Tele 200mm".into()), ..Filter::default() };
    assert_eq!(l.matching(&lens), l.sorted(&[l.two, l.four]));
}

#[test]
fn date_range_includes_from_and_excludes_until() {
    let l = Library::new();
    let time = |id: VariantId| {
        let photo = l.catalog().variant(id).unwrap().photo;
        l.catalog().photo(photo).unwrap().capture_time.unwrap()
    };
    let range =
        |from, until| Filter { captured: Some(DateRange { from, until }), ..Filter::default() };
    assert_eq!(l.matching(&range(Some(time(l.two)), None)), l.sorted(&[l.two, l.three, l.five]));
    assert_eq!(l.matching(&range(None, Some(time(l.two)))), l.sorted(&[l.one, l.four]));
    assert_eq!(
        l.matching(&range(Some(time(l.one)), Some(time(l.three)))),
        l.sorted(&[l.one, l.two])
    );
}

#[test]
fn keyword_filter_includes_descendants() {
    let l = Library::new();
    let animals = l.catalog().create_keyword(None, "Animals").unwrap();
    let birds = l.catalog().create_keyword(Some(animals), "Birds").unwrap();
    let owl = l.catalog().create_keyword(Some(birds), "Owl").unwrap();
    l.catalog().add_keyword(&[l.one], owl).unwrap();
    l.catalog().add_keyword(&[l.two], animals).unwrap();
    let keyword = |id: KeywordId| Filter { keyword: Some(id), ..Filter::default() };
    assert_eq!(l.matching(&keyword(owl)), l.sorted(&[l.one]));
    assert_eq!(l.matching(&keyword(birds)), l.sorted(&[l.one]));
    assert_eq!(l.matching(&keyword(animals)), l.sorted(&[l.one, l.two]));
}

#[test]
fn folder_filter_with_and_without_subfolders() {
    let l = Library::new();
    let trip = l.folder_id("trip");
    let direct = Filter {
        folder: Some(FolderFilter { folder: trip, include_subfolders: false }),
        ..Filter::default()
    };
    assert_eq!(l.matching(&direct), l.sorted(&[l.one, l.two]));
    let recursive = Filter {
        folder: Some(FolderFilter { folder: trip, include_subfolders: true }),
        ..Filter::default()
    };
    assert_eq!(l.matching(&recursive), l.sorted(&[l.one, l.two, l.three]));
}

#[test]
fn manual_collection_filter() {
    let l = Library::new();
    let collection = l.catalog().create_collection("Best").unwrap();
    l.catalog().add_to_collection(collection, &[l.two, l.five, l.two]).unwrap();
    let filter = Filter { collection: Some(collection), ..Filter::default() };
    assert_eq!(l.matching(&filter), l.sorted(&[l.two, l.five]));
    l.catalog().remove_from_collection(collection, &[l.two]).unwrap();
    assert_eq!(l.matching(&filter), l.sorted(&[l.five]));
}

#[test]
fn text_search_covers_names_gear_and_keywords() {
    let l = Library::new();
    let wedding = l.catalog().create_keyword(None, "Wedding").unwrap();
    l.catalog().add_keyword(&[l.three], wedding).unwrap();
    let text = |value: &str| Filter { text: Some(value.into()), ..Filter::default() };
    assert_eq!(l.matching(&text("IMG_0002")), l.sorted(&[l.two]));
    assert_eq!(l.matching(&text("dsc")).len(), 2);
    assert_eq!(l.matching(&text("zenith")).len(), 2);
    assert_eq!(l.matching(&text("macro")), l.sorted(&[l.five]));
    assert_eq!(l.matching(&text("WEDD")), l.sorted(&[l.three]));
    assert_eq!(l.matching(&text("acme tele")), l.sorted(&[l.two]), "every word must match");
    assert_eq!(l.matching(&text("   ")).len(), 5);
}

#[test]
fn text_search_treats_wildcards_literally() {
    let l = Library::new();
    let text = |value: &str| Filter { text: Some(value.into()), ..Filter::default() };
    assert_eq!(l.matching(&text("%")), []);
    assert_eq!(l.matching(&text("IMG_")).len(), 3, "the underscore is a literal character");
    assert_eq!(l.matching(&text("IMGx0001")), []);
    assert_eq!(l.matching(&text("x'; DROP TABLE photos; --")), []);
    assert_eq!(l.matching(&Filter::default()).len(), 5);
}

#[test]
fn edited_filter_follows_the_recipe() {
    let l = Library::new();
    let mut recipe = Recipe::default();
    recipe.basic.exposure = 0.5;
    l.catalog().save_recipe(l.one, &recipe, "Exposure").unwrap();
    let edited = |value| Filter { edited: Some(value), ..Filter::default() };
    assert_eq!(l.matching(&edited(true)), l.sorted(&[l.one]));
    assert_eq!(l.matching(&edited(false)).len(), 4);

    l.catalog().undo(l.one).unwrap();
    assert_eq!(l.matching(&edited(true)), [], "undoing back to the default isn't an edit");
    assert_eq!(l.catalog().variant(l.one).unwrap().edited_at, None);
}

#[test]
fn status_filter() {
    let l = Library::new();
    std::fs::remove_file(l.test.path("home/dsc_0004.dng")).unwrap();
    l.test.scan();
    let status = |value| Filter { status: Some(value), ..Filter::default() };
    assert_eq!(l.matching(&status(PhotoStatus::Missing)), l.sorted(&[l.four]));
    assert_eq!(l.matching(&status(PhotoStatus::Available)).len(), 4);
}

#[test]
fn filters_combine_with_and() {
    let l = Library::new();
    l.catalog().set_rating(&[l.one, l.four], 4).unwrap();
    let filter = Filter {
        rating: Some(RatingFilter { comparison: Comparison::GreaterOrEqual, value: 4 }),
        camera: Some("Acme One".into()),
        text: Some("wide".into()),
        ..Filter::default()
    };
    assert_eq!(l.matching(&filter), l.sorted(&[l.one]));
}

fn names(rows: &[LibraryRow]) -> Vec<String> {
    rows.iter().map(|row| row.path.file_stem().unwrap().to_string_lossy().into_owned()).collect()
}

#[test]
fn sort_by_capture_time() {
    let l = Library::new();
    let sort = |descending| {
        let rows = l.catalog().query(
            &Filter::default(),
            Sort { key: SortKey::CaptureTime, descending },
            ALL,
        );
        names(&rows.unwrap())
    };
    assert_eq!(sort(false), ["dsc_0004", "IMG_0001", "IMG_0002", "IMG_0003", "dsc_0005"]);
    assert_eq!(sort(true), ["dsc_0005", "IMG_0003", "IMG_0002", "IMG_0001", "dsc_0004"]);
}

#[test]
fn a_burst_within_one_second_stays_in_file_order() {
    let test = TestCatalog::new();
    let same_second = "2024:05:01 09:00:00";
    let shuffled =
        ["DSC_0007", "DSC_0001", "DSC_0005", "DSC_0003", "DSC_0002", "DSC_0006", "DSC_0004"];
    for (seed, name) in shuffled.iter().enumerate() {
        SyntheticRaw { captured: same_second.into(), seed: seed as u32, ..SyntheticRaw::default() }
            .write(&test.path(&format!("{name}.dng")));
    }
    test.scan();
    let order = |descending| {
        let sort = Sort { key: SortKey::CaptureTime, descending };
        names(&test.catalog.query(&Filter::default(), sort, ALL).unwrap())
    };
    let ascending =
        ["DSC_0001", "DSC_0002", "DSC_0003", "DSC_0004", "DSC_0005", "DSC_0006", "DSC_0007"];
    assert_eq!(order(false), ascending);
    let mut descending = ascending;
    descending.reverse();
    assert_eq!(order(true), descending);
}

#[test]
fn equal_ratings_fall_back_to_capture_order() {
    let l = Library::new();
    let rows = l.catalog().query(
        &Filter::default(),
        Sort { key: SortKey::Rating, descending: false },
        ALL,
    );
    assert_eq!(names(&rows.unwrap()), ["dsc_0004", "IMG_0001", "IMG_0002", "IMG_0003", "dsc_0005"]);
}

#[test]
fn virtual_copies_sit_next_to_their_master_in_time_order() {
    let l = Library::new();
    let copy = l.catalog().create_virtual_copy(l.two).unwrap();
    let rows = l.catalog().query(&Filter::default(), Sort::default(), ALL).unwrap();
    let position = |id| rows.iter().position(|row| row.variant == id).unwrap();
    assert_eq!(position(copy), position(l.two) + 1);
}

#[test]
fn sort_by_name_ignores_case() {
    let l = Library::new();
    let rows = l.catalog().query(
        &Filter::default(),
        Sort { key: SortKey::FileName, descending: false },
        ALL,
    );
    assert_eq!(names(&rows.unwrap()), ["dsc_0004", "dsc_0005", "IMG_0001", "IMG_0002", "IMG_0003"]);
}

#[test]
fn sort_by_rating() {
    let l = Library::new();
    l.catalog().set_rating(&[l.three], 5).unwrap();
    l.catalog().set_rating(&[l.one], 2).unwrap();
    let rows =
        l.catalog().query(&Filter::default(), Sort { key: SortKey::Rating, descending: true }, ALL);
    let rows = rows.unwrap();
    assert_eq!(rows[0].variant, l.three);
    assert_eq!(rows[1].variant, l.one);
}

#[test]
fn sort_by_edit_time() {
    let l = Library::new();
    let mut recipe = Recipe::default();
    recipe.basic.exposure = 0.5;
    l.catalog().save_recipe(l.two, &recipe, "a").unwrap();
    thread::sleep(std::time::Duration::from_millis(5));
    recipe.basic.exposure = 0.75;
    l.catalog().save_recipe(l.five, &recipe, "b").unwrap();
    let rows = l.catalog().query(
        &Filter::default(),
        Sort { key: SortKey::EditTime, descending: true },
        ALL,
    );
    let rows = rows.unwrap();
    assert_eq!(rows[0].variant, l.five);
    assert_eq!(rows[1].variant, l.two);
    assert!(rows[0].edited && rows[1].edited && !rows[2].edited);
}

#[test]
fn pages_are_stable_and_cover_everything_once() {
    let l = Library::new();
    for key in [SortKey::CaptureTime, SortKey::FileName, SortKey::Rating, SortKey::EditTime] {
        let sort = Sort { key, descending: true };
        let mut seen = Vec::new();
        for offset in 0..5 {
            let page =
                l.catalog().query(&Filter::default(), sort, Page { offset, limit: 1 }).unwrap();
            seen.push(page[0].variant);
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 5, "{key:?}");
    }
    let past_end =
        l.catalog().query(&Filter::default(), Sort::default(), Page { offset: 99, limit: 10 });
    assert!(past_end.unwrap().is_empty());
}

#[test]
fn rows_carry_what_a_grid_needs() {
    let l = Library::new();
    l.catalog().set_rating(&[l.one], 3).unwrap();
    l.catalog().set_flag(&[l.one], Flag::Pick).unwrap();
    l.catalog().set_label(&[l.one], Label::Blue).unwrap();
    let copy = l.catalog().create_virtual_copy(l.one).unwrap();
    let rows = l.catalog().query(&Filter::default(), Sort::default(), ALL).unwrap();
    let master = rows.iter().find(|row| row.variant == l.one).unwrap();
    let source = master.preview_source();
    assert_eq!(source.fingerprint, master.fingerprint);
    assert_eq!(source.path, master.path);
    assert_eq!((master.rating, master.flag, master.label), (3, Flag::Pick, Label::Blue));
    assert_eq!((master.width, master.height), (64, 48));
    assert_eq!(master.copy_name, None);
    let copied = rows.iter().find(|row| row.variant == copy).unwrap();
    assert_eq!(copied.copy_name.as_deref(), Some("Copy 1"));
    assert_eq!(copied.photo, master.photo);
}

#[test]
fn every_rule_type_selects_the_right_rows() {
    let l = Library::new();
    l.catalog().set_rating(&[l.one], 4).unwrap();
    l.catalog().set_flag(&[l.two], Flag::Pick).unwrap();
    l.catalog().set_label(&[l.three], Label::Green).unwrap();
    let mut recipe = Recipe::default();
    recipe.basic.exposure = 0.5;
    l.catalog().save_recipe(l.four, &recipe, "x").unwrap();
    let parent = l.catalog().create_keyword(None, "Family").unwrap();
    let child = l.catalog().create_keyword(Some(parent), "Grandma").unwrap();
    l.catalog().add_keyword(&[l.five], child).unwrap();

    let text = |op, value: &str| (op, value.to_owned());
    assert_eq!(
        l.rule(Rule::Rating { op: Comparison::GreaterOrEqual, value: 4 }),
        l.sorted(&[l.one])
    );
    assert_eq!(l.rule(Rule::Flag { value: Flag::Pick }), l.sorted(&[l.two]));
    assert_eq!(l.rule(Rule::Label { value: Label::Green }), l.sorted(&[l.three]));
    assert_eq!(l.rule(Rule::Edited { value: true }), l.sorted(&[l.four]));

    let (op, value) = text(TextOp::Is, "ACME ONE");
    assert_eq!(l.rule(Rule::Camera { op, value }).len(), 3);
    let (op, value) = text(TextOp::IsNot, "Acme One");
    assert_eq!(l.rule(Rule::Camera { op, value }), l.sorted(&[l.four, l.five]));
    let (op, value) = text(TextOp::Contains, "200");
    assert_eq!(l.rule(Rule::Lens { op, value }), l.sorted(&[l.two, l.four]));
    let (op, value) = text(TextOp::Excludes, "200");
    assert_eq!(l.rule(Rule::Lens { op, value }).len(), 3);
    let (op, value) = text(TextOp::StartsWith, "dsc_");
    assert_eq!(l.rule(Rule::FileName { op, value }), l.sorted(&[l.four, l.five]));
    let (op, value) = text(TextOp::EndsWith, "0001.dng");
    assert_eq!(l.rule(Rule::FileName { op, value }), l.sorted(&[l.one]));

    let (op, value) = text(TextOp::Is, "grandma");
    assert_eq!(l.rule(Rule::Keyword { op, value }), l.sorted(&[l.five]));
    let (op, value) = text(TextOp::Contains, "gran");
    assert_eq!(l.rule(Rule::Keyword { op, value }), l.sorted(&[l.five]));
    let (op, value) = text(TextOp::Excludes, "gran");
    assert_eq!(l.rule(Rule::Keyword { op, value }).len(), 4, "photos with no keywords pass");

    let trip =
        l.catalog().folders().unwrap().into_iter().find(|f| f.path.ends_with("trip")).unwrap();
    let path = trip.path.to_string_lossy().into_owned();
    assert_eq!(l.rule(Rule::Folder { path }), l.sorted(&[l.one, l.two, l.three]));

    let photo = l.catalog().variant(l.two).unwrap().photo;
    let captured = l.catalog().photo(photo).unwrap().capture_time.unwrap();
    assert_eq!(
        l.rule(Rule::CaptureTime { op: Comparison::Less, value: captured }),
        l.sorted(&[l.one, l.four])
    );
    assert_eq!(
        l.rule(Rule::Iso { op: Comparison::Greater, value: 800.0 }),
        l.sorted(&[l.four, l.five])
    );
    assert_eq!(l.rule(Rule::Aperture { op: Comparison::Equal, value: 2.8 }).len(), 5);
    assert_eq!(l.rule(Rule::FocalLength { op: Comparison::Less, value: 50.0 }), []);
    assert_eq!(l.rule(Rule::Shutter { op: Comparison::Equal, value: 0.004 }).len(), 5);
}

#[test]
fn rule_modes_and_nesting() {
    let l = Library::new();
    l.catalog().set_rating(&[l.one, l.four], 5).unwrap();
    let five_stars = || Rule::Rating { op: Comparison::Equal, value: 5 };
    let zenith = || Rule::Camera { op: TextOp::Contains, value: "Zenith".into() };

    let all = SmartRules { mode: MatchMode::All, rules: vec![five_stars(), zenith()] };
    assert_eq!(l.smart(all), l.sorted(&[l.four]));
    let any = SmartRules { mode: MatchMode::Any, rules: vec![five_stars(), zenith()] };
    assert_eq!(l.smart(any), l.sorted(&[l.one, l.four, l.five]));
    let none = SmartRules { mode: MatchMode::None, rules: vec![five_stars(), zenith()] };
    assert_eq!(l.smart(none), l.sorted(&[l.two, l.three]));

    let nested = SmartRules {
        mode: MatchMode::All,
        rules: vec![
            Rule::Iso { op: Comparison::GreaterOrEqual, value: 400.0 },
            Rule::Group(SmartRules { mode: MatchMode::Any, rules: vec![five_stars(), zenith()] }),
        ],
    };
    assert_eq!(l.smart(nested), l.sorted(&[l.four, l.five]));
}

#[test]
fn a_smart_collection_combines_with_other_filters() {
    let l = Library::new();
    let rules = SmartRules {
        mode: MatchMode::All,
        rules: vec![Rule::Camera { op: TextOp::Is, value: "Acme One".into() }],
    };
    let collection = l.catalog().create_smart_collection("Acme", &rules).unwrap();
    let filter = Filter {
        collection: Some(collection),
        lens: Some("Wide 24mm".into()),
        ..Filter::default()
    };
    assert_eq!(l.matching(&filter), l.sorted(&[l.one, l.three]));
}

#[test]
fn smart_rules_update_and_round_trip_through_the_catalog() {
    let l = Library::new();
    let rules = SmartRules { mode: MatchMode::Any, rules: vec![Rule::Flag { value: Flag::Pick }] };
    let collection = l.catalog().create_smart_collection("Picks", &rules).unwrap();
    let listed = l.catalog().collections().unwrap();
    assert_eq!(listed[0].rules.as_ref(), Some(&rules));

    let changed = SmartRules { mode: MatchMode::None, rules: rules.rules.clone() };
    l.catalog().set_smart_rules(collection, &changed).unwrap();
    assert_eq!(l.catalog().collections().unwrap()[0].rules, Some(changed));
    l.catalog().rename_collection(collection, "Not picks").unwrap();
    assert_eq!(l.catalog().collections().unwrap()[0].name, "Not picks");
}

#[test]
fn a_manual_collection_has_no_rules_to_update() {
    let l = Library::new();
    let collection = l.catalog().create_collection("Manual").unwrap();
    let error = l.catalog().set_smart_rules(collection, &SmartRules::default()).unwrap_err();
    assert!(matches!(error, Error::NotFound { .. }));
}

#[test]
fn an_unknown_collection_is_an_error_not_an_empty_list() {
    let l = Library::new();
    let filter =
        Filter { collection: Some(unlight_catalog::CollectionId(999)), ..Filter::default() };
    assert!(matches!(
        l.catalog().query(&filter, Sort::default(), ALL),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn corrupt_stored_rules_are_an_error_not_a_panic() {
    let l = Library::new();
    let collection = l.catalog().create_smart_collection("x", &SmartRules::default()).unwrap();
    let conn = Connection::open(l.test.dir.path().join("catalog.db")).unwrap();
    conn.execute("UPDATE collections SET rules = 'not json'", []).unwrap();
    let filter = Filter { collection: Some(collection), ..Filter::default() };
    assert!(matches!(l.catalog().query(&filter, Sort::default(), ALL), Err(Error::Json(_))));
}

#[test]
fn removing_keywords_collections_and_photos_cleans_up() {
    let l = Library::new();
    let parent = l.catalog().create_keyword(None, "A").unwrap();
    let child = l.catalog().create_keyword(Some(parent), "B").unwrap();
    l.catalog().add_keyword(&[l.one], child).unwrap();
    let collection = l.catalog().create_collection("C").unwrap();
    l.catalog().add_to_collection(collection, &[l.one]).unwrap();

    l.catalog().delete_keyword(parent).unwrap();
    assert!(l.catalog().keywords().unwrap().is_empty());
    assert!(l.catalog().keywords_of(l.one).unwrap().is_empty());

    let photo = l.catalog().variant(l.one).unwrap().photo;
    l.catalog().remove_photos(&[photo]).unwrap();
    assert!(matches!(l.catalog().variant(l.one), Err(Error::NotFound { .. })));
    let filter = Filter { collection: Some(collection), ..Filter::default() };
    assert_eq!(l.matching(&filter), []);

    let trip = l.folder_id("trip");
    l.catalog().remove_folder(trip).unwrap();
    assert_eq!(l.matching(&Filter::default()).len(), 2, "the folder's subfolders go too");
}

#[test]
fn keywords_are_unique_per_parent_ignoring_case() {
    let l = Library::new();
    let first = l.catalog().create_keyword(None, "Owl").unwrap();
    assert_eq!(l.catalog().create_keyword(None, "owl").unwrap(), first);
    let parent = l.catalog().create_keyword(None, "Birds").unwrap();
    let nested = l.catalog().create_keyword(Some(parent), "Owl").unwrap();
    assert_ne!(nested, first);
}

#[test]
fn keyword_listing_puts_parents_first_and_renames_stick() {
    let l = Library::new();
    let parent = l.catalog().create_keyword(None, "Zoo").unwrap();
    let child = l.catalog().create_keyword(Some(parent), "Ant").unwrap();
    let names: Vec<_> = l.catalog().keywords().unwrap().into_iter().map(|k| k.id).collect();
    assert_eq!(names, [parent, child]);
    l.catalog().rename_keyword(child, "Bee").unwrap();
    assert_eq!(l.catalog().keywords().unwrap()[1].name, "Bee");
    assert!(matches!(l.catalog().rename_keyword(KeywordId(999), "x"), Err(Error::NotFound { .. })));
}

#[test]
fn ratings_are_validated_and_bad_ids_are_reported() {
    let l = Library::new();
    assert!(matches!(l.catalog().set_rating(&[l.one], 6), Err(Error::InvalidRating(6))));
    let missing = VariantId(99_999);
    assert!(matches!(l.catalog().set_rating(&[missing], 1), Err(Error::NotFound { .. })));
    assert_eq!(l.catalog().variant(l.one).unwrap().rating, 0);
}

#[test]
fn a_failed_bulk_edit_changes_nothing() {
    let l = Library::new();
    assert!(l.catalog().set_rating(&[l.one, VariantId(99_999), l.two], 5).is_err());
    assert_eq!(l.catalog().variant(l.one).unwrap().rating, 0);
}

fn exposure(value: f32) -> Recipe {
    let mut recipe = Recipe::default();
    recipe.basic.exposure = value;
    recipe
}

#[test]
fn undo_and_redo_walk_the_history() {
    let l = Library::new();
    let id = l.one;
    l.catalog().save_recipe(id, &exposure(1.0), "first").unwrap();
    l.catalog().save_recipe(id, &exposure(2.0), "second").unwrap();
    l.catalog().save_recipe(id, &exposure(3.0), "third").unwrap();

    assert_eq!(l.catalog().undo(id).unwrap(), Some(exposure(2.0)));
    assert_eq!(l.catalog().variant(id).unwrap().recipe, exposure(2.0));
    assert_eq!(l.catalog().undo(id).unwrap(), Some(exposure(1.0)));
    assert_eq!(
        l.catalog().undo(id).unwrap(),
        Some(Recipe::default()),
        "back to before the first edit"
    );
    assert_eq!(l.catalog().undo(id).unwrap(), None);
    assert_eq!(l.catalog().variant(id).unwrap().recipe, Recipe::default());

    assert_eq!(l.catalog().redo(id).unwrap(), Some(exposure(1.0)));
    assert_eq!(l.catalog().redo(id).unwrap(), Some(exposure(2.0)));
    assert_eq!(l.catalog().redo(id).unwrap(), Some(exposure(3.0)));
    assert_eq!(l.catalog().redo(id).unwrap(), None);
    assert_eq!(l.catalog().variant(id).unwrap().recipe, exposure(3.0));
}

#[test]
fn a_new_edit_after_undo_discards_the_redo_steps() {
    let l = Library::new();
    let id = l.one;
    l.catalog().save_recipe(id, &exposure(1.0), "first").unwrap();
    l.catalog().save_recipe(id, &exposure(2.0), "second").unwrap();
    l.catalog().undo(id).unwrap();
    l.catalog().save_recipe(id, &exposure(9.0), "branch").unwrap();

    assert_eq!(l.catalog().redo(id).unwrap(), None);
    let labels: Vec<_> =
        l.catalog().history(id).unwrap().steps.into_iter().map(|s| s.label).collect();
    assert_eq!(labels, ["Start", "first", "branch"]);
    assert_eq!(l.catalog().undo(id).unwrap(), Some(exposure(1.0)));
}

#[test]
fn saving_an_unchanged_recipe_adds_no_step() {
    let l = Library::new();
    l.catalog().save_recipe(l.one, &Recipe::default(), "nothing").unwrap();
    assert!(l.catalog().history(l.one).unwrap().steps.is_empty());
    l.catalog().save_recipe(l.one, &exposure(1.0), "a").unwrap();
    l.catalog().save_recipe(l.one, &exposure(1.0), "same").unwrap();
    assert_eq!(l.catalog().history(l.one).unwrap().steps.len(), 2);
}

#[test]
fn an_untouched_variant_has_no_history_to_undo() {
    let l = Library::new();
    assert_eq!(l.catalog().undo(l.one).unwrap(), None);
    assert_eq!(l.catalog().redo(l.one).unwrap(), None);
    let history = l.catalog().history(l.one).unwrap();
    assert!(history.steps.is_empty());
    assert_eq!(history.position, None);
}

#[test]
fn history_reports_the_current_position_and_can_jump() {
    let l = Library::new();
    let id = l.one;
    l.catalog().save_recipe(id, &exposure(1.0), "a").unwrap();
    l.catalog().save_recipe(id, &exposure(2.0), "b").unwrap();
    let history = l.catalog().history(id).unwrap();
    assert_eq!(history.steps.len(), 3);
    assert_eq!(history.position, Some(history.steps[2].seq));

    let first_edit = history.steps[1].seq;
    assert_eq!(l.catalog().go_to_history_step(id, first_edit).unwrap(), exposure(1.0));
    assert_eq!(l.catalog().history(id).unwrap().position, Some(first_edit));
    assert_eq!(l.catalog().redo(id).unwrap(), Some(exposure(2.0)), "later steps survive a jump");
    assert!(matches!(l.catalog().go_to_history_step(id, 999), Err(Error::NotFound { .. })));
}

#[test]
fn history_is_capped_and_the_oldest_step_becomes_the_undo_limit() {
    let l = Library::new();
    let id = l.one;
    let edits = HISTORY_CAP + 20;
    for step in 0..edits {
        l.catalog().save_recipe(id, &exposure(step as f32 / 100.0), "edit").unwrap();
    }
    let history = l.catalog().history(id).unwrap();
    assert_eq!(history.steps.len() as i64, HISTORY_CAP);
    assert_eq!(history.steps.last().unwrap().seq, history.position.unwrap());

    let mut undone = 0;
    while l.catalog().undo(id).unwrap().is_some() {
        undone += 1;
    }
    assert_eq!(undone, HISTORY_CAP - 1);
    assert_eq!(
        l.catalog().variant(id).unwrap().recipe,
        exposure((edits - HISTORY_CAP) as f32 / 100.0)
    );
}

#[test]
fn the_cap_applies_per_variant() {
    let l = Library::new();
    l.catalog().save_recipe(l.two, &exposure(1.0), "keep").unwrap();
    for step in 0..HISTORY_CAP + 5 {
        l.catalog().save_recipe(l.one, &exposure(step as f32), "edit").unwrap();
    }
    assert_eq!(l.catalog().history(l.two).unwrap().steps.len(), 2);
}

#[test]
fn a_recipe_with_nan_is_refused_before_it_can_corrupt_a_variant() {
    let l = Library::new();
    let mut bad = Recipe::default();
    bad.basic.exposure = f32::NAN;
    assert!(matches!(l.catalog().save_recipe(l.one, &bad, "bad"), Err(Error::InvalidRecipe)));
    assert_eq!(l.catalog().variant(l.one).unwrap().recipe, Recipe::default());
}

#[test]
fn a_saved_recipe_survives_reopening_the_catalog() {
    let l = Library::new();
    let mut recipe = exposure(1.5);
    recipe.masks.push(unlight_recipe::Mask { name: "Sky".into(), ..Default::default() });
    l.catalog().save_recipe(l.one, &recipe, "x").unwrap();
    let path = l.test.dir.path().join("catalog.db");
    let reopened = Catalog::open(&path).unwrap();
    assert_eq!(reopened.variant(l.one).unwrap().recipe, recipe);
    assert_eq!(reopened.history(l.one).unwrap().steps.len(), 2);
}

#[test]
fn virtual_copies_stay_independent() {
    let l = Library::new();
    let master = l.one;
    let keyword = l.catalog().create_keyword(None, "Shared").unwrap();
    l.catalog().add_keyword(&[master], keyword).unwrap();
    l.catalog().set_rating(&[master], 3).unwrap();
    l.catalog().set_label(&[master], Label::Red).unwrap();
    l.catalog().set_flag(&[master], Flag::Pick).unwrap();
    l.catalog().save_recipe(master, &exposure(1.0), "base").unwrap();

    let copy = l.catalog().create_virtual_copy(master).unwrap();
    let copied = l.catalog().variant(copy).unwrap();
    assert_eq!(copied.recipe, exposure(1.0));
    assert_eq!((copied.rating, copied.label), (3, Label::Red));
    assert_eq!(copied.flag, Flag::Unflagged, "a flag is a culling decision about one variant");
    assert_eq!(copied.copy_name.as_deref(), Some("Copy 1"));
    assert_eq!(l.catalog().keywords_of(copy).unwrap().len(), 1);
    assert!(l.catalog().history(copy).unwrap().steps.is_empty());

    l.catalog().save_recipe(copy, &exposure(-2.0), "copy edit").unwrap();
    l.catalog().set_rating(&[copy], 1).unwrap();
    let other = l.catalog().create_keyword(None, "Only copy").unwrap();
    l.catalog().add_keyword(&[copy], other).unwrap();
    l.catalog().remove_keyword(&[copy], keyword).unwrap();

    let original = l.catalog().variant(master).unwrap();
    assert_eq!(original.recipe, exposure(1.0));
    assert_eq!(original.rating, 3);
    assert_eq!(l.catalog().keywords_of(master).unwrap()[0].id, keyword);
    assert_eq!(l.catalog().keywords_of(copy).unwrap()[0].id, other);

    assert_eq!(l.catalog().undo(copy).unwrap(), Some(exposure(1.0)));
    assert_eq!(l.catalog().variant(master).unwrap().recipe, exposure(1.0));
    assert_eq!(l.catalog().history(master).unwrap().steps.len(), 2);
}

#[test]
fn copies_are_listed_after_the_master_and_named_in_turn() {
    let l = Library::new();
    let first = l.catalog().create_virtual_copy(l.one).unwrap();
    let second = l.catalog().create_virtual_copy(first).unwrap();
    let photo = l.catalog().variant(l.one).unwrap().photo;
    let variants = l.catalog().variants_of(photo).unwrap();
    let ids: Vec<_> = variants.iter().map(|variant| variant.id).collect();
    assert_eq!(ids, [l.one, first, second]);
    assert_eq!(variants[2].copy_name.as_deref(), Some("Copy 2"));
    assert_eq!(l.matching(&Filter::default()).len(), 7);
}

#[test]
fn a_copy_can_be_deleted_but_a_master_cannot() {
    let l = Library::new();
    let copy = l.catalog().create_virtual_copy(l.one).unwrap();
    assert!(matches!(l.catalog().delete_variant(l.one), Err(Error::MasterVariant)));
    l.catalog().delete_variant(copy).unwrap();
    assert!(matches!(l.catalog().variant(copy), Err(Error::NotFound { .. })));
    assert_eq!(l.matching(&Filter::default()).len(), 5);
}

#[test]
fn a_copy_never_touches_the_sidecar_state() {
    let l = Library::new();
    let copy = l.catalog().create_virtual_copy(l.one).unwrap();
    l.catalog().set_rating(&[copy], 5).unwrap();
    assert!(l.catalog().dirty_sidecars().unwrap().is_empty());
    l.catalog().set_rating(&[l.one], 5).unwrap();
    let photo: PhotoId = l.catalog().variant(l.one).unwrap().photo;
    assert_eq!(l.catalog().dirty_sidecars().unwrap(), [photo]);
}

#[test]
fn snapshots_capture_and_restore_as_an_undoable_step() {
    let l = Library::new();
    let id = l.one;
    l.catalog().save_recipe(id, &exposure(1.0), "a").unwrap();
    let snapshot = l.catalog().create_snapshot(id, "Before colour").unwrap();
    l.catalog().save_recipe(id, &exposure(2.0), "b").unwrap();

    let listed = l.catalog().snapshots(id).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].name.as_str(), listed[0].recipe.clone()),
        ("Before colour", exposure(1.0))
    );

    assert_eq!(l.catalog().apply_snapshot(snapshot).unwrap(), exposure(1.0));
    assert_eq!(l.catalog().variant(id).unwrap().recipe, exposure(1.0));
    let history = l.catalog().history(id).unwrap();
    assert_eq!(history.steps.last().unwrap().label, "Snapshot: Before colour");
    assert_eq!(l.catalog().undo(id).unwrap(), Some(exposure(2.0)));

    l.catalog().delete_snapshot(snapshot).unwrap();
    assert!(l.catalog().snapshots(id).unwrap().is_empty());
    assert!(matches!(l.catalog().apply_snapshot(snapshot), Err(Error::NotFound { .. })));
}

#[test]
fn presets_are_grouped_replaced_by_name_and_applied() {
    let l = Library::new();
    let warm = PartialRecipe::between(&Recipe::default(), &exposure(0.5)).unwrap();
    let cool = PartialRecipe::between(&Recipe::default(), &exposure(-0.5)).unwrap();
    let first = l.catalog().save_preset("Portrait", "Warm", &warm).unwrap();
    l.catalog().save_preset("Landscape", "Cool", &cool).unwrap();
    let again = l.catalog().save_preset("Portrait", "Warm", &cool).unwrap();
    assert_eq!(first, again, "same group and name replaces");

    let presets = l.catalog().presets().unwrap();
    let keys: Vec<_> = presets.iter().map(|p| (p.group.as_str(), p.name.as_str())).collect();
    assert_eq!(keys, [("Landscape", "Cool"), ("Portrait", "Warm")]);

    let mut target = Recipe::default();
    target.basic.contrast = 15.0;
    let applied = presets[1].settings.apply_to(&target).unwrap();
    assert_eq!((applied.basic.exposure, applied.basic.contrast), (-0.5, 15.0));

    l.catalog().delete_preset(first).unwrap();
    assert_eq!(l.catalog().presets().unwrap().len(), 1);
}

#[test]
fn the_catalog_can_be_shared_across_threads() {
    fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<Catalog>();
    assert_shareable::<unlight_catalog::PreviewCache>();

    let library = Arc::new(Library::new());
    let writers: Vec<_> = (0..8)
        .map(|worker| {
            let library = Arc::clone(&library);
            thread::spawn(move || {
                for step in 0..25 {
                    let id = [library.one, library.two, library.three][worker % 3];
                    library.catalog().set_rating(&[id], (step % 6) as u8).unwrap();
                    library.catalog().save_recipe(id, &exposure(step as f32), "edit").unwrap();
                    assert_eq!(library.catalog().count(&Filter::default()).unwrap(), 5);
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let history = library.catalog().history(library.one).unwrap();
    assert!(!history.steps.is_empty());
    assert_eq!(history.steps.last().unwrap().seq, history.position.unwrap());
}

#[test]
fn opening_a_catalog_turns_on_wal_and_foreign_keys() {
    let test = TestCatalog::new();
    let conn = Connection::open(test.dir.path().join("catalog.db")).unwrap();
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
    assert_eq!(mode, "wal");
    test.catalog.optimize().unwrap();
}

#[test]
fn a_catalog_from_a_newer_build_is_refused_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    drop(Catalog::open(&path).unwrap());
    Connection::open(&path).unwrap().pragma_update(None, "user_version", 9999).unwrap();
    assert!(matches!(Catalog::open(&path), Err(Error::SchemaTooNew { found: 9999, .. })));
}

#[test]
fn a_file_that_is_not_a_database_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    std::fs::write(&path, vec![0x42; 4096]).unwrap();
    assert!(Catalog::open(&path).is_err());
}
