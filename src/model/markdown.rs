//! A small Markdown parser for live preview.
//!
//! # What this is for
//!
//! Notes render as formatted text when you are not editing them, and reveal
//! their markup when you click in. That is one `GtkTextView` throughout — not
//! two widgets swapped — so the cursor lands exactly where you clicked and the
//! scroll position never jumps. The view achieves it by applying tags for
//! style and marking the syntax characters with a tag whose `invisible`
//! property is toggled on focus.
//!
//! So this parser has an unusual requirement: as well as *what* is styled, it
//! must report exactly **which characters are syntax**, so they can be hidden.
//! A general Markdown library gives you the former and not the latter, which is
//! why this is hand-written rather than `pulldown-cmark`.
//!
//! # Offsets
//!
//! Everything is in **character** offsets, because that is what
//! `GtkTextBuffer::iter_at_offset` takes. Byte offsets would silently corrupt
//! any note containing an accent or an emoji.
//!
//! # Scope
//!
//! Deliberately a subset — what people actually write on a sticky note. It is
//! not CommonMark and does not try to be: no reference links, no nested
//! emphasis, no tables, no setext headings. Anything unrecognised is left as
//! plain text rather than half-styled.

/// A styled region of the note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// `# ` … `###### `, level 1–6.
    Heading(u8),
    Bold,
    Italic,
    Strikethrough,
    /// `` `inline` ``
    Code,
    /// A line inside a ``` fence.
    CodeBlock,
    /// `> quoted`
    Quote,
    /// The content of a `- ` or `1. ` item, indented. The level is the nesting
    /// depth, 0 for a top-level item.
    ListItem(u8),
    /// The visible text of `[text](url)`.
    Link,
}

/// A run of characters to style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Inclusive character offset of the first character.
    pub start: usize,
    /// Exclusive character offset of the end.
    pub end: usize,
    pub style: Style,
}

/// A run of characters that is syntax rather than content, and is hidden while
/// the note is not being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub spans: Vec<Span>,
    pub markers: Vec<Marker>,
}

impl Parsed {
    fn push_span(&mut self, start: usize, end: usize, style: Style) {
        if end > start {
            self.spans.push(Span { start, end, style });
        }
    }

    fn push_marker(&mut self, start: usize, end: usize) {
        if end > start {
            self.markers.push(Marker { start, end });
        }
    }
}

/// Parse `text` into styled spans and hideable syntax markers.
pub fn parse(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let chars: Vec<char> = text.chars().collect();

    let mut line_start = 0usize;
    let mut in_fence = false;
    // Leading-space widths of the enclosing list levels, outermost first.
    let mut levels: Vec<usize> = Vec::new();

    while line_start <= chars.len() {
        let line_end = chars[line_start..]
            .iter()
            .position(|&c| c == '\n')
            .map(|offset| line_start + offset)
            .unwrap_or(chars.len());
        let line = &chars[line_start..line_end];

        if is_fence(line) {
            // The fence itself is syntax; the lines between are code.
            parsed.push_marker(line_start, line_end);
            in_fence = !in_fence;
        } else if in_fence {
            parsed.push_span(line_start, line_end, Style::CodeBlock);
        } else {
            parse_line(line, line_start, &mut parsed, &mut levels);
        }

        if line_end >= chars.len() {
            break;
        }
        line_start = line_end + 1;
    }

    parsed
}

/// The note's text with Markdown syntax removed.
///
/// For anywhere the note is *named* rather than shown: the window title, the
/// accessible label, the tray menu. A title reading "# Shopping" advertises the
/// file format; it should read "Shopping".
///
/// Goes further than hiding markers in the editor. List bullets stay visible
/// while editing, because a text view has no glyph to put in their place — but
/// in a title "- milk" is noise, so they are dropped here too.
pub fn strip(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let parsed = parse(text);

    let mut hidden = vec![false; chars.len()];
    for marker in &parsed.markers {
        for flag in hidden
            .iter_mut()
            .take(marker.end.min(chars.len()))
            .skip(marker.start)
        {
            *flag = true;
        }
    }

    // Bullets are content in the editor and clutter in a title.
    let mut line_start = 0usize;
    while line_start < chars.len() {
        let line_end = chars[line_start..]
            .iter()
            .position(|&c| c == '\n')
            .map(|offset| line_start + offset)
            .unwrap_or(chars.len());
        let line = &chars[line_start..line_end];
        let indent = line.iter().take_while(|c| **c == ' ').count();
        let rest = &line[indent..];
        if let Some(len) = bullet_len(rest).or_else(|| ordered_len(rest)) {
            for flag in hidden
                .iter_mut()
                .take((line_start + indent + len).min(chars.len()))
                .skip(line_start + indent)
            {
                *flag = true;
            }
        }
        line_start = line_end + 1;
    }

    chars
        .iter()
        .zip(hidden)
        .filter(|(_, hidden)| !hidden)
        .map(|(c, _)| *c)
        .collect()
}

