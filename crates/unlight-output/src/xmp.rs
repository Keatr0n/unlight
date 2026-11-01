use crate::metadata::{ImageMetadata, MetadataPolicy};

const HEADER: &str = "<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
<rdf:Description rdf:about=\"\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" \
xmlns:xmpRights=\"http://ns.adobe.com/xap/1.0/rights/\">\n";
const FOOTER: &str = "</rdf:Description>\n</rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"r\"?>";

fn is_valid_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().filter(|c| is_valid_xml_char(*c)) {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn copyright_properties(metadata: &ImageMetadata) -> String {
    let mut body = String::new();
    if let Some(creator) = &metadata.creator {
        body.push_str(&format!(
            "<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>\n",
            escaped(creator)
        ));
    }
    if let Some(copyright) = &metadata.copyright {
        body.push_str(&format!(
            "<dc:rights><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:rights>\n",
            escaped(copyright)
        ));
        body.push_str("<xmpRights:Marked>True</xmpRights:Marked>\n");
    }
    body
}

fn descriptive_properties(metadata: &ImageMetadata) -> String {
    let mut body = String::new();
    if let Some(rating) = metadata.rating {
        body.push_str(&format!("<xmp:Rating>{}</xmp:Rating>\n", rating.min(5)));
    }
    if let Some(time) = &metadata.capture_time {
        body.push_str(&format!(
            "<xmp:CreateDate>{}</xmp:CreateDate>\n",
            time.iso_8601()
        ));
    }
    body
}

pub(crate) fn packet(metadata: &ImageMetadata, policy: MetadataPolicy) -> Option<String> {
    let body = match policy {
        MetadataPolicy::None => return None,
        MetadataPolicy::CopyrightOnly => copyright_properties(metadata),
        MetadataPolicy::All => copyright_properties(metadata) + &descriptive_properties(metadata),
    };
    (!body.is_empty()).then(|| format!("{HEADER}{body}{FOOTER}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::CaptureTime;

    fn sample() -> ImageMetadata {
        ImageMetadata {
            creator: Some("Jane <Doe> & Sons".into()),
            copyright: Some("© 2026 \"Jane\"".into()),
            rating: Some(4),
            capture_time: Some(CaptureTime::from_unix_seconds(0)),
            ..Default::default()
        }
    }

    #[test]
    fn writes_escaped_copyright_and_creator() {
        let packet = packet(&sample(), MetadataPolicy::All).unwrap();
        assert!(
            packet.contains("<rdf:li>Jane &lt;Doe&gt; &amp; Sons</rdf:li>"),
            "{packet}"
        );
        assert!(packet.contains("© 2026 &quot;Jane&quot;"), "{packet}");
        assert!(packet.contains("<xmpRights:Marked>True</xmpRights:Marked>"));
    }

    #[test]
    fn all_adds_rating_and_date() {
        let packet = packet(&sample(), MetadataPolicy::All).unwrap();
        assert!(packet.contains("<xmp:Rating>4</xmp:Rating>"));
        assert!(packet.contains("<xmp:CreateDate>1970-01-01T00:00:00</xmp:CreateDate>"));
    }

    #[test]
    fn copyright_only_leaves_out_rating_and_date() {
        let packet = packet(&sample(), MetadataPolicy::CopyrightOnly).unwrap();
        assert!(!packet.contains("Rating"));
        assert!(!packet.contains("CreateDate"));
        assert!(packet.contains("dc:rights"));
    }

    #[test]
    fn nothing_to_say_means_no_packet() {
        assert_eq!(packet(&ImageMetadata::default(), MetadataPolicy::All), None);
        assert_eq!(packet(&sample(), MetadataPolicy::None), None);
    }

    #[test]
    fn characters_xml_forbids_are_dropped() {
        assert_eq!(escaped("a\u{0}b\u{1F}c\u{FFFE}d"), "abcd");
    }

    #[test]
    fn markup_in_a_field_cannot_escape_it() {
        let hostile = ImageMetadata {
            copyright: Some("</rdf:li></rdf:Alt><evil/>".into()),
            ..Default::default()
        };
        let packet = packet(&hostile, MetadataPolicy::All).unwrap();
        assert!(!packet.contains("<evil/>"));
    }
}
