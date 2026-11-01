//! XMP sidecars. Reading builds a small tree of the document. Writing streams it instead, so
//! that everything Unlight doesn't own, such as Lightroom's `crs:` settings, comes through
//! byte for byte. Unlight's own properties go in a separate `rdf:Description`, which declares
//! its namespaces itself and so can't clash with whatever prefixes the file already uses.

use crate::{
    Error, Flag, Label, Result,
    error::io_error,
    files::{Durability, write_atomic},
};
use quick_xml::{
    NsReader, Writer, XmlVersion,
    escape::{escape, partial_escape, resolve_predefined_entity},
    events::{BytesRef, BytesStart, BytesText, Event, attributes::Attribute},
    name::{NamespaceResolver, QName, ResolveResult},
};
use std::{
    borrow::Cow,
    fs, io,
    path::{Path, PathBuf},
};
use unlight_recipe::Recipe;

const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XMP_NS: &str = "http://ns.adobe.com/xap/1.0/";
const DC_NS: &str = "http://purl.org/dc/elements/1.1/";
const LR_NS: &str = "http://ns.adobe.com/lightroom/1.0/";
const UNLIGHT_NS: &str = "urn:unlight:xmp:1.0#";

const EMPTY_DOCUMENT: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Unlight">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 </rdf:RDF>
</x:xmpmeta>
"#;

const HIERARCHY_SEPARATOR: char = '|';
/// The tree is walked and dropped recursively, so a hostile file mustn't nest without limit.
/// Real XMP is a handful of levels deep.
const MAX_DEPTH: usize = 64;
/// Bridge and others store a rejected photo as this rating.
const REJECTED_RATING: f32 = -1.0;

/// The metadata Unlight reads from and writes to a sidecar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XmpData {
    pub rating: Option<u8>,
    pub flag: Option<Flag>,
    pub label: Option<Label>,
    /// Each keyword as a path from the root of its hierarchy.
    pub keywords: Vec<Vec<String>>,
    pub recipe: Option<Recipe>,
}

/// Where a raw's sidecar lives: an existing `IMG_1.ARW.xmp` if that is what's there,
/// otherwise Lightroom's `IMG_1.xmp`.
pub fn sidecar_path(raw: &Path) -> PathBuf {
    let candidates = sidecar_candidates(raw);
    let existing = candidates.iter().find(|candidate| candidate.exists());
    existing.unwrap_or(&candidates[0]).clone()
}

pub(crate) fn sidecar_candidates(raw: &Path) -> [PathBuf; 3] {
    let mut appended = raw.as_os_str().to_owned();
    appended.push(".xmp");
    [raw.with_extension("xmp"), raw.with_extension("XMP"), appended.into()]
}

pub fn read_sidecar(path: &Path) -> Result<XmpData> {
    let xml = fs::read(path).map_err(io_error(path))?;
    parse(&xml).map_err(|detail| malformed(path, detail))
}

/// Updates the properties in `data` and keeps the rest of the file. A file that isn't valid
/// XMP is an error and stays as it is, since it may hold something we can't see.
pub fn write_sidecar(path: &Path, data: &XmpData) -> Result<()> {
    let merged = match fs::read(path) {
        Ok(existing) => merge(&existing, data),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            merge(EMPTY_DOCUMENT.as_bytes(), data)
        }
        Err(error) => return Err(io_error(path)(error)),
    };
    let merged = merged.map_err(|detail| malformed(path, detail))?;
    write_atomic(path, &merged, Durability::Synced).map_err(io_error(path))
}

fn malformed(path: &Path, detail: String) -> Error {
    Error::MalformedXmp { path: path.to_owned(), detail }
}

/// A property Unlight reads and rewrites. Anything else in a sidecar belongs to someone else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Property {
    Rating,
    Label,
    Subject,
    Hierarchy,
    Flag,
    Recipe,
}

impl Property {
    fn identify(namespace: &str, local: &str) -> Option<Self> {
        match (namespace, local) {
            (XMP_NS, "Rating") => Some(Self::Rating),
            (XMP_NS, "Label") => Some(Self::Label),
            (DC_NS, "subject") => Some(Self::Subject),
            (LR_NS, "hierarchicalSubject") => Some(Self::Hierarchy),
            (UNLIGHT_NS, "Flag") => Some(Self::Flag),
            (UNLIGHT_NS, "Recipe") => Some(Self::Recipe),
            _ => None,
        }
    }
}

type ParseResult<T> = std::result::Result<T, String>;

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn namespace_of<'a>(resolved: &ResolveResult<'a>) -> &'a str {
    match resolved {
        ResolveResult::Bound(namespace) => namespace.0,
        _ => "",
    }
}