/// ``` or ~~~ , optionally with a language after it.
fn is_fence(line: &[char]) -> bool {
    let trimmed: Vec<char> = line.iter().copied().skip_while(|c| *c == ' ').collect();
    trimmed.starts_with(&['`', '`', '`']) || trimmed.starts_with(&['~', '~', '~'])
}

fn parse_line(line: &[char], offset: usize, parsed: &mut Parsed, levels: &mut Vec<usize>) {
    if line.is_empty() {
        // A blank line separates items but does not end the list.
        return;
    }

    // ---- block prefix ----
    let indent = line.iter().take_while(|c| **c == ' ').count();
    let rest = &line[indent..];

    let is_item = bullet_len(rest).or_else(|| ordered_len(rest)).is_some();
    if !is_item && indent == 0 {
        // Anything else at the left edge ends the list; an indented line is a
        // continuation of the item above and leaves the nesting alone.
        levels.clear();
    }

    let (content_start, block_style) = if let Some(level) = heading_level(rest) {
        // "### " — the hashes and the space are syntax.
        let marker_len = level as usize + 1;
        parsed.push_marker(offset + indent, offset + indent + marker_len);
        (indent + marker_len, Some(Style::Heading(level)))
    } else if rest.starts_with(&['>']) {
        let marker_len = if rest.get(1) == Some(&' ') { 2 } else { 1 };
        parsed.push_marker(offset + indent, offset + indent + marker_len);
        (indent + marker_len, Some(Style::Quote))
    } else if let Some(marker_len) = bullet_len(rest).or_else(|| ordered_len(rest)) {
        // The bullet stays *visible*: hiding it would delete the only thing
        // that makes a list look like a list, since a text view cannot
        // substitute a nicer glyph for it. The spaces in front of it are
        // syntax, though — the level's margin does that job now, and leaving
        // them in would indent a nested item twice over.
        parsed.push_marker(offset, offset + indent);
        (
            indent + marker_len,
            Some(Style::ListItem(depth(levels, indent))),
        )
    } else {
        (indent, None)
    };

    let content_start = content_start.min(line.len());
    if let Some(style) = block_style {
        parsed.push_span(offset + content_start, offset + line.len(), style);
    }

    parse_inline(&line[content_start..], offset + content_start, parsed);
}

/// How deeply a list item at `indent` spaces is nested.
///
/// Taken from the widths already seen in this list rather than from a fixed
/// number of spaces per level, so two-space and four-space notes both nest one
/// level at a time — and a note that mixes them still nests monotonically.
fn depth(levels: &mut Vec<usize>, indent: usize) -> u8 {
    while levels.last().is_some_and(|width| *width > indent) {
        levels.pop();
    }
    if levels.last() != Some(&indent) {
        levels.push(indent);
    }
    // Past a handful of levels the margin would eat the note, so the styling
    // stops deepening; the parse is still honest about the structure.
    (levels.len() - 1).min(MAX_LIST_DEPTH as usize) as u8
}

/// Deepest nesting level with its own indent.
pub const MAX_LIST_DEPTH: u8 = 4;

/// `#` to `######` followed by a space.
fn heading_level(line: &[char]) -> Option<u8> {
    let hashes = line.iter().take_while(|c| **c == '#').count();
    if (1..=6).contains(&hashes) && line.get(hashes) == Some(&' ') {
        Some(hashes as u8)
    } else {
        None
    }
}

/// `- `, `* ` or `+ `. Not `*emphasis*`, which has no space.
fn bullet_len(line: &[char]) -> Option<usize> {
    match line.first() {
        Some('-') | Some('*') | Some('+') if line.get(1) == Some(&' ') => Some(2),
        _ => None,
    }
}

