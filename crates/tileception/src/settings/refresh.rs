//! config.toml changed on disk while Settings is open. What the person has not
//! touched follows the file; their edits stay, with a notice when the file
//! changed the same setting.
use super::*;
use crate::text_input::TextInput;

/// Takes the values now on disk into the draft. A field the draft leaves as
/// it was takes the new value; an edited field keeps the edit. Returns the
/// edited fields that the file changed to something else too.
fn merge_fields(
    before: &[Option<toml::Value>],
    draft: &mut [Option<toml::Value>],
    disk: &[Option<toml::Value>],
) -> Vec<usize> {
    let mut kept = Vec::new();
    for (index, ((old, value), new)) in before.iter().zip(draft.iter_mut()).zip(disk).enumerate() {
        if value == old {
            value.clone_from(new);
        } else if old != new && value != new {
            kept.push(index);
        }
    }
    kept
}

/// Moves a text draft onto a new source: untouched text becomes the source,
/// edited text stays. True when edits stay over a change in the source.
pub(super) fn merge_text(original: &mut String, text: &mut String, source: &str) -> bool {
    if original == source {
        return false;
    }
    let edited = text != original;
    if !edited {
        source.clone_into(text);
    }
    source.clone_into(original);
    edited
}

/// Moves a staged whole-file draft onto a new source. A draft that changes
/// only whole settings (`keys`) is rebuilt from the source with those settings
/// as the draft has them; one with other edits, such as a comment or a table
/// Settings has no row for, cannot be carried over and gives None.
pub(super) fn rebase_document<'a>(
    original: &str,
    draft: &str,
    source: &str,
    keys: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let parsed: toml_edit::DocumentMut = draft.parse().ok()?;
    let mut edits = Vec::new();
    for key in keys {
        let excerpt = crate::config_edit::section_text(draft, key).ok()?;
        if excerpt == crate::config_edit::section_text(original, key).ok()? {
            continue;
        }
        let present = match key.split_once('.') {
            Some((section, name)) => parsed.get(section).and_then(|item| item.get(name)).is_some(),
            None => parsed.get(key).is_some(),
        };
        edits.push((key, present.then_some(excerpt)));
    }
    let overlay = |base: &str| {
        edits.iter().try_fold(base.to_string(), |text, (key, excerpt)| match excerpt {
            Some(excerpt) => crate::config_edit::splice_section(&text, key, excerpt),
            None => crate::config_edit::draft_text(&text, &[(key, None)]),
        })
    };
    // Only when the settings alone make the draft is nothing else lost.
    if overlay(original).ok()? != draft {
        return None;
    }
    overlay(source).ok()
}

/// Moves the whole-file draft and the text it was made from onto a new file
/// text. Apply checks the file against that base, so a staged draft that
/// cannot move keeps the old one: Apply then refuses instead of writing over
/// the change. False in that case.
pub(super) fn merge_document(original: &mut String, draft: &mut String, source: &str) -> bool {
    if draft == original {
        merge_text(original, draft, source);
        return true;
    }
    if original == source {
        return true;
    }
    match rebase_document(original, draft, source, FIELDS.iter().map(|field| field.key)) {
        Some(document) => {
            *draft = document;
            source.clone_into(original);
            true
        }
        None => false,
    }
}

/// Whether two TOML texts hold the same values, however they are laid out.
pub(super) fn same_toml(left: &str, right: &str) -> bool {
    left == right
        || left.parse::<toml::Table>().is_ok_and(|left| right.parse::<toml::Table>().is_ok_and(|right| left == right))
}

/// Replaces the text of a field the person has not edited, keeping the caret
/// where it was as far as the new text allows.
pub(super) fn replace_input(input: &mut TextInput, text: String) {
    let mut cursor = input.cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    *input = TextInput { text, cursor, anchor: None };
}