#[derive(Debug, Default)]
struct Node {
    namespace: String,
    local: String,
    attributes: Vec<NodeAttribute>,
    children: Vec<Node>,
    text: String,
}

#[derive(Debug)]
struct NodeAttribute {
    namespace: String,
    local: String,
    value: String,
}

impl Node {
    fn from_start(
        resolver: &NamespaceResolver,
        namespace: &str,
        start: &BytesStart,
    ) -> ParseResult<Self> {
        let mut attributes = Vec::new();
        for attribute in start.attributes() {
            let attribute = attribute.map_err(message)?;
            if attribute.key.as_namespace_binding().is_some() {
                continue;
            }
            let (resolved, local) = resolver.resolve_attribute(attribute.key);
            let value = attribute.normalized_value(XmlVersion::Implicit1_0).map_err(message)?;
            attributes.push(NodeAttribute {
                namespace: namespace_of(&resolved).to_owned(),
                local: local.as_ref().to_owned(),
                value: value.into_owned(),
            });
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            local: start.local_name().as_ref().to_owned(),
            attributes,
            ..Self::default()
        })
    }

    fn is_rdf(&self, local: &str) -> bool {
        self.namespace == RDF_NS && self.local == local
    }

    fn find_rdf_root(&self) -> Option<&Node> {
        if self.is_rdf("RDF") {
            return Some(self);
        }
        self.children.iter().find_map(Node::find_rdf_root)
    }

    /// A property's values: its list items or, from a lenient writer, its plain text.
    fn values(&self) -> Vec<String> {
        let lists = self
            .children
            .iter()
            .filter(|child| child.is_rdf("Bag") || child.is_rdf("Seq") || child.is_rdf("Alt"));
        let items = lists.flat_map(|list| &list.children).filter(|child| child.is_rdf("li"));
        let values: Vec<_> = items.map(|item| item.text.trim().to_owned()).collect();
        if values.is_empty() && !self.text.trim().is_empty() {
            return vec![self.text.trim().to_owned()];
        }
        values
    }
}

fn reference_text(reference: &BytesRef) -> ParseResult<String> {
    if let Some(character) = reference.resolve_char_ref().map_err(message)? {
        return Ok(character.to_string());
    }
    let entity = reference.xml10_content();
    resolve_predefined_entity(&entity)
        .map(str::to_owned)
        .ok_or_else(|| format!("unknown entity &{entity};"))
}