/// `1. ` / `12) `
fn ordered_len(line: &[char]) -> Option<usize> {
    let digits = line.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    match (line.get(digits), line.get(digits + 1)) {
        (Some('.'), Some(' ')) | (Some(')'), Some(' ')) => Some(digits + 2),
        _ => None,
    }
}

/// Emphasis, code spans and links within a single line's content.
///
/// Each `try_*` returns the index to resume from, always greater than `i`, so
/// the loop cannot stall. An earlier version inferred progress from the last
/// span pushed, which spun forever on some inputs and skipped characters on
/// others — structural guarantees beat inference here.
fn parse_inline(line: &[char], offset: usize, parsed: &mut Parsed) {
    let mut i = 0;
    while i < line.len() {
        if let Some(next) = try_code(line, i, offset, parsed) {
            i = next;
        } else if let Some(next) = try_link(line, i, offset, parsed) {
            i = next;
        } else if let Some(next) = try_emphasis(line, i, offset, parsed) {
            i = next;
        } else {
            i += 1;
        }
    }
}

/// `` `code` `` — wins over everything, since nothing inside it is formatting.
fn try_code(line: &[char], i: usize, offset: usize, parsed: &mut Parsed) -> Option<usize> {
    if line.get(i) != Some(&'`') {
        return None;
    }
    let close = find(line, i + 1, &['`'])?;
    parsed.push_marker(offset + i, offset + i + 1);
    parsed.push_span(offset + i + 1, offset + close, Style::Code);
    parsed.push_marker(offset + close, offset + close + 1);
    Some(close + 1)
}

/// `[label](target)` — the label is what you read, the rest is syntax.
fn try_link(line: &[char], i: usize, offset: usize, parsed: &mut Parsed) -> Option<usize> {
    let link = parse_link(line, i)?;
    parsed.push_marker(offset + i, offset + link.label_start);
    parsed.push_span(
        offset + link.label_start,
        offset + link.label_end,
        Style::Link,
    );
    parsed.push_marker(offset + link.label_end, offset + link.end);
    Some(link.end)
}

/// Bold, strikethrough and italic. Two-character delimiters are tried first, or
/// `**x**` would read as an empty italic followed by stray asterisks.
fn try_emphasis(line: &[char], i: usize, offset: usize, parsed: &mut Parsed) -> Option<usize> {
    const DELIMITERS: [(&[char], Style); 5] = [
        (&['*', '*'], Style::Bold),
        (&['_', '_'], Style::Bold),
        (&['~', '~'], Style::Strikethrough),
        (&['*'], Style::Italic),
        (&['_'], Style::Italic),
    ];

    for (delimiter, style) in DELIMITERS {
        if !line[i..].starts_with(delimiter) {
            continue;
        }
        let content_start = i + delimiter.len();
        let Some(close) = find(line, content_start, delimiter) else {
            continue;
        };
        if close == content_start {
            continue; // "****" is not emphasis.
        }
        // Delimiters must hug their content, as in Markdown proper: an opener
        // may not be followed by a space, nor a closer preceded by one. Without
        // this, prose like "a * b * c" silently turns into italics, and any
        // note using asterisks as separators reformats itself.
        let opens = line.get(content_start).is_some_and(|c| !c.is_whitespace());
        let closes = line.get(close - 1).is_some_and(|c| !c.is_whitespace());
        if !opens || !closes {
            continue;
        }
        parsed.push_marker(offset + i, offset + content_start);
        parsed.push_span(offset + content_start, offset + close, style);
        parsed.push_marker(offset + close, offset + close + delimiter.len());
        return Some(close + delimiter.len());
    }
    None
}

struct Link {
    label_start: usize,
    label_end: usize,
    end: usize,
}

/// `[label](target)` starting at `i`.
fn parse_link(line: &[char], i: usize) -> Option<Link> {
    if line.get(i) != Some(&'[') {
        return None;
    }
    let label_end = find(line, i + 1, &[']'])?;
    if line.get(label_end + 1) != Some(&'(') {
        return None;
    }
    let close = find(line, label_end + 2, &[')'])?;
    if label_end == i + 1 {
        return None; // empty label
    }
    Some(Link {
        label_start: i + 1,
        label_end,
        end: close + 1,
    })
}