/// "A", "A and B", "A, B and C".
fn listing(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [only] => (*only).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

impl Tileception {
    /// The compositor read config.toml again; `text` is what it read. Edits
    /// Settings made itself are already in its snapshot, so they change
    /// nothing here.
    pub(crate) fn settings_config_changed(&mut self, text: &str) {
        if !self.settings.open || self.settings.before.is_empty() {
            return;
        }
        // A file that does not parse leaves everything as it is; Apply
        // reports the problem.
        let Ok(disk) = crate::config::Config::parse(text).and_then(|config| values(&config.file)) else { return };
        if text == self.settings.document_before && disk == self.settings.before {
            return;
        }
        let settings = &mut self.settings;
        let typed = settings.editing.map(|index| {
            let shown = settings.draft[index].as_ref().and_then(toml::Value::as_str).unwrap_or("");
            (index, settings.input.text != shown)
        });
        let edges = sections::edges();
        if settings.edges_before.as_ref() == Some(&settings.before[edges]) {
            settings.edges_before = Some(disk[edges].clone());
        }
        let mut kept: Vec<&'static str> = merge_fields(&settings.before, &mut settings.draft, &disk)
            .into_iter()
            .map(|index| FIELDS[index].label)
            .collect();
        if let Some((index, typed)) = typed
            && settings.before[index] != disk[index]
        {
            if typed {
                kept.push(FIELDS[index].label);
            } else {
                let shown = settings.draft[index].as_ref().and_then(toml::Value::as_str).unwrap_or("").to_string();
                replace_input(&mut settings.input, shown);
                settings.editor_epoch = settings.editor_epoch.wrapping_add(1);
            }
        }
        let reloaded = settings.before != disk;
        settings.before = disk;
        let stale = !merge_document(&mut settings.document_before, &mut settings.document_draft, text);
        kept.extend(self.sections_refreshed());
        kept.extend(self.settings_editors_refreshed());
        kept.sort_unstable();
        kept.dedup();
        self.settings.home_cloud_labels = [self.settings_home_cloud_label(true), self.settings_home_cloud_label(false)];
        if stale {
            self.settings.message = "config.toml changed on disk, and your draft of the whole file cannot take the \
                                     change, so Apply will refuse. Discard reloads the file."
                .into();
            self.settings.error = true;
        } else if !kept.is_empty() {
            self.settings.message = format!(
                "config.toml changed on disk. Your edits to {} are kept; Apply saves them over the file's.",
                listing(&kept)
            );
            self.settings.error = false;
        } else if reloaded {
            self.settings.message = "config.toml changed on disk. Settings shows its new values.".into();
            self.settings.error = false;
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.invalidate_settings_client();
        self.schedule_render();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn value(number: i64) -> Option<toml::Value> {
        Some(toml::Value::Integer(number))
    }

    #[test]
    fn untouched_fields_follow_the_file() {
        let before = [value(1), value(2), None, value(4)];
        let mut draft = before.clone();
        let disk = [value(10), value(2), value(3), None];
        assert!(merge_fields(&before, &mut draft, &disk).is_empty());
        assert_eq!(draft, disk);
    }

    #[test]
    fn edited_fields_keep_the_edit() {
        let before = [value(1), value(2), value(3), value(4)];
        // Edited and changed on disk; edited only; edited to what the file
        // now says; untouched.
        let mut draft = [value(5), value(6), value(7), value(4)];
        let disk = [value(9), value(2), value(7), value(8)];
        assert_eq!(merge_fields(&before, &mut draft, &disk), [0]);
        assert_eq!(draft, [value(5), value(6), value(7), value(8)]);
    }

    #[test]
    fn text_drafts_follow_or_stay() {
        let (mut original, mut text) = ("a = 1\n".to_string(), "a = 1\n".to_string());
        assert!(!merge_text(&mut original, &mut text, "a = 2\n"));
        assert_eq!((original.as_str(), text.as_str()), ("a = 2\n", "a = 2\n"));

        let (mut original, mut text) = ("a = 1\n".to_string(), "a = 5\n".to_string());
        assert!(merge_text(&mut original, &mut text, "a = 2\n"));
        assert_eq!((original.as_str(), text.as_str()), ("a = 2\n", "a = 5\n"));

        // An unchanged source is no conflict, whatever the draft says.
        assert!(!merge_text(&mut original, &mut text, "a = 2\n"));
        assert_eq!(text, "a = 5\n");
    }

    fn rebase(original: &str, draft: &str, source: &str) -> Option<String> {
        rebase_document(original, draft, source, FIELDS.iter().map(|field| field.key))
    }

    #[test]
    fn a_staged_document_does_not_drop_file_changes() {
        let mut before = "[theme]\nradius = 4\n".to_string();
        let mut draft = "[theme]\nradius = 4\n# note\n".to_string();
        let disk = "[theme]\nradius = 4\n\n[plugins.demo]\ncommand = \"/bin/true\"\n";
        // A comment is no setting: the draft cannot move, so Apply refuses.
        assert!(!merge_document(&mut before, &mut draft, disk));
        assert!(
            before != disk || draft.contains("plugins.demo"),
            "Apply would write a draft without the file's change"
        );
        assert_eq!(draft, "[theme]\nradius = 4\n# note\n");

        let mut before = "[theme]\nradius = 4\n".to_string();
        let mut draft = "[theme]\nradius = 9\n".to_string();
        assert!(merge_document(&mut before, &mut draft, disk));
        assert_eq!(before, disk);
        assert!(draft.contains("plugins.demo") && draft.contains("radius = 9"), "{draft}");
    }

    #[test]
    fn staged_settings_move_onto_the_new_file() {
        let before = "[theme]\nradius = 4 # corners\nglow = 8\n";
        let disk = "[theme]\nradius = 4 # corners\nglow = 8\n\n[plugins.demo]\ncommand = \"/bin/true\"\n";
        // Changed with the controls, as Settings stages a document.
        let draft = crate::config_edit::draft_text(before, &[("theme.radius", Some(toml::Value::Integer(9)))]).unwrap();
        let rebased = rebase(before, &draft, disk).unwrap();
        assert!(rebased.contains("plugins.demo"), "{rebased}");
        assert!(rebased.contains("radius = 9 # corners"), "{rebased}");
        // Reset, which removes the setting.
        let draft = crate::config_edit::draft_text(before, &[("theme.glow", None)]).unwrap();
        let rebased = rebase(before, &draft, disk).unwrap();
        assert!(rebased.contains("plugins.demo") && !rebased.contains("glow"), "{rebased}");
        // A section screen or editor splices its excerpt in.
        let draft = crate::config_edit::splice_section(before, "theme.glow", "[theme]\nglow = 12 # wide\n").unwrap();
        let rebased = rebase(before, &draft, disk).unwrap();
        assert!(rebased.contains("plugins.demo") && rebased.contains("glow = 12 # wide"), "{rebased}");
        // The file changed the same setting: the draft's value stays.
        let disk = "[theme]\nradius = 6 # corners\nglow = 8\n";
        let draft = crate::config_edit::draft_text(before, &[("theme.radius", Some(toml::Value::Integer(9)))]).unwrap();
        assert_eq!(rebase(before, &draft, disk).unwrap(), draft);
    }

    #[test]
    fn replaced_input_keeps_the_caret_on_a_character() {
        let mut input = TextInput { text: "auto".into(), cursor: 4, anchor: Some(0) };
        replace_input(&mut input, "1.25".into());
        assert_eq!((input.text.as_str(), input.cursor, input.anchor), ("1.25", 4, None));
        replace_input(&mut input, "é".into());
        assert_eq!(input.cursor, 2);
        let mut input = TextInput { text: "abc".into(), cursor: 1, anchor: None };
        replace_input(&mut input, "é".into());
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn layout_alone_is_no_change() {
        assert!(same_toml("[a]\nb = 1\n", "\n[a]\nb = 1 # one\n"));
        assert!(!same_toml("[a]\nb = 1\n", "[a]\nb = 2\n"));
        assert!(!same_toml("[a\n", "[a]\n"));
    }

    #[test]
    fn listings_read_naturally() {
        assert_eq!(listing(&["Scale"]), "Scale");
        assert_eq!(listing(&["A", "B"]), "A and B");
        assert_eq!(listing(&["A", "B", "C"]), "A, B and C");
    }
}