fn read_tree(xml: &str) -> ParseResult<Node> {
    let mut reader = NsReader::from_str(xml);
    let mut open = vec![Node::default()];
    loop {
        let (resolved, event) = reader.read_resolved_event().map_err(message)?;
        let namespace = namespace_of(&resolved).to_owned();
        let depth = open.len();
        let Some(top) = open.last_mut() else {
            return Err("unbalanced elements".to_owned());
        };
        match event {
            Event::Start(_) if depth > MAX_DEPTH => return Err("nested too deeply".to_owned()),
            Event::Start(start) => {
                open.push(Node::from_start(reader.resolver(), &namespace, &start)?)
            }
            Event::Empty(start) => {
                top.children.push(Node::from_start(reader.resolver(), &namespace, &start)?)
            }
            Event::Text(text) => top.text.push_str(&text.xml10_content()),
            Event::CData(data) => top.text.push_str(&data.xml10_content()),
            Event::GeneralRef(reference) => top.text.push_str(&reference_text(&reference)?),
            Event::End(_) => {
                let closed = open.pop();
                match (open.last_mut(), closed) {
                    (Some(parent), Some(closed)) => parent.children.push(closed),
                    _ => return Err("unbalanced elements".to_owned()),
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    match (open.pop(), open.is_empty()) {
        (Some(document), true) => Ok(document),
        _ => Err("the file ends before its elements close".to_owned()),
    }
}

/// What a document says, plus whether its `xmp:Label` is free text outside the five colours.
#[derive(Debug, Default)]
struct Parsed {
    data: XmpData,
    has_foreign_label: bool,
    subjects: Vec<String>,
    hierarchies: Vec<String>,
}

impl Parsed {
    fn apply(&mut self, property: Property, values: &[String]) {
        let first = values.first().map_or("", |value| value.trim());
        match property {
            Property::Rating => self.apply_rating(first),
            Property::Label => match Label::from_name(first) {
                Some(label) => self.data.label = Some(label),
                None => self.has_foreign_label |= !first.is_empty(),
            },
            Property::Flag => match first {
                "pick" => self.data.flag = Some(Flag::Pick),
                "reject" => self.data.flag = Some(Flag::Reject),
                _ => {}
            },
            Property::Subject => self.subjects.extend_from_slice(values),
            Property::Hierarchy => self.hierarchies.extend_from_slice(values),
            Property::Recipe => self.data.recipe = serde_json::from_str(first).ok(),
        }
    }

    fn apply_rating(&mut self, text: &str) {
        let Ok(rating) = text.parse::<f32>() else { return };
        if rating == REJECTED_RATING {
            self.data.rating = Some(0);
            self.data.flag.get_or_insert(Flag::Reject);
        } else if (0.0..=5.0).contains(&rating) {
            self.data.rating = Some(rating.round() as u8);
        }
    }

    /// `dc:subject` is flat and usually repeats every level of `lr:hierarchicalSubject`, so a
    /// flat keyword only counts when no hierarchy already mentions it.
    fn combine_keywords(&mut self) {
        let mut keywords: Vec<Vec<String>> = self
            .hierarchies
            .iter()
            .map(|path| {
                let levels = path.split(HIERARCHY_SEPARATOR).map(|level| level.trim().to_owned());
                levels.filter(|level| !level.is_empty()).collect::<Vec<_>>()
            })
            .filter(|path| !path.is_empty())
            .collect();
        for subject in self.subjects.iter().filter(|subject| !subject.is_empty()) {
            let already_there =
                keywords.iter().flatten().any(|level| level.eq_ignore_ascii_case(subject));
            if !already_there {
                keywords.push(vec![subject.clone()]);
            }
        }
        keywords.sort();
        keywords.dedup();
        self.data.keywords = keywords;
    }
}

fn parse_document(xml: &str) -> ParseResult<Parsed> {
    let document = read_tree(xml)?;
    let root = document.find_rdf_root().ok_or("no rdf:RDF element")?;
    let mut parsed = Parsed::default();
    for description in root.children.iter().filter(|child| child.is_rdf("Description")) {
        for attribute in &description.attributes {
            if let Some(property) = Property::identify(&attribute.namespace, &attribute.local) {
                parsed.apply(property, std::slice::from_ref(&attribute.value));
            }
        }
        for child in &description.children {
            if let Some(property) = Property::identify(&child.namespace, &child.local) {
                parsed.apply(property, &child.values());
            }
        }
    }
    parsed.combine_keywords();
    Ok(parsed)
}

fn as_text(xml: &[u8]) -> ParseResult<&str> {
    std::str::from_utf8(xml).map_err(|error| format!("not UTF-8: {error}"))
}

fn parse(xml: &[u8]) -> ParseResult<XmpData> {
    parse_document(as_text(xml)?).map(|parsed| parsed.data)
}

/// What a merge may remove from the file.
#[derive(Clone, Copy, Debug)]
struct Ownership {
    keep_foreign_label: bool,
}

impl Ownership {
    fn owns(self, property: Option<Property>) -> bool {
        match property {
            Some(Property::Label) => !self.keep_foreign_label,
            Some(_) => true,
            None => false,
        }
    }
}

fn is_blank(text: &BytesText) -> bool {
    text.trim_ascii().is_empty()
}

fn is_rdf_about(resolver: &NamespaceResolver, key: QName) -> bool {
    let (resolved, local) = resolver.resolve_attribute(key);
    namespace_of(&resolved) == RDF_NS && local.as_ref() == "about"
}

/// A value's raw text, safe to put back between double quotes.
fn quoted_value(value: &str) -> Cow<'_, str> {
    if value.contains('"') {
        Cow::Owned(value.replace('"', "&quot;"))
    } else {
        Cow::Borrowed(value)
    }
}

/// The start tag without the attributes Unlight owns, and whether it still says anything.
/// An untouched tag comes back as it was, down to its formatting.
fn strip_attributes(
    resolver: &NamespaceResolver,
    start: &BytesStart,
    ownership: Ownership,
) -> ParseResult<(BytesStart<'static>, bool)> {
    let mut stripped = BytesStart::new(start.name().as_ref().to_owned());
    let mut removed_any = false;
    let mut has_content = false;
    for attribute in start.attributes() {
        let attribute = attribute.map_err(message)?;
        let (resolved, local) = resolver.resolve_attribute(attribute.key);
        if ownership.owns(Property::identify(namespace_of(&resolved), local.as_ref())) {
            removed_any = true;
            continue;
        }
        has_content |= attribute.key.as_namespace_binding().is_none()
            && !is_rdf_about(resolver, attribute.key);
        let value = Cow::Owned(quoted_value(&attribute.value).into_owned());
        stripped.push_attribute(Attribute { key: attribute.key, value });
    }
    let kept = if removed_any { stripped } else { start.clone().into_owned() };
    Ok((kept, has_content))
}

/// Reads one top-level `rdf:Description` to its end, dropping what Unlight owns. Returns the
/// events to keep, or `None` if nothing foreign is left in it.
fn filter_description(
    reader: &mut NsReader<&[u8]>,
    start: &BytesStart,
    self_closed: bool,
    ownership: Ownership,
) -> ParseResult<Option<Vec<Event<'static>>>> {
    let (kept, mut has_content) = strip_attributes(reader.resolver(), start, ownership)?;
    if self_closed {
        return Ok(has_content.then(|| vec![Event::Empty(kept)]));
    }
    let mut events = vec![Event::Start(kept)];
    let mut depth = 0usize;
    loop {
        let (resolved, event) = reader.read_resolved_event().map_err(message)?;
        let owned = match &event {
            Event::Start(element) | Event::Empty(element) if depth == 0 => {
                let local = element.local_name();
                ownership.owns(Property::identify(namespace_of(&resolved), local.as_ref()))
            }
            _ => false,
        };
        if owned {
            if matches!(events.last(), Some(Event::Text(text)) if is_blank(text)) {
                events.pop();
            }
            if let Event::Start(element) = &event {
                reader.read_to_end(element.name()).map_err(message)?;
            }
            continue;
        }
        match &event {
            Event::Start(_) => {
                depth += 1;
                has_content = true;
            }
            Event::End(_) if depth == 0 => {
                events.push(event.into_owned());
                return Ok(has_content.then_some(events));
            }
            Event::End(_) => depth -= 1,
            Event::Text(text) if is_blank(text) => {}
            Event::Eof => return Err("the file ends inside rdf:Description".to_owned()),
            _ => has_content = true,
        }
        events.push(event.into_owned());
    }
}

fn list_block(tag: &str, list: &str, items: &[String]) -> String {
    let items: String = items
        .iter()
        .map(|item| format!("     <rdf:li>{}</rdf:li>\n", escape(item.as_str())))
        .collect();
    format!("   <{tag}>\n    <rdf:{list}>\n{items}    </rdf:{list}>\n   </{tag}>\n")
}

/// Unlight's own `rdf:Description`, from its leading newline to the newline after its close.
fn description_block(about: &str, data: &XmpData) -> ParseResult<String> {
    let namespaces =
        [("rdf", RDF_NS), ("xmp", XMP_NS), ("dc", DC_NS), ("lr", LR_NS), ("unlight", UNLIGHT_NS)];
    let mut block = format!("\n  <rdf:Description rdf:about=\"{about}\"");
    for (prefix, uri) in namespaces {
        block.push_str(&format!("\n    xmlns:{prefix}=\"{uri}\""));
    }
    if let Some(rating) = data.rating {
        block.push_str(&format!("\n    xmp:Rating=\"{rating}\""));
    }
    if let Some(name) = data.label.and_then(Label::name) {
        block.push_str(&format!("\n    xmp:Label=\"{name}\""));
    }
    match data.flag {
        Some(Flag::Pick) => block.push_str("\n    unlight:Flag=\"pick\""),
        Some(Flag::Reject) => block.push_str("\n    unlight:Flag=\"reject\""),
        _ => {}
    }
    block.push_str(">\n");

    let leaves: Vec<String> =
        data.keywords.iter().filter_map(|path| path.last().cloned()).collect();
    if !leaves.is_empty() {
        block.push_str(&list_block("dc:subject", "Bag", &leaves));
    }
    let hierarchies: Vec<String> = data
        .keywords
        .iter()
        .filter(|path| path.len() > 1)
        .map(|path| path.join(&HIERARCHY_SEPARATOR.to_string()))
        .collect();
    if !hierarchies.is_empty() {
        block.push_str(&list_block("lr:hierarchicalSubject", "Bag", &hierarchies));
    }
    if let Some(recipe) = &data.recipe {
        let json = serde_json::to_string(recipe).map_err(message)?;
        block.push_str(&format!(
            "   <unlight:Recipe>{}</unlight:Recipe>\n",
            partial_escape(json.as_str())
        ));
    }
    block.push_str("  </rdf:Description>\n ");
    Ok(block)
}

fn raw_about(resolver: &NamespaceResolver, start: &BytesStart) -> Option<String> {
    start
        .attributes()
        .flatten()
        .find(|attribute| is_rdf_about(resolver, attribute.key))
        .map(|attribute| quoted_value(&attribute.value).into_owned())
}

/// Where the stream is relative to the first `rdf:RDF` element, the only one Unlight touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RdfPosition {
    Before,
    Inside { depth: usize },
    After,
}

fn emit(writer: &mut Writer<Vec<u8>>, event: Event<'_>) -> ParseResult<()> {
    writer.write_event(event).map_err(message)
}

fn emit_held(writer: &mut Writer<Vec<u8>>, held: &mut Option<Event<'static>>) -> ParseResult<()> {
    held.take().map_or(Ok(()), |event| emit(writer, event))
}

/// Rewrites a document with `data` in place of Unlight's properties. Merging twice gives the
/// same bytes, because blank text in front of a dropped `rdf:Description` goes with it.
fn merge(existing: &[u8], data: &XmpData) -> ParseResult<Vec<u8>> {
    let xml = as_text(existing)?;
    let parsed = parse_document(xml)?;
    let ownership =
        Ownership { keep_foreign_label: data.label.is_none() && parsed.has_foreign_label };

    let mut reader = NsReader::from_str(xml);
    let mut writer = Writer::new(Vec::with_capacity(existing.len() + 4096));
    let mut position = RdfPosition::Before;
    let mut depth = 0usize;
    let mut held_space = None;
    let mut about = None;
    loop {
        let (resolved, event) = reader.read_resolved_event().map_err(message)?;
        let element = match &event {
            Event::Start(start) | Event::Empty(start) => Some(start),
            _ => None,
        };
        let is_rdf_element = |local: &str| {
            element.is_some_and(|start| {
                namespace_of(&resolved) == RDF_NS && start.local_name().as_ref() == local
            })
        };
        let is_root = is_rdf_element("RDF") && position == RdfPosition::Before;
        let inside_root = position == (RdfPosition::Inside { depth });
        let is_description = is_rdf_element("Description") && inside_root;
        let self_closed = matches!(event, Event::Empty(_));

        match event {
            Event::Eof => break,
            Event::Text(text) if inside_root && is_blank(&text) => {
                emit_held(&mut writer, &mut held_space)?;
                held_space = Some(Event::Text(text.into_owned()));
            }
            Event::Start(start) | Event::Empty(start) if is_description => {
                about = about.or_else(|| raw_about(reader.resolver(), &start));
                match filter_description(&mut reader, &start, self_closed, ownership)? {
                    Some(events) => {
                        emit_held(&mut writer, &mut held_space)?;
                        events.into_iter().try_for_each(|kept| emit(&mut writer, kept))?;
                    }
                    None => held_space = None,
                }
            }
            Event::Empty(start) if is_root => {
                let end = start.to_end().into_owned();
                emit(&mut writer, Event::Start(start))?;
                writer.get_mut().extend_from_slice(description_block("", data)?.as_bytes());
                emit(&mut writer, Event::End(end))?;
                position = RdfPosition::After;
            }
            Event::Start(start) => {
                emit_held(&mut writer, &mut held_space)?;
                emit(&mut writer, Event::Start(start))?;
                depth += 1;
                if is_root {
                    position = RdfPosition::Inside { depth };
                }
            }
            Event::End(end) => {
                if inside_root {
                    held_space = None;
                    let block = description_block(about.as_deref().unwrap_or(""), data)?;
                    writer.get_mut().extend_from_slice(block.as_bytes());
                    position = RdfPosition::After;
                }
                emit(&mut writer, Event::End(end))?;
                depth = depth.saturating_sub(1);
            }
            other => {
                emit_held(&mut writer, &mut held_space)?;
                emit(&mut writer, other)?;
            }
        }
    }
    Ok(writer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHTROOM: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 7.0">
 <!-- written by Lightroom -->
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmp:CreatorTool="Adobe Lightroom"
    xmp:Rating="4"
    xmp:Label="Green"
    crs:Exposure2012="+0.65">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>Animals</rdf:li>
     <rdf:li>Owl</rdf:li>
     <rdf:li>Travel</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>Animals|Owl</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"#;

    fn parsed(xml: &str) -> XmpData {
        parse(xml.as_bytes()).unwrap()
    }

    fn merged(xml: &str, data: &XmpData) -> String {
        String::from_utf8(merge(xml.as_bytes(), data).unwrap()).unwrap()
    }

    fn document(description_body: &str) -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF_NS}">{description_body}</rdf:RDF></x:xmpmeta>"#
        )
    }

    fn keywords(paths: &[&[&str]]) -> Vec<Vec<String>> {
        paths.iter().map(|path| path.iter().map(|level| (*level).to_owned()).collect()).collect()
    }

    #[test]
    fn reads_lightroom_rating_label_and_keywords() {
        let data = parsed(LIGHTROOM);
        assert_eq!(data.rating, Some(4));
        assert_eq!(data.label, Some(Label::Green));
        assert_eq!(data.keywords, keywords(&[&["Animals", "Owl"], &["Travel"]]));
        assert_eq!(data.recipe, None);
    }

    #[test]
    fn reads_properties_written_as_elements() {
        let data = parsed(&document(&format!(
            r#"<rdf:Description xmlns:xmp="{XMP_NS}"><xmp:Rating>2</xmp:Rating><xmp:Label>Blue</xmp:Label></rdf:Description>"#
        )));
        assert_eq!(data.rating, Some(2));
        assert_eq!(data.label, Some(Label::Blue));
    }

    #[test]
    fn namespaces_match_by_uri_not_prefix() {
        let data = parsed(&document(&format!(
            r#"<rdf:Description xmlns:ns0="{XMP_NS}" ns0:Rating="3"/>"#
        )));
        assert_eq!(data.rating, Some(3));
        let other = parsed(&document(
            r#"<rdf:Description xmlns:xmp="http://example.com/not-xmp/" xmp:Rating="3"/>"#,
        ));
        assert_eq!(other.rating, None);
    }

    #[test]
    fn properties_may_be_spread_over_descriptions() {
        let data = parsed(&document(&format!(
            r#"<rdf:Description xmlns:xmp="{XMP_NS}" xmp:Rating="1"/><rdf:Description xmlns:xmp="{XMP_NS}" xmp:Label="Red"/>"#
        )));
        assert_eq!(data.rating, Some(1));
        assert_eq!(data.label, Some(Label::Red));
    }

    #[test]
    fn rating_edge_cases() {
        let rating = |value: &str| {
            parsed(&document(&format!(
                r#"<rdf:Description xmlns:xmp="{XMP_NS}" xmp:Rating="{value}"/>"#
            )))
        };
        assert_eq!(rating("5").rating, Some(5));
        assert_eq!(rating("3.0").rating, Some(3));
        assert_eq!(rating("9").rating, None);
        assert_eq!(rating("high").rating, None);
        let rejected = rating("-1");
        assert_eq!(rejected.rating, Some(0));
        assert_eq!(rejected.flag, Some(Flag::Reject));
    }

    #[test]
    fn keyword_text_decodes_entities_and_cdata() {
        let data = parsed(&document(&format!(
            r#"<rdf:Description xmlns:dc="{DC_NS}"><dc:subject><rdf:Bag>
                <rdf:li>Rock &amp; Roll</rdf:li>
                <rdf:li>caf&#233;</rdf:li>
                <rdf:li><![CDATA[a<b]]></rdf:li>
            </rdf:Bag></dc:subject></rdf:Description>"#
        )));
        assert_eq!(data.keywords, keywords(&[&["Rock & Roll"], &["a<b"], &["café"]]));
    }

    #[test]
    fn a_plain_text_subject_still_counts() {
        let data = parsed(&document(&format!(
            r#"<rdf:Description xmlns:dc="{DC_NS}"><dc:subject>solo</dc:subject></rdf:Description>"#
        )));
        assert_eq!(data.keywords, keywords(&[&["solo"]]));
    }

    #[test]
    fn nested_descriptions_are_not_top_level_properties() {
        let data = parsed(&document(&format!(
            r#"<rdf:Description xmlns:xmp="{XMP_NS}" xmlns:s="http://example.com/s/"><s:Look><rdf:Description xmp:Rating="5"/></s:Look></rdf:Description>"#
        )));
        assert_eq!(data.rating, None);
    }

    #[test]
    fn a_description_inside_a_foreign_node_is_left_alone() {
        let nested = format!(r#"<f:Node xmlns:f="http://example.com/f/"><rdf:Description xmlns:xmp="{XMP_NS}" xmp:Rating="5"/></f:Node>"#);
        let out = merged(&document(&nested), &XmpData::default());
        assert!(out.contains(&nested), "{out}");
    }

    #[test]
    fn a_utf8_byte_order_mark_is_tolerated() {
        let xml = format!("\u{feff}{LIGHTROOM}");
        assert_eq!(parsed(&xml).rating, Some(4));
    }

    #[test]
    fn malformed_input_is_rejected() {
        let truncated = &LIGHTROOM[..LIGHTROOM.len() / 2];
        let cases: &[&[u8]] = &[
            b"",
            b"not xml at all",
            b"<x:xmpmeta",
            b"<a><b></a></b>",
            b"<plain>xml without rdf</plain>",
            &[0xff, 0xfe, 0x00, 0x3c],
            truncated.as_bytes(),
        ];
        for case in cases {
            assert!(parse(case).is_err(), "{:?}", String::from_utf8_lossy(case));
        }
        let unknown_entity = document(&format!(
            r#"<rdf:Description xmlns:dc="{DC_NS}"><dc:subject>&nope;</dc:subject></rdf:Description>"#
        ));
        assert!(parse(unknown_entity.as_bytes()).is_err());
    }

    #[test]
    fn hostile_nesting_is_rejected_without_blowing_the_stack() {
        let depth = 200_000;
        let xml = format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
        assert!(parse(xml.as_bytes()).is_err());
    }

    #[test]
    fn writing_into_nothing_makes_a_readable_sidecar() {
        let mut recipe = Recipe::default();
        recipe.basic.exposure = 1.25;
        recipe
            .masks
            .push(unlight_recipe::Mask { name: "a<b & \"c\"".into(), ..Default::default() });
        let data = XmpData {
            rating: Some(5),
            flag: Some(Flag::Pick),
            label: Some(Label::Purple),
            keywords: keywords(&[&["Places", "Rock & Roll Hall"], &["solo"]]),
            recipe: Some(recipe),
        };
        assert_eq!(parsed(&merged(EMPTY_DOCUMENT, &data)), data);
    }

    #[test]
    fn foreign_content_survives_a_merge() {
        let out = merged(LIGHTROOM, &XmpData { rating: Some(1), ..XmpData::default() });
        for foreign in [
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>"#,
            r#"x:xmptk="Adobe XMP Core 7.0""#,
            "<!-- written by Lightroom -->",
            r#"xmp:CreatorTool="Adobe Lightroom""#,
            r#"crs:Exposure2012="+0.65""#,
            "<crs:ToneCurvePV2012>\n    <rdf:Seq>\n     <rdf:li>0, 0</rdf:li>",
            r#"<?xpacket end="w"?>"#,
        ] {
            assert!(out.contains(foreign), "lost {foreign} in:\n{out}");
        }
        assert_eq!(parsed(&out).rating, Some(1));
        assert_eq!(parsed(&out).label, None);
        assert_eq!(parsed(&out).keywords, Vec::<Vec<String>>::new());
        assert!(!out.contains("Animals"));
    }

    #[test]
    fn a_description_with_nothing_of_ours_is_left_byte_identical() {
        let foreign = r#"<rdf:Description   rdf:about="x"
     xmlns:foo='http://example.com/foo/'   foo:a='1&amp;2'  foo:b="it's"/>"#;
        let out = merged(&document(foreign), &XmpData::default());
        assert!(out.contains(foreign), "{out}");
    }

    #[test]
    fn stripped_attributes_keep_the_raw_values_of_the_rest() {
        let xml = document(&format!(
            r#"<rdf:Description xmlns:xmp="{XMP_NS}" xmlns:foo="http://example.com/foo/" xmp:Rating="2" foo:a='say "hi" &amp; go'/>"#
        ));
        let out = merged(&xml, &XmpData::default());
        assert!(out.contains(r#"foo:a="say &quot;hi&quot; &amp; go""#), "{out}");
        assert_eq!(parsed(&out).rating, None);
    }

    #[test]
    fn merging_twice_gives_the_same_bytes() {
        let data = XmpData {
            rating: Some(3),
            flag: Some(Flag::Reject),
            label: Some(Label::Red),
            keywords: keywords(&[&["a", "b"]]),
            recipe: Some(Recipe::default()),
        };
        let once = merged(LIGHTROOM, &data);
        assert_eq!(merged(&once, &data), once);
        let fresh = merged(EMPTY_DOCUMENT, &data);
        assert_eq!(merged(&fresh, &data), fresh);
    }

    #[test]
    fn changed_properties_replace_old_ones_whatever_form_they_took() {
        let xml = document(&format!(
            r#"<rdf:Description xmlns:xmp="{XMP_NS}"><xmp:Rating>2</xmp:Rating><xmp:Keep>yes</xmp:Keep></rdf:Description>"#
        ));
        let out = merged(&xml, &XmpData { rating: Some(4), ..XmpData::default() });
        assert_eq!(out.matches("Rating").count(), 1);
        assert!(out.contains("<xmp:Keep>yes</xmp:Keep>"));
        assert_eq!(parsed(&out).rating, Some(4));
    }

    #[test]
    fn a_label_we_do_not_know_stays_until_we_set_one() {
        let xml =
            document(&format!(r#"<rdf:Description xmlns:xmp="{XMP_NS}" xmp:Label="To review"/>"#));
        let untouched = merged(&xml, &XmpData::default());
        assert!(untouched.contains(r#"xmp:Label="To review""#));
        let replaced = merged(&xml, &XmpData { label: Some(Label::Red), ..XmpData::default() });
        assert!(!replaced.contains("To review"));
        assert_eq!(parsed(&replaced).label, Some(Label::Red));
    }

    #[test]
    fn clearing_a_known_label_removes_it() {
        let out = merged(LIGHTROOM, &XmpData::default());
        assert!(!out.contains("xmp:Label"));
    }

    #[test]
    fn our_prefixes_cannot_collide_with_a_foreign_binding() {
        let xml = document(
            r#"<rdf:Description xmlns:dc="http://example.com/other/" xmlns:xmp="http://example.com/x/" xmp:Rating="9" dc:subject="theirs"/>"#,
        );
        let data =
            XmpData { rating: Some(2), keywords: keywords(&[&["ours"]]), ..XmpData::default() };
        let out = merged(&xml, &data);
        assert!(
            out.contains(r#"xmp:Rating="9""#) && out.contains(r#"dc:subject="theirs""#),
            "{out}"
        );
        assert_eq!(parsed(&out), data);
    }

    #[test]
    fn a_differently_prefixed_rdf_root_still_gets_our_description() {
        let xml = format!(
            r#"<r:RDF xmlns:r="{RDF_NS}"><r:Description xmlns:foo="http://example.com/foo/" foo:a="1"/></r:RDF>"#
        );
        let out = merged(&xml, &XmpData { rating: Some(5), ..XmpData::default() });
        assert!(out.contains(r#"foo:a="1""#));
        assert_eq!(parsed(&out).rating, Some(5));
    }

    #[test]
    fn an_empty_rdf_element_is_opened_up() {
        let xml = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF_NS}"/></x:xmpmeta>"#
        );
        let out = merged(&xml, &XmpData { rating: Some(2), ..XmpData::default() });
        assert_eq!(parsed(&out).rating, Some(2));
    }

    #[test]
    fn the_rdf_about_value_carries_over() {
        let xml = document(&format!(
            r#"<rdf:Description rdf:about="uuid:abc" xmlns:xmp="{XMP_NS}" xmp:Rating="1"/>"#
        ));
        let out = merged(&xml, &XmpData { rating: Some(2), ..XmpData::default() });
        assert!(out.contains(r#"rdf:about="uuid:abc""#), "{out}");
    }

    #[test]
    fn a_broken_recipe_is_dropped_but_the_rest_still_reads() {
        let data = parsed(&document(&format!(
            r#"<rdf:Description xmlns:xmp="{XMP_NS}" xmlns:unlight="{UNLIGHT_NS}" xmp:Rating="2"><unlight:Recipe>not json</unlight:Recipe></rdf:Description>"#
        )));
        assert_eq!(data.rating, Some(2));
        assert_eq!(data.recipe, None);
    }

    #[test]
    fn malformed_files_are_never_merged_over() {
        assert!(merge(b"<not-closed>", &XmpData::default()).is_err());
    }

    #[test]
    fn sidecar_path_prefers_what_exists() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("IMG_1.ARW");
        assert_eq!(sidecar_path(&raw), dir.path().join("IMG_1.xmp"));
        let appended = dir.path().join("IMG_1.ARW.xmp");
        fs::write(&appended, "").unwrap();
        assert_eq!(sidecar_path(&raw), appended);
        let paired = dir.path().join("IMG_1.xmp");
        fs::write(&paired, "").unwrap();
        assert_eq!(sidecar_path(&raw), paired);
    }

    #[test]
    fn writing_a_sidecar_leaves_a_malformed_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        fs::write(&path, "<broken").unwrap();
        let error = write_sidecar(&path, &XmpData::default()).unwrap_err();
        assert!(matches!(error, Error::MalformedXmp { .. }));
        assert_eq!(fs::read_to_string(&path).unwrap(), "<broken");
    }

    #[test]
    fn a_sidecar_round_trips_through_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        let data = XmpData { rating: Some(3), ..XmpData::default() };
        write_sidecar(&path, &data).unwrap();
        assert_eq!(read_sidecar(&path).unwrap(), data);
        write_sidecar(&path, &XmpData { rating: Some(4), ..data }).unwrap();
        assert_eq!(read_sidecar(&path).unwrap().rating, Some(4));
    }

    #[test]
    fn reading_a_missing_sidecar_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(read_sidecar(&dir.path().join("none.xmp")), Err(Error::Io { .. })));
    }
}