/// Index of the next occurrence of `needle` at or after `from`.
fn find(line: &[char], from: usize, needle: &[char]) -> Option<usize> {
    (from..line.len().saturating_sub(needle.len() - 1))
        .find(|&index| line[index..].starts_with(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text a span covers, for readable assertions.
    fn text_of(source: &str, span: &Span) -> String {
        source
            .chars()
            .skip(span.start)
            .take(span.end - span.start)
            .collect()
    }

    fn styles(parsed: &Parsed) -> Vec<Style> {
        parsed.spans.iter().map(|s| s.style).collect()
    }

    /// What the note looks like with the markers hidden — the rendered view.
    fn rendered(source: &str) -> String {
        let parsed = parse(source);
        source
            .chars()
            .enumerate()
            .filter(|(index, _)| {
                !parsed
                    .markers
                    .iter()
                    .any(|m| *index >= m.start && *index < m.end)
            })
            .map(|(_, c)| c)
            .collect()
    }

    #[test]
    fn strip_removes_markup_for_titles() {
        assert_eq!(strip("# Heading Goes Here"), "Heading Goes Here");
        assert_eq!(strip("**bold** and *italic*"), "bold and italic");
        assert_eq!(strip("`code` here"), "code here");
        assert_eq!(strip("~~gone~~"), "gone");
        assert_eq!(strip("see [the docs](https://example.com)"), "see the docs");
        assert_eq!(strip("> quoted"), "quoted");
    }

    #[test]
    fn strip_drops_list_bullets_even_though_the_editor_keeps_them() {
        assert_eq!(strip("- milk"), "milk");
        assert_eq!(strip("* milk"), "milk");
        assert_eq!(strip("1. first"), "first");
        assert_eq!(strip("- **oat** milk\n- bread"), "oat milk\nbread");
    }

    #[test]
    fn strip_leaves_plain_text_and_partial_markup_alone() {
        for source in [
            "just a note",
            "a * b * c",
            "#hashtag",
            "**unfinished",
            "5 * 3",
        ] {
            assert_eq!(strip(source), source, "{source:?}");
        }
    }

    #[test]
    fn strip_is_multibyte_safe() {
        assert_eq!(strip("# 🎉 Héllo **wörld**"), "🎉 Héllo wörld");
    }

    #[test]
    fn plain_text_is_left_alone() {
        let parsed = parse("just a note");
        assert!(parsed.spans.is_empty());
        assert!(parsed.markers.is_empty());
        assert_eq!(rendered("just a note"), "just a note");
    }

    #[test]
    fn empty_input_does_not_panic() {
        assert_eq!(parse(""), Parsed::default());
        assert_eq!(parse("\n\n\n"), Parsed::default());
    }

    #[test]
    fn bold_hides_its_asterisks() {
        let source = "buy **oat milk** today";
        let parsed = parse(source);
        assert_eq!(styles(&parsed), vec![Style::Bold]);
        assert_eq!(text_of(source, &parsed.spans[0]), "oat milk");
        assert_eq!(rendered(source), "buy oat milk today");
    }

    #[test]
    fn italic_and_strikethrough() {
        let source = "*soon* and ~~later~~";
        let parsed = parse(source);
        assert_eq!(styles(&parsed), vec![Style::Italic, Style::Strikethrough]);
        assert_eq!(rendered(source), "soon and later");
    }

    #[test]
    fn bold_wins_over_italic_for_double_asterisks() {
        // Scanned longest-delimiter-first, or "**x**" would read as an empty
        // italic followed by stray asterisks.
        let parsed = parse("**x**");
        assert_eq!(styles(&parsed), vec![Style::Bold]);
    }

    #[test]
    fn headings_are_levelled_and_their_hashes_hidden() {
        for level in 1..=6u8 {
            let source = format!("{} Title", "#".repeat(level as usize));
            let parsed = parse(&source);
            assert_eq!(styles(&parsed), vec![Style::Heading(level)]);
            assert_eq!(text_of(&source, &parsed.spans[0]), "Title");
            assert_eq!(rendered(&source), "Title");
        }
    }

    #[test]
    fn seven_hashes_is_not_a_heading() {
        let parsed = parse("####### nope");
        assert!(parsed.spans.is_empty());
        assert_eq!(rendered("####### nope"), "####### nope");
    }

    #[test]
    fn a_hash_without_a_space_is_not_a_heading() {
        // "#1 priority" and "#hashtag" are things people write on notes.
        assert!(parse("#1 priority").spans.is_empty());
        assert_eq!(rendered("#hashtag"), "#hashtag");
    }

    #[test]
    fn bullets_stay_visible_because_they_are_the_rendering() {
        let source = "- milk\n- bread";
        let parsed = parse(source);
        assert_eq!(
            styles(&parsed),
            vec![Style::ListItem(0), Style::ListItem(0)]
        );
        assert_eq!(text_of(source, &parsed.spans[0]), "milk");
        // Hiding "- " would leave an unexplained indent, so it is kept.
        assert_eq!(rendered(source), source);
    }

    #[test]
    fn an_asterisk_bullet_is_not_italic() {
        let parsed = parse("* milk");
        assert_eq!(styles(&parsed), vec![Style::ListItem(0)]);
    }

    #[test]
    fn numbered_lists_are_recognised() {
        assert_eq!(styles(&parse("1. first")), vec![Style::ListItem(0)]);
        assert_eq!(styles(&parse("12) twelfth")), vec![Style::ListItem(0)]);
        assert!(parse("1.no space").spans.is_empty());
    }

    #[test]
    fn indented_items_nest_one_level_per_step() {
        // Two-space and four-space notes are both one level per step.
        for unit in ["  ", "    "] {
            let source = format!("- milk\n{unit}- semi-skimmed\n{unit}{unit}- one pint\n- bread");
            assert_eq!(
                styles(&parse(&source)),
                vec![
                    Style::ListItem(0),
                    Style::ListItem(1),
                    Style::ListItem(2),
                    Style::ListItem(0),
                ]
            );
        }
    }

    #[test]
    fn nesting_never_goes_backwards_when_widths_are_mixed() {
        let parsed = parse("- a\n   - b\n  - c\n- d");
        assert_eq!(
            styles(&parsed),
            vec![
                Style::ListItem(0),
                Style::ListItem(1),
                // Narrower than "b" but still indented, so it is b's sibling.
                Style::ListItem(1),
                Style::ListItem(0),
            ]
        );
    }

    #[test]
    fn a_paragraph_at_the_left_edge_ends_the_list() {
        let parsed = parse("  - deep\nprose\n  - deep again");
        assert_eq!(
            styles(&parsed),
            vec![Style::ListItem(0), Style::ListItem(0)]
        );
    }

    #[test]
    fn a_blank_line_between_items_keeps_the_nesting() {
        let parsed = parse("- milk\n\n  - semi-skimmed");
        assert_eq!(
            styles(&parsed),
            vec![Style::ListItem(0), Style::ListItem(1)]
        );
    }

    #[test]
    fn nesting_stops_deepening_past_the_styled_maximum() {
        let source: String = (0..8)
            .map(|level| format!("{}- item\n", "  ".repeat(level)))
            .collect();
        let deepest = styles(&parse(&source))
            .into_iter()
            .filter_map(|style| match style {
                Style::ListItem(level) => Some(level),
                _ => None,
            })
            .max();
        assert_eq!(deepest, Some(MAX_LIST_DEPTH));
    }

    #[test]
    fn quotes_hide_their_marker() {
        let source = "> to be fair";
        let parsed = parse(source);
        assert_eq!(styles(&parsed), vec![Style::Quote]);
        assert_eq!(rendered(source), "to be fair");
    }

    #[test]
    fn inline_code_hides_its_backticks() {
        let source = "run `cargo test` first";
        let parsed = parse(source);
        assert_eq!(styles(&parsed), vec![Style::Code]);
        assert_eq!(text_of(source, &parsed.spans[0]), "cargo test");
        assert_eq!(rendered(source), "run cargo test first");
    }

    #[test]
    fn formatting_inside_code_is_literal() {
        let source = "`**not bold**`";
        let parsed = parse(source);
        assert_eq!(styles(&parsed), vec![Style::Code]);
        assert_eq!(text_of(source, &parsed.spans[0]), "**not bold**");
    }

    #[test]
    fn fenced_blocks_style_their_contents_and_hide_the_fences() {
        let source = "```\nlet x = 1;\nlet y = 2;\n```";
        let parsed = parse(source);
        assert_eq!(styles(&parsed), vec![Style::CodeBlock, Style::CodeBlock]);
        assert_eq!(text_of(source, &parsed.spans[0]), "let x = 1;");
        assert_eq!(rendered(source), "\nlet x = 1;\nlet y = 2;\n");
    }

    #[test]
    fn links_show_the_label_and_hide_the_target() {
        let source = "see [the docs](https://example.com) later";
        let parsed = parse(source);
        assert_eq!(styles(&parsed), vec![Style::Link]);
        assert_eq!(text_of(source, &parsed.spans[0]), "the docs");
        assert_eq!(rendered(source), "see the docs later");
    }

    #[test]
    fn unmatched_markers_are_left_as_plain_text() {
        // Half-typed formatting is the normal state while writing, and must
        // never swallow the rest of the note.
        for source in [
            "**unfinished",
            "a * b * c *",
            "`unclosed",
            "[label](unclosed",
            "[label] (spaced)",
            "a * b * c *",
            "5 * 3 * 2",
        ] {
            let parsed = parse(source);
            assert_eq!(
                rendered(source),
                source,
                "{source:?} hid characters it should not have"
            );
            assert!(
                parsed.spans.iter().all(|s| s.end <= source.chars().count()),
                "{source:?} produced an out-of-range span"
            );
        }
    }

    #[test]
    fn asterisks_used_as_punctuation_do_not_italicise() {
        // Real note content: separators, arithmetic, footnote marks.
        for source in ["a * b * c", "5 * 3 * 2 = 30", "note *", "* "] {
            assert_eq!(rendered(source), source, "{source:?} was reformatted");
            assert!(parse(source).spans.is_empty(), "{source:?} was styled");
        }
        // But real emphasis still works either side of them.
        assert_eq!(rendered("2 * 3 and *this*"), "2 * 3 and this");
    }

    #[test]
    fn an_unclosed_fence_does_not_swallow_the_note() {
        // Typing "```" mid-note is normal; the rest must stay readable.
        let source = "```\nstill visible";
        assert_eq!(rendered(source), "\nstill visible");
        assert_eq!(styles(&parse(source)), vec![Style::CodeBlock]);
    }

    #[test]
    fn offsets_are_characters_not_bytes() {
        // The bug this guards: byte offsets would land mid-codepoint and either
        // panic in GtkTextBuffer or style the wrong characters.
        let source = "héllo **wörld** 🎉";
        let parsed = parse(source);
        assert_eq!(text_of(source, &parsed.spans[0]), "wörld");
        assert_eq!(rendered(source), "héllo wörld 🎉");

        let emoji = "🎉🎉 **b** 🎉";
        assert_eq!(text_of(emoji, &parse(emoji).spans[0]), "b");
    }

    #[test]
    fn every_span_and_marker_is_within_the_text() {
        let source =
            "# Title\n\n- **bold** item\n- `code` and [link](u)\n\n> quote\n\n```\nfn x() {}\n```";
        let parsed = parse(source);
        let len = source.chars().count();
        for span in &parsed.spans {
            assert!(span.start < span.end && span.end <= len, "{span:?}");
        }
        for marker in &parsed.markers {
            assert!(marker.start < marker.end && marker.end <= len, "{marker:?}");
        }
    }

    #[test]
    fn markers_never_overlap_each_other() {
        // Overlapping hidden ranges would double-count and could hide content.
        let source = "# **T** and `c` and [l](u)\n- *i*";
        let mut markers = parse(source).markers;
        markers.sort_by_key(|m| m.start);
        for pair in markers.windows(2) {
            assert!(
                pair[0].end <= pair[1].start,
                "markers overlap: {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn a_realistic_note_renders_as_expected() {
        let source = "# Shopping\n\n- **oat** milk\n- bread\n\n> before 6pm";
        assert_eq!(
            rendered(source),
            "Shopping\n\n- oat milk\n- bread\n\nbefore 6pm"
        );
    }

    #[test]
    fn parsing_is_stable_under_incremental_typing() {
        // Every prefix of a note gets typed at some point; none may panic.
        let source = "# T\n- **b** `c` [l](u) ~~s~~ *i*\n```\nx\n```";
        for length in 0..=source.chars().count() {
            let prefix: String = source.chars().take(length).collect();
            let parsed = parse(&prefix);
            let len = prefix.chars().count();
            assert!(parsed.spans.iter().all(|s| s.end <= len), "{prefix:?}");
            assert!(parsed.markers.iter().all(|m| m.end <= len), "{prefix:?}");
        }
    }
}
