use crate::{Error, ImageMetadata, Result};

/// Leaves room in the usual 255-byte limit for a collision suffix and an extension.
const MAX_STEM_BYTES: usize = 220;
const FALLBACK_STEM: &str = "untitled";

const RESERVED_ON_WINDOWS: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

enum Part<'a> {
    Literal(&'a str),
    Name,
    Date,
    Sequence,
    Camera,
    Rating,
}

fn parse(template: &str) -> Result<Vec<Part<'_>>> {
    let mut parts = Vec::new();
    let mut rest = template;
    while !rest.is_empty() {
        let brace = rest.find(['{', '}']).unwrap_or(rest.len());
        if brace > 0 {
            parts.push(Part::Literal(&rest[..brace]));
            rest = &rest[brace..];
            continue;
        }
        let close = rest
            .find('}')
            .filter(|_| rest.starts_with('{'))
            .ok_or_else(|| Error::Template(format!("unbalanced brace in `{template}`")))?;
        parts.push(match &rest[1..close] {
            "name" => Part::Name,
            "date" => Part::Date,
            "seq" => Part::Sequence,
            "camera" => Part::Camera,
            "rating" => Part::Rating,
            other => return Err(Error::Template(format!("unknown token `{{{other}}}`"))),
        });
        rest = &rest[close + 1..];
    }
    Ok(parts)
}

pub(crate) fn validate(template: &str) -> Result<()> {
    parse(template).map(drop)
}

fn is_forbidden(c: char) -> bool {
    let separators_or_shell_magic =
        matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
    let bidi_controls =
        matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    c.is_control() || separators_or_shell_magic || bidi_controls
}

fn truncate_to_boundary(text: &mut String, max_bytes: usize) {
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

/// Reduces any text to one safe path component, which rules out traversal entirely.
fn sanitize(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| if is_forbidden(c) { '_' } else { c })
        .collect();
    let mut stem = replaced.trim_matches(['.', ' ']).to_owned();
    truncate_to_boundary(&mut stem, MAX_STEM_BYTES);
    stem.truncate(stem.trim_end_matches(['.', ' ']).len());

    let device_name = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if stem.is_empty() {
        FALLBACK_STEM.to_owned()
    } else if RESERVED_ON_WINDOWS.contains(&device_name.as_str()) {
        format!("_{stem}")
    } else {
        stem
    }
}

/// The file name without its extension.
pub(crate) fn render(template: &str, metadata: &ImageMetadata) -> Result<String> {
    let mut stem = String::new();
    for part in parse(template)? {
        match part {
            Part::Literal(text) => stem.push_str(text),
            Part::Name => stem.push_str(&metadata.source_name),
            Part::Date => match &metadata.capture_time {
                Some(time) => stem.push_str(&time.date()),
                None => stem.push_str("undated"),
            },
            Part::Sequence => stem.push_str(&format!("{:04}", metadata.sequence)),
            Part::Camera => {
                let camera = metadata
                    .camera_model
                    .as_ref()
                    .or(metadata.camera_make.as_ref());
                stem.push_str(camera.map_or("unknown-camera", String::as_str));
            }
            Part::Rating => stem.push_str(&metadata.rating.unwrap_or(0).to_string()),
        }
    }
    Ok(sanitize(&stem))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptureTime;
    use std::path::{Component, Path};

    fn metadata() -> ImageMetadata {
        ImageMetadata {
            source_name: "DSC0001".into(),
            sequence: 42,
            rating: Some(4),
            camera_model: Some("ILCE-7RM5".into()),
            capture_time: Some(CaptureTime {
                year: 2026,
                month: 3,
                day: 14,
                hour: 9,
                minute: 26,
                second: 53,
                utc_offset_minutes: None,
            }),
            ..Default::default()
        }
    }

    fn with_name(name: &str) -> ImageMetadata {
        ImageMetadata {
            source_name: name.into(),
            ..metadata()
        }
    }

    fn assert_single_component(stem: &str) {
        let mut components = Path::new(stem).components();
        assert!(
            matches!(components.next(), Some(Component::Normal(_))),
            "`{stem}`"
        );
        assert!(
            components.next().is_none(),
            "`{stem}` has more than one component"
        );
    }

    #[test]
    fn every_token_expands() {
        let stem = render("{date}_{camera}_{name}_{seq}_{rating}", &metadata()).unwrap();
        assert_eq!(stem, "2026-03-14_ILCE-7RM5_DSC0001_0042_4");
    }

    #[test]
    fn literals_pass_through() {
        assert_eq!(
            render("export {name} final", &metadata()).unwrap(),
            "export DSC0001 final"
        );
    }

    #[test]
    fn missing_values_have_stand_ins() {
        let empty = ImageMetadata::default();
        assert_eq!(
            render("{date}_{camera}_{rating}", &empty).unwrap(),
            "undated_unknown-camera_0"
        );
    }

    #[test]
    fn camera_falls_back_to_make() {
        let make_only = ImageMetadata {
            camera_model: None,
            camera_make: Some("Fuji".into()),
            ..metadata()
        };
        assert_eq!(render("{camera}", &make_only).unwrap(), "Fuji");
    }

    #[test]
    fn long_sequences_are_not_cut() {
        let big = ImageMetadata {
            sequence: 1_234_567,
            ..metadata()
        };
        assert_eq!(render("{seq}", &big).unwrap(), "1234567");
    }

    #[test]
    fn unknown_tokens_are_refused() {
        assert!(matches!(
            render("{nope}", &metadata()),
            Err(Error::Template(_))
        ));
        assert!(validate("{name}_{Name}").is_err());
    }

    #[test]
    fn unbalanced_braces_are_refused() {
        for template in ["{name", "name}", "{", "}", "{na{me}", "a}{name}"] {
            assert!(validate(template).is_err(), "`{template}` passed");
        }
    }

    #[test]
    fn traversal_in_metadata_stays_in_one_component() {
        for hostile in [
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "/etc/passwd",
            "..",
            ".",
            "../",
            "a/../../b",
            "./.././..",
        ] {
            let stem = render("{name}", &with_name(hostile)).unwrap();
            assert_single_component(&stem);
            assert!(!stem.contains('/') && !stem.contains('\\'), "`{stem}`");
            assert!(!stem.starts_with('.'), "`{stem}`");
        }
    }

    #[test]
    fn traversal_in_the_template_itself_is_neutralised() {
        for template in ["../{name}", "/tmp/{name}", "{name}/../..", "..", "../.."] {
            assert_single_component(&render(template, &metadata()).unwrap());
        }
    }

    #[test]
    fn traversal_from_camera_metadata_is_neutralised() {
        let hostile = ImageMetadata {
            camera_model: Some("../../.ssh/authorized_keys".into()),
            ..metadata()
        };
        let stem = render("{camera}", &hostile).unwrap();
        assert_single_component(&stem);
        assert_eq!(stem, "_.._.ssh_authorized_keys");
    }

    #[test]
    fn forbidden_characters_become_underscores() {
        assert_eq!(
            render("{name}", &with_name("a:b*c?d\"e<f>g|h")).unwrap(),
            "a_b_c_d_e_f_g_h"
        );
    }

    #[test]
    fn control_and_nul_bytes_are_removed() {
        assert_eq!(
            render("{name}", &with_name("a\0b\nc\r\td\u{1b}[0m")).unwrap(),
            "a_b_c__d_[0m"
        );
    }

    #[test]
    fn bidi_overrides_cannot_disguise_the_name() {
        assert_eq!(
            render("{name}", &with_name("photo\u{202E}gpj.exe")).unwrap(),
            "photo_gpj.exe"
        );
    }

    #[test]
    fn leading_dots_cannot_hide_a_file() {
        assert_eq!(render("{name}", &with_name(".bashrc")).unwrap(), "bashrc");
    }

    #[test]
    fn trailing_dots_and_spaces_are_trimmed() {
        assert_eq!(render("{name}", &with_name("photo. . ")).unwrap(), "photo");
    }

    #[test]
    fn empty_results_get_a_fallback() {
        for name in ["", " ", "...", ". . ."] {
            assert_eq!(
                render("{name}", &with_name(name)).unwrap(),
                FALLBACK_STEM,
                "`{name}`"
            );
        }
    }

    #[test]
    fn windows_device_names_are_defused() {
        assert_eq!(render("{name}", &with_name("CON")).unwrap(), "_CON");
        assert_eq!(render("{name}", &with_name("nul.txt")).unwrap(), "_nul.txt");
        assert_eq!(render("{name}", &with_name("COM7")).unwrap(), "_COM7");
        assert_eq!(render("{name}", &with_name("CONTEXT")).unwrap(), "CONTEXT");
    }

    #[test]
    fn long_names_are_cut_on_a_character_boundary() {
        let stem = render("{name}", &with_name(&"é".repeat(500))).unwrap();
        assert!(stem.len() <= MAX_STEM_BYTES);
        assert!(stem.chars().all(|c| c == 'é'));
    }

    #[test]
    fn unicode_is_kept() {
        assert_eq!(
            render("{name}", &with_name("写真 été 📷")).unwrap(),
            "写真 été 📷"
        );
    }

    #[test]
    fn cutting_cannot_leave_a_trailing_dot() {
        let name = format!("{}.tail", "a".repeat(MAX_STEM_BYTES - 1));
        let stem = render("{name}", &with_name(&name)).unwrap();
        assert!(!stem.ends_with('.'), "`{stem}`");
    }
}
