//! Markdown for the composer: the source a message is written in,
//! turned into the HTML that goes on the wire, and back again.
//!
//! The dialect is the one <https://www.markdownguide.org/> documents —
//! CommonMark for the basic syntax, plus every extended item that guide
//! lists: tables, fenced code blocks, footnotes, heading IDs, definition
//! lists, strikethrough, task lists, emoji shortcodes, highlighting,
//! subscript and superscript, and bare URLs turned into links. The pieces
//! pulldown-cmark does not carry (highlight, emoji, bare URLs) are done over
//! its event stream here, so they compose with everything else instead of
//! being string surgery on the finished HTML.
//!
//! The HTML is written for *mail*, not for a browser: every rule that
//! matters rides as a `style` attribute, because a `<style>` block is the
//! first thing a webmail client throws away.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// The dialect. Deliberately without smart punctuation (it would change the
/// characters the user typed), maths (`$` is a currency sign far more often
/// than it opens a formula) and metadata blocks (a leading `---` in a
/// message is a horizontal rule, not front matter).
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
}

/// Markdown source to the HTML part of a message.
pub fn to_html(src: &str) -> String {
    let events: Vec<Event> = extend(Parser::new_ext(src, options()).collect());
    let mut html = String::with_capacity(src.len() * 3 / 2);
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    inline_styles(&html)
}

/// Escape text for an HTML attribute or body.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// The three extended items pulldown-cmark leaves to the host, applied to
/// the text runs of the stream (never inside code, never inside a link):
/// `==highlight==`, `:emoji:` shortcodes, and bare URLs and email addresses.
///
/// `==` is tracked across events, not within one, so a highlight may wrap
/// other inline markup (`==**shouting**==`) and still close.
fn extend(events: Vec<Event>) -> Vec<Event<'static>> {
    let events = coalesce(events);
    let mut out: Vec<Event<'static>> = Vec::with_capacity(events.len());
    let mut code_depth = 0usize;
    let mut link_depth = 0usize;
    let mut mark_open = false;
    for ev in events {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                code_depth += 1;
                out.push(Event::Start(Tag::CodeBlock(own_code_kind(kind))));
            }
            Event::End(TagEnd::CodeBlock) => {
                code_depth = code_depth.saturating_sub(1);
                out.push(Event::End(TagEnd::CodeBlock));
            }
            Event::Start(tag @ Tag::Link { .. }) => {
                link_depth += 1;
                out.push(Event::Start(own_tag(tag)));
            }
            Event::End(TagEnd::Link) => {
                link_depth = link_depth.saturating_sub(1);
                out.push(Event::End(TagEnd::Link));
            }
            Event::Text(t) if code_depth == 0 => {
                let text = emoji(&t);
                push_text(&mut out, &text, link_depth == 0, &mut mark_open);
            }
            other => out.push(own(other)),
        }
    }
    // An unbalanced `==` leaves the highlight open; close it rather than
    // hand a stray tag to the recipient's client.
    if mark_open {
        out.push(Event::InlineHtml("</mark>".into()));
    }
    out
}

/// Join neighbouring text events into one run. The parser splits text at
/// characters it *might* have used as a delimiter (`^` and `~` among them),
/// which would otherwise hide a `H~2~O` from the pass below by handing it
/// over in three pieces.
fn coalesce(events: Vec<Event>) -> Vec<Event<'static>> {
    let mut out: Vec<Event<'static>> = Vec::with_capacity(events.len());
    for ev in events {
        match (out.last_mut(), ev) {
            (Some(Event::Text(prev)), Event::Text(t)) => {
                let mut joined = prev.to_string();
                joined.push_str(&t);
                *prev = joined.into();
            }
            (_, ev) => out.push(own(ev)),
        }
    }
    out
}

/// Clone an event into an owned one (the parser borrows the source).
fn own(ev: Event) -> Event<'static> {
    match ev {
        Event::Text(s) => Event::Text(s.into_string().into()),
        Event::Code(s) => Event::Code(s.into_string().into()),
        Event::Html(s) => Event::Html(s.into_string().into()),
        Event::InlineHtml(s) => Event::InlineHtml(s.into_string().into()),
        Event::FootnoteReference(s) => Event::FootnoteReference(s.into_string().into()),
        Event::SoftBreak => Event::SoftBreak,
        Event::HardBreak => Event::HardBreak,
        Event::Rule => Event::Rule,
        Event::TaskListMarker(b) => Event::TaskListMarker(b),
        Event::InlineMath(s) => Event::InlineMath(s.into_string().into()),
        Event::DisplayMath(s) => Event::DisplayMath(s.into_string().into()),
        Event::Start(t) => Event::Start(own_tag(t)),
        Event::End(t) => Event::End(t),
    }
}

fn own_tag(t: Tag) -> Tag<'static> {
    use pulldown_cmark::Tag::*;
    match t {
        Paragraph => Paragraph,
        Heading { level, id, classes, attrs } => Heading {
            level,
            id: id.map(|s| s.into_string().into()),
            classes: classes.into_iter().map(|s| s.into_string().into()).collect(),
            attrs: attrs
                .into_iter()
                .map(|(k, v)| (k.into_string().into(), v.map(|v| v.into_string().into())))
                .collect(),
        },
        BlockQuote(k) => BlockQuote(k),
        CodeBlock(CodeBlockKind::Fenced(s)) => CodeBlock(CodeBlockKind::Fenced(s.into_string().into())),
        CodeBlock(CodeBlockKind::Indented) => CodeBlock(CodeBlockKind::Indented),
        HtmlBlock => HtmlBlock,
        List(n) => List(n),
        Item => Item,
        FootnoteDefinition(s) => FootnoteDefinition(s.into_string().into()),
        DefinitionList => DefinitionList,
        DefinitionListTitle => DefinitionListTitle,
        DefinitionListDefinition => DefinitionListDefinition,
        Table(a) => Table(a),
        TableHead => TableHead,
        TableRow => TableRow,
        TableCell => TableCell,
        Emphasis => Emphasis,
        Strong => Strong,
        Strikethrough => Strikethrough,
        Superscript => Superscript,
        Subscript => Subscript,
        Link { link_type, dest_url, title, id } => Link {
            link_type,
            dest_url: dest_url.into_string().into(),
            title: title.into_string().into(),
            id: id.into_string().into(),
        },
        Image { link_type, dest_url, title, id } => Image {
            link_type,
            dest_url: dest_url.into_string().into(),
            title: title.into_string().into(),
            id: id.into_string().into(),
        },
        MetadataBlock(k) => MetadataBlock(k),
    }
}

fn own_code_kind(k: CodeBlockKind) -> CodeBlockKind<'static> {
    match k {
        CodeBlockKind::Fenced(s) => CodeBlockKind::Fenced(s.into_string().into()),
        CodeBlockKind::Indented => CodeBlockKind::Indented,
    }
}

/// Split one text run into events, opening and closing `<mark>` on `==` and
/// turning bare URLs and email addresses into links. `linkable` is false
/// inside an existing link, where another `<a>` would nest.
fn push_text(out: &mut Vec<Event<'static>>, text: &str, linkable: bool, mark_open: &mut bool) {
    let mut rest = text;
    while let Some(at) = rest.find("==") {
        let (head, tail) = rest.split_at(at);
        push_scripts(out, head, linkable);
        out.push(Event::InlineHtml(
            if *mark_open {
                "</mark>"
            } else {
                // The colors are named outright: a recipient reading on a
                // dark ground would otherwise get black-on-black from a
                // `background` alone.
                "<mark style=\"background:#fff3a3;color:#000\">"
            }
            .into(),
        ));
        *mark_open = !*mark_open;
        rest = &tail[2..];
    }
    push_scripts(out, rest, linkable);
}

/// `H~2~O` and `X^2^`: the guide's own subscript and superscript examples
/// are inside a word, which is exactly where the parser declines to read
/// them (its delimiters must flank). The flanking cases are already events
/// by the time this runs, so what is left in a text run is the intraword
/// form, matched only when it is short, unbroken and unambiguous — a lone
/// `~` in a path, a `~~strikethrough~~`, or a caret with a space after it
/// all stay the characters they are.
fn push_scripts(out: &mut Vec<Event<'static>>, text: &str, linkable: bool) {
    let mut rest = text;
    while let Some((at, marker, close)) = find_script(rest) {
        push_linkified(out, &rest[..at], linkable);
        let tag = if marker == '~' { "sub" } else { "sup" };
        out.push(Event::InlineHtml(
            format!("<{tag}>{}</{tag}>", esc(&rest[at + 1..close])).into(),
        ));
        rest = &rest[close + 1..];
    }
    push_linkified(out, rest, linkable);
}

/// `(marker offset, marker, closing-marker offset)` of the first
/// subscript/superscript run in `text`.
fn find_script(text: &str) -> Option<(usize, char, usize)> {
    const MAX: usize = 32;
    let b = text.as_bytes();
    for (i, c) in text.char_indices() {
        if c != '~' && c != '^' {
            continue;
        }
        if b.get(i + 1) == Some(&(c as u8)) {
            continue; // `~~` is strikethrough
        }
        let after = &text[i + 1..];
        let Some(end) = after.find(c) else { continue };
        let inner = &after[..end];
        if inner.is_empty()
            || inner.len() > MAX
            || inner.chars().any(char::is_whitespace)
            || inner.ends_with(c)
        {
            continue;
        }
        return Some((i, c, i + 1 + end));
    }
    None
}

/// Bare `https://…`, `www.…` and `name@host` runs become links; everything
/// else stays text (and is escaped by the HTML writer).
fn push_linkified(out: &mut Vec<Event<'static>>, text: &str, linkable: bool) {
    if text.is_empty() {
        return;
    }
    if !linkable {
        out.push(Event::Text(text.to_string().into()));
        return;
    }
    let mut last = 0usize;
    for (start, end, kind) in find_urls(text) {
        if start > last {
            out.push(Event::Text(text[last..start].to_string().into()));
        }
        let shown = &text[start..end];
        let href = match kind {
            UrlKind::Web => shown.to_string(),
            UrlKind::Bare => format!("https://{shown}"),
            UrlKind::Mail => format!("mailto:{shown}"),
        };
        out.push(Event::InlineHtml(
            format!("<a href=\"{}\">{}</a>", esc(&href), esc(shown)).into(),
        ));
        last = end;
    }
    if last < text.len() {
        out.push(Event::Text(text[last..].to_string().into()));
    }
}

#[derive(Clone, Copy)]
enum UrlKind {
    /// Carries its own scheme.
    Web,
    /// `www.` with no scheme.
    Bare,
    Mail,
}

/// Byte ranges of the links in `text`. Trailing punctuation is left out, so
/// a sentence ending "see https://example.com." keeps its full stop.
fn find_urls(text: &str) -> Vec<(usize, usize, UrlKind)> {
    let b = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0usize;
    let mut last_end = 0usize;
    while i < b.len() {
        // Byte stepping: never slice mid-character.
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        // An email address: the word around an `@`. Checked before the
        // word-boundary rule below, because an address's `@` is never at
        // the start of a word.
        if b[i] == b'@' && i > 0 {
            let start = text[..i].rfind(is_break_char).map(|p| p + 1).unwrap_or(0);
            let end = i + text[i..].find(is_break_char).unwrap_or(text.len() - i);
            let end = trim_trailing(text, i, end);
            let domain = text.get(i + 1..end).unwrap_or("");
            if start >= last_end && start < i && domain.contains('.') && !domain.starts_with('.') {
                found.push((start, end, UrlKind::Mail));
                i = end;
                last_end = end;
                continue;
            }
        }
        // A URL only ever starts at a word boundary.
        if i == 0 || is_break(b[i - 1]) {
            let rest = &text[i..];
            let kind = if rest.starts_with("https://") || rest.starts_with("http://") {
                Some(UrlKind::Web)
            } else if rest.starts_with("www.") {
                Some(UrlKind::Bare)
            } else {
                None
            };
            if let Some(kind) = kind {
                let end = i + rest
                    .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
                    .unwrap_or(rest.len());
                let end = trim_trailing(text, i, end);
                if end > i {
                    found.push((i, end, kind));
                    i = end;
                    last_end = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    found
}

fn is_break(c: u8) -> bool {
    (c as char).is_whitespace() || matches!(c, b'(' | b'[' | b'<' | b'"' | b'\'')
}

fn is_break_char(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',' | ';')
}

/// Pull sentence punctuation (and an unbalanced closing bracket) back off
/// the end of a link.
fn trim_trailing(text: &str, start: usize, mut end: usize) -> usize {
    while end > start {
        let c = text[..end].chars().next_back().unwrap_or(' ');
        let drop = matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | '*' | '_')
            || (c == ')' && text[start..end].matches('(').count() < text[start..end].matches(')').count());
        if !drop {
            break;
        }
        end -= c.len_utf8();
    }
    end
}

/// `:shortcode:` to the character. The table is the set the guide's own
/// emoji page leans on plus the shortcodes that actually turn up in mail;
/// anything unknown is left exactly as typed, so a `:-)` or a Rust path
/// never changes under the user.
fn emoji(text: &str) -> String {
    if !text.contains(':') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(':') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        match tail.find(':').filter(|e| *e > 0 && *e <= 32) {
            Some(end) => {
                let name = &tail[..end];
                match EMOJI.iter().find(|(k, _)| *k == name) {
                    Some((_, ch)) => {
                        out.push_str(ch);
                        rest = &tail[end + 1..];
                    }
                    None => {
                        out.push(':');
                        rest = tail;
                    }
                }
            }
            None => {
                out.push(':');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Shortcode table (GitHub's names, which are what the guide documents).
const EMOJI: &[(&str, &str)] = &[
    ("smile", "\u{1F604}"), ("smiley", "\u{1F603}"), ("grin", "\u{1F601}"),
    ("grinning", "\u{1F600}"), ("joy", "\u{1F602}"), ("rofl", "\u{1F923}"),
    ("sweat_smile", "\u{1F605}"), ("wink", "\u{1F609}"), ("blush", "\u{1F60A}"),
    ("slightly_smiling_face", "\u{1F642}"), ("upside_down_face", "\u{1F643}"),
    ("relaxed", "\u{263A}\u{FE0F}"), ("yum", "\u{1F60B}"), ("sunglasses", "\u{1F60E}"),
    ("heart_eyes", "\u{1F60D}"), ("kissing_heart", "\u{1F618}"), ("thinking", "\u{1F914}"),
    ("neutral_face", "\u{1F610}"), ("expressionless", "\u{1F611}"), ("no_mouth", "\u{1F636}"),
    ("smirk", "\u{1F60F}"), ("unamused", "\u{1F612}"), ("roll_eyes", "\u{1F644}"),
    ("grimacing", "\u{1F62C}"), ("lying_face", "\u{1F925}"), ("relieved", "\u{1F60C}"),
    ("pensive", "\u{1F614}"), ("sleepy", "\u{1F62A}"), ("sleeping", "\u{1F634}"),
    ("mask", "\u{1F637}"), ("nauseated_face", "\u{1F922}"), ("sneezing_face", "\u{1F927}"),
    ("dizzy_face", "\u{1F635}"), ("cowboy_hat_face", "\u{1F920}"), ("confused", "\u{1F615}"),
    ("worried", "\u{1F61F}"), ("frowning_face", "\u{2639}\u{FE0F}"), ("open_mouth", "\u{1F62E}"),
    ("hushed", "\u{1F62F}"), ("astonished", "\u{1F632}"), ("flushed", "\u{1F633}"),
    ("fearful", "\u{1F628}"), ("cold_sweat", "\u{1F630}"), ("cry", "\u{1F622}"),
    ("sob", "\u{1F62D}"), ("scream", "\u{1F631}"), ("confounded", "\u{1F616}"),
    ("persevere", "\u{1F623}"), ("disappointed", "\u{1F61E}"), ("sweat", "\u{1F613}"),
    ("weary", "\u{1F629}"), ("tired_face", "\u{1F62B}"), ("triumph", "\u{1F624}"),
    ("rage", "\u{1F621}"), ("angry", "\u{1F620}"), ("innocent", "\u{1F607}"),
    ("imp", "\u{1F47F}"), ("skull", "\u{1F480}"), ("ghost", "\u{1F47B}"),
    ("alien", "\u{1F47D}"), ("robot", "\u{1F916}"), ("clown_face", "\u{1F921}"),
    ("heart", "\u{2764}\u{FE0F}"), ("orange_heart", "\u{1F9E1}"), ("yellow_heart", "\u{1F49B}"),
    ("green_heart", "\u{1F49A}"), ("blue_heart", "\u{1F499}"), ("purple_heart", "\u{1F49C}"),
    ("black_heart", "\u{1F5A4}"), ("broken_heart", "\u{1F494}"), ("sparkling_heart", "\u{1F496}"),
    ("thumbsup", "\u{1F44D}"), ("+1", "\u{1F44D}"), ("thumbsdown", "\u{1F44E}"), ("-1", "\u{1F44E}"),
    ("ok_hand", "\u{1F44C}"), ("clap", "\u{1F44F}"), ("raised_hands", "\u{1F64C}"),
    ("pray", "\u{1F64F}"), ("muscle", "\u{1F4AA}"), ("point_right", "\u{1F449}"),
    ("point_left", "\u{1F448}"), ("point_up", "\u{261D}\u{FE0F}"), ("point_down", "\u{1F447}"),
    ("wave", "\u{1F44B}"), ("handshake", "\u{1F91D}"), ("writing_hand", "\u{270D}\u{FE0F}"),
    ("eyes", "\u{1F440}"), ("brain", "\u{1F9E0}"), ("tada", "\u{1F389}"),
    ("confetti_ball", "\u{1F38A}"), ("balloon", "\u{1F388}"), ("gift", "\u{1F381}"),
    ("trophy", "\u{1F3C6}"), ("medal", "\u{1F3C5}"), ("crown", "\u{1F451}"),
    ("fire", "\u{1F525}"), ("star", "\u{2B50}"), ("star2", "\u{1F31F}"),
    ("sparkles", "\u{2728}"), ("zap", "\u{26A1}"), ("boom", "\u{1F4A5}"),
    ("rainbow", "\u{1F308}"), ("sunny", "\u{2600}\u{FE0F}"), ("cloud", "\u{2601}\u{FE0F}"),
    ("umbrella", "\u{2614}"), ("snowflake", "\u{2744}\u{FE0F}"), ("moon", "\u{1F314}"),
    ("earth_africa", "\u{1F30D}"), ("rocket", "\u{1F680}"), ("airplane", "\u{2708}\u{FE0F}"),
    ("car", "\u{1F697}"), ("bike", "\u{1F6B2}"), ("house", "\u{1F3E0}"),
    ("office", "\u{1F3E2}"), ("coffee", "\u{2615}"), ("tea", "\u{1F375}"),
    ("beer", "\u{1F37A}"), ("wine_glass", "\u{1F377}"), ("cake", "\u{1F370}"),
    ("pizza", "\u{1F355}"), ("apple", "\u{1F34E}"), ("bread", "\u{1F35E}"),
    ("dog", "\u{1F436}"), ("cat", "\u{1F431}"), ("bird", "\u{1F426}"),
    ("fish", "\u{1F41F}"), ("bug", "\u{1F41B}"), ("bee", "\u{1F41D}"),
    ("seedling", "\u{1F331}"), ("evergreen_tree", "\u{1F332}"), ("four_leaf_clover", "\u{1F340}"),
    ("rose", "\u{1F339}"), ("sunflower", "\u{1F33B}"), ("mushroom", "\u{1F344}"),
    ("email", "\u{1F4E7}"), ("envelope", "\u{2709}\u{FE0F}"), ("inbox_tray", "\u{1F4E5}"),
    ("outbox_tray", "\u{1F4E4}"), ("package", "\u{1F4E6}"), ("paperclip", "\u{1F4CE}"),
    ("pushpin", "\u{1F4CC}"), ("calendar", "\u{1F4C5}"), ("clock", "\u{1F551}"),
    ("hourglass", "\u{231B}"), ("alarm_clock", "\u{23F0}"), ("bell", "\u{1F514}"),
    ("mag", "\u{1F50D}"), ("key", "\u{1F511}"), ("lock", "\u{1F512}"),
    ("unlock", "\u{1F513}"), ("wrench", "\u{1F527}"), ("hammer", "\u{1F528}"),
    ("gear", "\u{2699}\u{FE0F}"), ("bulb", "\u{1F4A1}"), ("book", "\u{1F4D6}"),
    ("books", "\u{1F4DA}"), ("memo", "\u{1F4DD}"), ("pencil2", "\u{270F}\u{FE0F}"),
    ("clipboard", "\u{1F4CB}"), ("chart_with_upwards_trend", "\u{1F4C8}"),
    ("chart_with_downwards_trend", "\u{1F4C9}"), ("bar_chart", "\u{1F4CA}"),
    ("computer", "\u{1F4BB}"), ("iphone", "\u{1F4F1}"), ("printer", "\u{1F5A8}\u{FE0F}"),
    ("bulb_off", "\u{1F4A1}"), ("moneybag", "\u{1F4B0}"), ("credit_card", "\u{1F4B3}"),
    ("warning", "\u{26A0}\u{FE0F}"), ("no_entry", "\u{26D4}"), ("x", "\u{274C}"),
    ("white_check_mark", "\u{2705}"), ("heavy_check_mark", "\u{2714}\u{FE0F}"),
    ("ballot_box_with_check", "\u{2611}\u{FE0F}"), ("question", "\u{2753}"),
    ("exclamation", "\u{2757}"), ("bangbang", "\u{203C}\u{FE0F}"), ("100", "\u{1F4AF}"),
    ("recycle", "\u{267B}\u{FE0F}"), ("arrow_right", "\u{27A1}\u{FE0F}"),
    ("arrow_left", "\u{2B05}\u{FE0F}"), ("arrow_up", "\u{2B06}\u{FE0F}"),
    ("arrow_down", "\u{2B07}\u{FE0F}"), ("link", "\u{1F517}"), ("bookmark", "\u{1F516}"),
];

/// Every presentational rule the finished HTML needs, as `style`
/// attributes. A `<style>` block would be stripped by most webmail clients
/// before the recipient ever saw it, so the styling has to ride on the tags
/// themselves.
///
/// An attribute the writer already produced (a table cell's alignment) is
/// kept and wins, since it is appended after ours.
fn inline_styles(html: &str) -> String {
    // The code inside a fence needs none of the inline-code treatment; hide
    // those pairs while `code` is styled, then put them back styled as a
    // block.
    const FENCE: &str = "\u{0}FENCE\u{0}";
    let mut out = html.replace("<pre><code", FENCE);
    for (tag, css) in [
        ("code", "background:rgba(128,128,128,0.12);padding:1px 4px;border-radius:4px;\
                  font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:0.95em"),
        ("table", "border-collapse:collapse;margin:8px 0"),
        ("th", "border:1px solid rgba(128,128,128,0.45);padding:6px 10px;\
                background:rgba(128,128,128,0.12);text-align:left"),
        ("td", "border:1px solid rgba(128,128,128,0.45);padding:6px 10px"),
        ("blockquote", "margin:0 0 0 8px;padding-left:10px;\
                        border-left:3px solid rgba(128,128,128,0.4)"),
        ("hr", "border:none;border-top:1px solid rgba(128,128,128,0.4);margin:14px 0"),
        ("img", "max-width:100%;height:auto"),
        ("dt", "font-weight:600;margin-top:6px"),
        ("dd", "margin:0 0 4px 20px"),
        ("kbd", "border:1px solid rgba(128,128,128,0.5);border-radius:4px;padding:0 4px;\
                 font-family:ui-monospace,monospace;font-size:0.9em"),
    ] {
        out = add_style(&out, tag, css);
    }
    out = out.replace(
        FENCE,
        "<pre style=\"background:rgba(128,128,128,0.12);padding:10px 12px;border-radius:6px;\
         overflow:auto;white-space:pre-wrap;word-wrap:break-word\"><code \
         style=\"font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:0.95em\"",
    );
    out = out.replace("<input disabled=\"\" type=\"checkbox\"", "<input disabled=\"\" type=\"checkbox\" style=\"margin-right:6px\"");
    out = out.replace(
        "<div class=\"footnote-definition\"",
        "<div class=\"footnote-definition\" style=\"font-size:0.92em;opacity:0.85\"",
    );
    out
}

/// Give every `<tag>` and `<tag …>` in `html` a `style`, merged with one the
/// tag already carries.
fn add_style(html: &str, tag: &str, css: &str) -> String {
    let open = format!("<{tag}");
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    while let Some(at) = rest.find(&open) {
        out.push_str(&rest[..at]);
        let after = &rest[at + open.len()..];
        out.push_str(&open);
        // `<code` must not match `<codepoint`, and a tag with no `>` is not
        // a tag at all.
        let boundary = after.starts_with('>') || after.starts_with(' ') || after.starts_with('/');
        let close = after.find('>');
        let (Some(close), true) = (close, boundary) else {
            rest = after;
            continue;
        };
        let attrs = &after[..close];
        match attrs.find("style=\"") {
            // Merged, not appended: HTML keeps the *first* of two attributes
            // with the same name, so a second `style` would be dropped
            // whole. Ours goes at the front of the existing value, where the
            // tag's own rules still override it.
            Some(p) => {
                let head = p + "style=\"".len();
                out.push_str(&attrs[..head]);
                out.push_str(css);
                out.push(';');
                out.push_str(&attrs[head..]);
            }
            None => {
                out.push_str(&format!(" style=\"{css}\""));
                out.push_str(attrs);
            }
        }
        out.push('>');
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// HTML back to Markdown
// ---------------------------------------------------------------------------

/// A parsed element.
#[derive(Debug)]
struct Elem {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    Text(String),
    Elem(Elem),
}

impl Elem {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Tags that never have children.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Tags that close an open `<p>` (and each other, for list and table parts).
fn closes(open: &str, incoming: &str) -> bool {
    match open {
        "p" => matches!(
            incoming,
            "p" | "div" | "ul" | "ol" | "li" | "table" | "blockquote" | "pre" | "hr" | "h1" | "h2"
                | "h3" | "h4" | "h5" | "h6" | "dl" | "section" | "article" | "header" | "footer"
        ),
        "li" => matches!(incoming, "li"),
        "dt" | "dd" => matches!(incoming, "dt" | "dd"),
        "td" | "th" => matches!(incoming, "td" | "th" | "tr"),
        "tr" => matches!(incoming, "tr"),
        _ => false,
    }
}

/// Parse HTML into a forest. Deliberately forgiving: unknown tags, unclosed
/// tags and stray closers are all survivable, because this runs over mail
/// bodies and hand-written source, not validated documents.
fn parse(html: &str) -> Vec<Node> {
    let mut roots: Vec<Node> = Vec::new();
    let mut stack: Vec<Elem> = Vec::new();
    let b = html.as_bytes();
    let mut i = 0usize;
    let mut text = String::new();

    macro_rules! push_node {
        ($node:expr) => {{
            let node = $node;
            match stack.last_mut() {
                Some(top) => top.children.push(node),
                None => roots.push(node),
            }
        }};
    }
    macro_rules! flush_text {
        () => {
            if !text.is_empty() {
                let t = std::mem::take(&mut text);
                push_node!(Node::Text(decode_entities(&t)));
            }
        };
    }

    while i < b.len() {
        if b[i] != b'<' {
            let next = html[i..].find('<').map(|p| i + p).unwrap_or(html.len());
            text.push_str(&html[i..next]);
            i = next;
            continue;
        }
        let rest = &html[i..];
        if rest.starts_with("<!--") {
            i += rest.find("-->").map(|p| p + 3).unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i += rest.find('>').map(|p| p + 1).unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("</") {
            let Some(gt) = rest.find('>') else { break };
            let name = rest[2..gt].trim().to_ascii_lowercase();
            flush_text!();
            // Close up to the matching tag; a closer with nothing open to
            // match is simply dropped.
            if let Some(pos) = stack.iter().rposition(|e| e.name == name) {
                while stack.len() > pos {
                    let done = stack.pop().unwrap();
                    push_node!(Node::Elem(done));
                }
            }
            i += gt + 1;
            continue;
        }
        // An opening tag, or a lone `<` that is just text.
        let Some((name, attrs, self_closing, len)) = parse_tag(rest) else {
            text.push('<');
            i += 1;
            continue;
        };
        flush_text!();
        while stack.last().is_some_and(|top| closes(&top.name, &name)) {
            let done = stack.pop().unwrap();
            push_node!(Node::Elem(done));
        }
        let elem = Elem { name: name.clone(), attrs, children: Vec::new() };
        if self_closing || VOID.contains(&name.as_str()) {
            push_node!(Node::Elem(elem));
        } else if name == "script" || name == "style" {
            // Their contents are never text: skip to the matching close.
            let close = format!("</{name}");
            let after = &html[i + len..];
            let end = after.to_ascii_lowercase().find(&close).unwrap_or(after.len());
            i += len + end;
            i += html[i..].find('>').map(|p| p + 1).unwrap_or(html.len() - i);
            continue;
        } else {
            stack.push(elem);
        }
        i += len;
    }
    flush_text!();
    while let Some(done) = stack.pop() {
        match stack.last_mut() {
            Some(top) => top.children.push(Node::Elem(done)),
            None => roots.push(Node::Elem(done)),
        }
    }
    roots
}

/// `(name, attributes, self-closing, byte length)` of the tag starting at
/// `rest`, or `None` when this `<` does not open one.
fn parse_tag(rest: &str) -> Option<(String, Vec<(String, String)>, bool, usize)> {
    let b = rest.as_bytes();
    if b.len() < 2 || !(b[1].is_ascii_alphabetic()) {
        return None;
    }
    let mut i = 1usize;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b':') {
        i += 1;
    }
    let name = rest[1..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return Some((name, attrs, self_closing, i));
        }
        if b[i] == b'>' {
            return Some((name, attrs, self_closing, i + 1));
        }
        if b[i] == b'/' {
            self_closing = true;
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !matches!(b[i], b'=' | b'>' | b'/') && !(b[i] as char).is_whitespace() {
            i += 1;
        }
        let key = rest[start..i].to_ascii_lowercase();
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && (b[i] as char).is_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let quote = b[i];
                i += 1;
                let vstart = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                value = decode_entities(&rest[vstart..i]);
                i = (i + 1).min(b.len());
            } else {
                let vstart = i;
                while i < b.len() && b[i] != b'>' && !(b[i] as char).is_whitespace() {
                    i += 1;
                }
                value = decode_entities(&rest[vstart..i]);
            }
        }
        if !key.is_empty() {
            attrs.push((key, value));
        }
    }
}

/// The handful of entities a mail body actually carries, plus numeric ones.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let Some(end) = tail.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = tail;
            continue;
        };
        let name = &tail[..end];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            "hellip" => Some('\u{2026}'),
            "mdash" => Some('\u{2014}'),
            "ndash" => Some('\u{2013}'),
            "lsquo" => Some('\u{2018}'),
            "rsquo" => Some('\u{2019}'),
            "ldquo" => Some('\u{201c}'),
            "rdquo" => Some('\u{201d}'),
            _ => name
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// HTML to Markdown source. Used when the composer switches format, and to
/// write the plain-text alternative of a message composed as HTML — Markdown
/// being, by design, the form of plain text that still reads as itself.
pub fn from_html(html: &str) -> String {
    let nodes = parse(html);
    let md = blocks(&nodes);
    md.trim_matches('\n').to_string()
}

/// The plain-text alternative for an HTML body.
pub fn html_to_text(html: &str) -> String {
    from_html(html)
}

/// Is this element a block, as far as the Markdown writer is concerned?
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "li" | "blockquote"
            | "pre" | "hr" | "table" | "dl" | "dt" | "dd" | "section" | "article" | "header"
            | "footer" | "main" | "aside" | "figure" | "figcaption" | "body" | "html" | "form"
            | "nav" | "tr" | "thead" | "tbody" | "tfoot"
    )
}

/// Render nodes in block context: each block on its own, paragraphs made out
/// of the inline runs between them.
fn blocks(nodes: &[Node]) -> String {
    let refs: Vec<&Node> = nodes.iter().collect();
    blocks_ref(&refs)
}

fn blocks_ref(nodes: &[&Node]) -> String {
    let mut out = String::new();
    let mut pending: Vec<&Node> = Vec::new();

    fn flush(pending: &mut Vec<&Node>, out: &mut String) {
        if pending.is_empty() {
            return;
        }
        let text = inlines(pending.iter().copied());
        pending.clear();
        let text = text.trim_matches(|c: char| c == ' ' || c == '\n');
        if !text.is_empty() {
            push_block(out, text);
        }
    }

    for node in nodes {
        match node {
            Node::Elem(e) if is_block(&e.name) || e.name == "table" => {
                flush(&mut pending, &mut out);
                let rendered = block(e);
                if !rendered.trim().is_empty() {
                    push_block(&mut out, &rendered);
                }
            }
            other => pending.push(other),
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Append one block, separated from the last by a blank line.
fn push_block(out: &mut String, text: &str) {
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(text.trim_end_matches('\n'));
}

/// One block element as Markdown.
fn block(e: &Elem) -> String {
    match e.name.as_str() {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = e.name[1..].parse::<usize>().unwrap_or(1);
            let text = inlines(e.children.iter()).trim().to_string();
            let id = e.attr("id").filter(|s| !s.is_empty()).map(|s| format!(" {{#{s}}}")).unwrap_or_default();
            format!("{} {text}{id}", "#".repeat(level))
        }
        "hr" => "---".to_string(),
        "pre" => {
            // The fence keeps the code exactly; a `language-x` class on the
            // inner <code> is the info string it came from.
            let lang = find_code_language(&e.children).unwrap_or_default();
            let text = raw_text(&e.children);
            let text = text.trim_matches('\n');
            let fence = if text.contains("```") { "````" } else { "```" };
            format!("{fence}{lang}\n{text}\n{fence}")
        }
        "blockquote" => prefix_lines(&blocks(&e.children), "> "),
        "ul" | "ol" => list(e),
        "li" => blocks(&e.children),
        "dl" => definition_list(e),
        "dt" => inlines(e.children.iter()).trim().to_string(),
        "dd" => format!(": {}", inlines(e.children.iter()).trim()),
        "table" => table(e),
        _ => blocks(&e.children),
    }
}

/// `-` or `1.` items, with continuation lines indented under their marker.
fn list(e: &Elem) -> String {
    let ordered = e.name == "ol";
    let mut n: usize = e.attr("start").and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut out = String::new();
    for child in &e.children {
        let Node::Elem(li) = child else { continue };
        if li.name != "li" {
            continue;
        }
        let marker = if ordered {
            let m = format!("{n}. ");
            n += 1;
            m
        } else {
            "- ".to_string()
        };
        // A task list keeps its box: the checkbox is an <input> the writer
        // put there, and `- [x]` is how it goes back to Markdown.
        let (box_mark, body) = task_marker(li);
        let text = blocks_ref(&body);
        let indent = " ".repeat(marker.chars().count());
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("");
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{marker}{box_mark}{first}"));
        for line in lines {
            out.push('\n');
            if line.is_empty() {
                continue;
            }
            out.push_str(&indent);
            out.push_str(line);
        }
    }
    out
}

/// `("[x] " | "[ ] " | "", the item's remaining children)`.
fn task_marker(li: &Elem) -> (String, Vec<&Node>) {
    let mut rest: Vec<&Node> = Vec::new();
    let mut mark = String::new();
    for child in &li.children {
        match child {
            Node::Elem(e) if e.name == "input" && e.attr("type") == Some("checkbox") => {
                mark = if e.attrs.iter().any(|(k, _)| k == "checked") { "[x] ".into() } else { "[ ] ".into() };
            }
            other => rest.push(other),
        }
    }
    (mark, rest)
}

fn definition_list(e: &Elem) -> String {
    let mut out = String::new();
    for child in &e.children {
        let Node::Elem(row) = child else { continue };
        match row.name.as_str() {
            "dt" => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(inlines(row.children.iter()).trim());
            }
            "dd" => {
                out.push_str("\n: ");
                out.push_str(inlines(row.children.iter()).trim());
            }
            _ => {}
        }
    }
    out
}

/// A GFM pipe table. Alignment comes from the cells' own `text-align`, which
/// is what [`to_html`] wrote there in the first place.
fn table(e: &Elem) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut aligns: Vec<&'static str> = Vec::new();
    let mut header = 0usize;
    collect_rows(&e.children, &mut rows, &mut aligns, &mut header);
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    aligns.resize(cols, "");
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        let mut cells: Vec<String> = row.clone();
        cells.resize(cols, String::new());
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
        if i == 0 {
            let rule: Vec<String> = aligns
                .iter()
                .map(|a| match *a {
                    "center" => ":---:".to_string(),
                    "right" => "---:".to_string(),
                    "left" => ":---".to_string(),
                    _ => "---".to_string(),
                })
                .collect();
            out.push_str(&format!("| {} |\n", rule.join(" | ")));
        }
    }
    // A table with no header row still needs one: an empty first row keeps
    // it a table instead of collapsing into prose.
    if header == 0 {
        let empty: Vec<String> = (0..cols).map(|_| String::new()).collect();
        out = format!("| {} |\n| {} |\n{}", empty.join(" | "), vec!["---"; cols].join(" | "), out);
    }
    out.trim_end().to_string()
}

fn collect_rows(
    nodes: &[Node],
    rows: &mut Vec<Vec<String>>,
    aligns: &mut Vec<&'static str>,
    header: &mut usize,
) {
    for node in nodes {
        let Node::Elem(e) = node else { continue };
        match e.name.as_str() {
            "thead" | "tbody" | "tfoot" => {
                if e.name == "thead" {
                    *header = 1;
                }
                collect_rows(&e.children, rows, aligns, header)
            }
            "tr" => {
                let mut row = Vec::new();
                for cell in &e.children {
                    let Node::Elem(c) = cell else { continue };
                    if c.name != "td" && c.name != "th" {
                        continue;
                    }
                    if rows.is_empty() {
                        aligns.push(alignment(c));
                    }
                    // A newline inside a cell would end the table row.
                    row.push(inlines(c.children.iter()).trim().replace('\n', " ").replace('|', "\\|"));
                }
                rows.push(row);
            }
            _ => collect_rows(&e.children, rows, aligns, header),
        }
    }
}

fn alignment(cell: &Elem) -> &'static str {
    let style = cell.attr("style").unwrap_or("").to_ascii_lowercase();
    let align = cell.attr("align").unwrap_or("").to_ascii_lowercase();
    for want in ["center", "right", "left"] {
        if style.contains(&format!("text-align: {want}"))
            || style.contains(&format!("text-align:{want}"))
            || align == want
        {
            return match want {
                "center" => "center",
                "right" => "right",
                _ => "left",
            };
        }
    }
    ""
}

/// The `language-x` class of a fenced block's inner `<code>`.
fn find_code_language(nodes: &[Node]) -> Option<String> {
    for node in nodes {
        if let Node::Elem(e) = node {
            if e.name == "code" {
                if let Some(class) = e.attr("class") {
                    if let Some(lang) = class.split_whitespace().find_map(|c| c.strip_prefix("language-")) {
                        return Some(lang.to_string());
                    }
                }
            }
            if let Some(found) = find_code_language(&e.children) {
                return Some(found);
            }
        }
    }
    None
}

/// The text of a subtree with nothing escaped and no whitespace collapsing —
/// what a `<pre>` holds.
fn raw_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Elem(e) if e.name == "br" => out.push('\n'),
            Node::Elem(e) => out.push_str(&raw_text(&e.children)),
        }
    }
    out
}

fn prefix_lines(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| if l.is_empty() { prefix.trim_end().to_string() } else { format!("{prefix}{l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render an inline run.
fn inlines<'a>(nodes: impl Iterator<Item = &'a Node>) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(&escape_md(&collapse(t))),
            Node::Elem(e) => out.push_str(&inline(e)),
        }
    }
    out
}

/// Collapse HTML whitespace the way a renderer would.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    if space && !out.is_empty() {
        out.push(' ');
    }
    out
}

/// Backslash the characters that would otherwise be read as markup. `_` is
/// left alone deliberately: it is far more often part of a name than an
/// emphasis mark, and CommonMark ignores it inside a word anyway.
fn escape_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '`' | '[' | ']' | '<') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn inline(e: &Elem) -> String {
    let inner = || inlines(e.children.iter());
    match e.name.as_str() {
        "br" => "  \n".to_string(),
        "strong" | "b" => wrap(&inner(), "**"),
        "em" | "i" => wrap(&inner(), "*"),
        "del" | "s" | "strike" => wrap(&inner(), "~~"),
        "mark" => wrap(&inner(), "=="),
        "sup" => wrap(&inner(), "^"),
        "sub" => wrap(&inner(), "~"),
        "code" => {
            let text = raw_text(&e.children);
            let fence = if text.contains('`') { "``" } else { "`" };
            format!("{fence}{text}{fence}")
        }
        "kbd" => format!("<kbd>{}</kbd>", raw_text(&e.children)),
        "img" => {
            let alt = e.attr("alt").unwrap_or("");
            let src = e.attr("src").unwrap_or("");
            format!("![{alt}]({src})")
        }
        "a" => {
            let href = e.attr("href").unwrap_or("").to_string();
            let text = inner();
            let bare = text.trim();
            if href.is_empty() {
                text
            } else if bare == href || href == format!("mailto:{bare}") {
                // An autolink reads better as itself than as a link whose
                // text and target are the same string twice.
                bare.replace('\\', "")
            } else {
                format!("[{}]({})", text.trim(), href)
            }
        }
        // Structure with no Markdown of its own: keep the text.
        _ => inner(),
    }
}

/// Wrap `text` in a marker, keeping the spaces it starts or ends with
/// outside — `** bold **` is not emphasis at all.
fn wrap(text: &str, marker: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return text.to_string();
    }
    let lead = if text.starts_with(' ') { " " } else { "" };
    let trail = if text.ends_with(' ') { " " } else { "" };
    format!("{lead}{marker}{trimmed}{marker}{trail}")
}

// ---------------------------------------------------------------------------
// Outgoing HTML
// ---------------------------------------------------------------------------

/// The sanitizer every user-authored body goes through on its way out: the
/// HTML a message is written in by hand, and the HTML a Markdown source
/// renders to (Markdown may carry raw HTML of its own).
///
/// It is the user's own material, so this is not a defence against them —
/// it is a defence for them. A `<script>` that reaches a recipient will at
/// best be stripped by their client and at worst put the message in the
/// spam folder, and a `<style>` block restyles whatever it lands in. The
/// structure mail actually needs — tables, inline styles, images, the tags
/// Markdown emits — all survives.
pub fn sanitize_outgoing(html: &str) -> String {
    use std::collections::HashSet;
    let mut b = ammonia::Builder::default();
    b.add_tags([
        "table", "thead", "tbody", "tfoot", "tr", "td", "th", "font", "center", "u", "span", "img",
        "mark", "sup", "sub", "del", "ins", "kbd", "samp", "var", "dl", "dt", "dd", "figure",
        "figcaption", "input", "section", "article", "details", "summary", "small", "big",
    ])
    .add_generic_attributes([
        "style", "class", "id", "align", "valign", "width", "height", "bgcolor", "border",
        "cellpadding", "cellspacing", "color", "face", "size", "colspan", "rowspan", "start",
        "dir", "lang",
    ])
    .add_tag_attributes("img", ["src", "alt", "width", "height", "title"])
    // Task lists: the box is an <input>, and a disabled one carries no
    // behaviour of any kind.
    .add_tag_attributes("input", ["type", "checked", "disabled"])
    .url_schemes(HashSet::from(["http", "https", "mailto", "tel", "data", "cid"]))
    .link_rel(None);
    b.clean(html).to_string()
}

/// Break already-written HTML onto one line per block, so switching a
/// message into the HTML view shows something a person can edit rather than
/// one unbroken line. Nothing inside a `<pre>` is touched, where a line
/// break would change the message.
pub fn pretty_html(html: &str) -> String {
    const BLOCKS: &[&str] = &[
        "p", "div", "table", "thead", "tbody", "tfoot", "tr", "ul", "ol", "li", "blockquote", "h1",
        "h2", "h3", "h4", "h5", "h6", "hr", "pre", "dl", "dt", "dd", "section", "article",
    ];
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if tail.starts_with("<pre") {
            // Straight through to the end of the block.
            let end = tail.find("</pre>").map(|p| p + 6).unwrap_or(tail.len());
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&tail[..end]);
            out.push('\n');
            rest = &tail[end..];
            continue;
        }
        let name: String = tail
            .trim_start_matches('<')
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        let is_block_tag = BLOCKS.contains(&name.as_str());
        let closing = tail.starts_with("</");
        let end = tail.find('>').map(|p| p + 1).unwrap_or(tail.len());
        if is_block_tag && !closing && !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&tail[..end]);
        if is_block_tag && closing {
            out.push('\n');
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    // Collapse the runs of blank lines the breaks above can leave behind.
    let mut cleaned = String::with_capacity(out.len());
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() && cleaned.ends_with("\n\n") {
            continue;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    cleaned.trim_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert `html` contains `needle`, with the whole rendering in the
    /// failure so a broken case reads itself.
    fn has(html: &str, needle: &str) {
        assert!(html.contains(needle), "expected {needle:?} in:\n{html}");
    }

    // -- markdownguide.org: basic syntax ---------------------------------

    #[test]
    fn headings() {
        has(&to_html("# One"), "<h1>One</h1>");
        has(&to_html("###### Six"), "<h6>Six</h6>");
        has(&to_html("Setext\n======"), "<h1>Setext</h1>");
        has(&to_html("Setext\n------"), "<h2>Setext</h2>");
    }

    #[test]
    fn emphasis() {
        has(&to_html("**bold**"), "<strong>bold</strong>");
        has(&to_html("__bold__"), "<strong>bold</strong>");
        has(&to_html("*italic*"), "<em>italic</em>");
        has(&to_html("_italic_"), "<em>italic</em>");
        has(&to_html("***both***"), "<em><strong>both</strong></em>");
    }

    #[test]
    fn blockquotes_lists_and_rules() {
        has(&to_html("> quoted"), "<blockquote");
        has(&to_html("> outer\n>> nested"), "<blockquote");
        has(&to_html("- one\n- two"), "<ul>");
        has(&to_html("1. one\n2. two"), "<ol>");
        has(&to_html("---"), "<hr");
        has(&to_html("***"), "<hr");
    }

    #[test]
    fn code_links_and_images() {
        has(&to_html("`inline`"), "<code");
        has(&to_html("    indented block"), "<pre");
        has(&to_html("[text](https://example.com)"), "href=\"https://example.com\"");
        has(&to_html("[text](https://example.com \"Title\")"), "title=\"Title\"");
        has(&to_html("<https://example.com>"), "href=\"https://example.com\"");
        has(&to_html("![alt](cid:pic)"), "<img");
        has(&to_html("[ref][1]\n\n[1]: https://example.com"), "href=\"https://example.com\"");
    }

    #[test]
    fn hard_break_and_escapes() {
        has(&to_html("one  \ntwo"), "<br />");
        has(&to_html(r"\*not emphasis\*"), "*not emphasis*");
    }

    // -- markdownguide.org: extended syntax -------------------------------

    #[test]
    fn tables_with_alignment() {
        let html = to_html("| a | b |\n| :- | ---: |\n| 1 | 2 |");
        has(&html, "<table");
        has(&html, "text-align: right");
        has(&html, "<th");
    }

    #[test]
    fn fenced_code_keeps_its_language() {
        let html = to_html("```rust\nfn main() {}\n```");
        has(&html, "<pre");
        has(&html, "language-rust");
        has(&html, "fn main() {}");
    }

    #[test]
    fn footnotes() {
        let html = to_html("Here[^1].\n\n[^1]: The note.");
        has(&html, "footnote-reference");
        has(&html, "The note.");
    }

    #[test]
    fn heading_ids() {
        has(&to_html("### Heading {#custom-id}"), "id=\"custom-id\"");
    }

    #[test]
    fn definition_lists() {
        let html = to_html("Term\n: The definition");
        has(&html, "<dt");
        has(&html, "<dd");
    }

    #[test]
    fn strikethrough_task_lists_sub_and_sup() {
        has(&to_html("~~gone~~"), "<del>gone</del>");
        has(&to_html("- [x] done\n- [ ] not"), "type=\"checkbox\"");
        has(&to_html("H~2~O"), "<sub>2</sub>");
        has(&to_html("X^2^"), "<sup>2</sup>");
    }

    #[test]
    fn highlight_emoji_and_bare_urls() {
        has(&to_html("==important=="), "<mark");
        has(&to_html("==**both**=="), "<mark");
        has(&to_html("ship it :rocket:"), "\u{1F680}");
        has(&to_html("unknown :nonsense: stays"), ":nonsense:");
        has(&to_html("see https://example.com/x today"), "href=\"https://example.com/x\"");
        has(&to_html("see www.example.com."), "href=\"https://www.example.com\"");
        has(&to_html("mail ada@example.com now"), "href=\"mailto:ada@example.com\"");
    }

    #[test]
    fn a_url_keeps_the_sentence_punctuation_out_of_the_link() {
        let html = to_html("Read https://example.com/page, then stop.");
        has(&html, "href=\"https://example.com/page\"");
        has(&html, ", then stop.");
    }

    #[test]
    fn code_is_left_alone() {
        let html = to_html("```\nhttps://example.com :rocket: ==x==\n```");
        has(&html, "https://example.com :rocket: ==x==");
        assert!(!html.contains("<mark"), "{html}");
    }

    #[test]
    fn styles_ride_on_the_tags() {
        let html = to_html("| a |\n| - |\n| 1 |");
        has(&html, "<table style=");
        has(&html, "border-collapse");
        // The writer's own alignment survives the merge into one attribute
        // — two `style`s on a tag and the second is thrown away.
        let aligned = to_html("| a |\n| ---: |\n| 1 |");
        has(&aligned, "text-align: right");
        has(&aligned, "padding:6px 10px;text-align: right");
        for tag in ["<th ", "<td "] {
            let at = aligned.find(tag).unwrap();
            let open = &aligned[at..at + aligned[at..].find('>').unwrap()];
            assert_eq!(open.matches("style=").count(), 1, "{open}");
        }
    }

    #[test]
    fn raw_html_passes_through() {
        has(&to_html("<span class=\"x\">kept</span>"), "<span class=\"x\">kept</span>");
    }

    // -- HTML back to Markdown -------------------------------------------

    #[test]
    fn round_trips_basic_syntax() {
        let src = "# Title\n\nSome **bold** and *italic* and `code`.\n\n- one\n- two\n\n> quoted\n\n[link](https://example.com)";
        let back = from_html(&to_html(src));
        has(&back, "# Title");
        has(&back, "**bold**");
        has(&back, "*italic*");
        has(&back, "`code`");
        has(&back, "- one");
        has(&back, "> quoted");
        has(&back, "[link](https://example.com)");
    }

    #[test]
    fn round_trips_extended_syntax() {
        let src = "| a | b |\n| --- | ---: |\n| 1 | 2 |\n\n~~gone~~\n\n- [x] done\n- [ ] not\n\n```rust\nfn f() {}\n```";
        let back = from_html(&to_html(src));
        has(&back, "| a | b |");
        has(&back, "---:");
        has(&back, "~~gone~~");
        has(&back, "- [x] done");
        has(&back, "- [ ] not");
        has(&back, "```rust");
        has(&back, "fn f() {}");
    }

    #[test]
    fn converts_a_composed_reply() {
        // The shape the rich editor hands over for a reply.
        let html = "<div>Yes, agreed.</div><p class=\"vireo-quote-attr\">On Monday, Ada wrote:</p>\
                    <blockquote><div>The original line.</div><div>And another.</div></blockquote>";
        let md = from_html(html);
        has(&md, "Yes, agreed.");
        has(&md, "> The original line.");
        has(&md, "> And another.");
    }

    #[test]
    fn keeps_an_inline_image_and_its_name() {
        let md = from_html("<p><img src=\"data:image/png;base64,AAA\" alt=\"shot.png\"></p>");
        has(&md, "![shot.png](data:image/png;base64,AAA)");
    }

    #[test]
    fn escapes_what_would_otherwise_be_markup() {
        let md = from_html("<p>2 * 3 * 4 and [brackets]</p>");
        has(&md, r"2 \* 3 \* 4");
        has(&md, r"\[brackets\]");
        // ...and it comes back as the text it was.
        has(&to_html(&md), "2 * 3 * 4 and [brackets]");
    }

    #[test]
    fn entities_and_breaks() {
        assert_eq!(from_html("<p>a &amp; b &lt;c&gt;</p>"), r"a & b \<c>");
        has(&from_html("<p>one<br>two</p>"), "one  \ntwo");
    }

    #[test]
    fn a_bare_link_reads_as_itself() {
        assert_eq!(from_html("<p><a href=\"https://example.com\">https://example.com</a></p>"), "https://example.com");
        assert_eq!(from_html("<p><a href=\"mailto:ada@example.com\">ada@example.com</a></p>"), "ada@example.com");
    }

    #[test]
    fn nested_lists_indent_under_their_marker() {
        let md = from_html("<ul><li>one<ul><li>inner</li></ul></li><li>two</li></ul>");
        has(&md, "- one");
        has(&md, "  - inner");
        has(&md, "- two");
    }

    #[test]
    fn scripts_and_styles_are_not_text() {
        let md = from_html("<p>before</p><script>alert('x')</script><style>p{color:red}</style><p>after</p>");
        assert!(!md.contains("alert"), "{md}");
        assert!(!md.contains("color:red"), "{md}");
        has(&md, "before");
        has(&md, "after");
    }

    #[test]
    fn unclosed_tags_survive() {
        let md = from_html("<p>one<p>two<div>three");
        has(&md, "one");
        has(&md, "two");
        has(&md, "three");
    }

    // -- Outgoing HTML ----------------------------------------------------

    #[test]
    fn sanitizer_keeps_mail_html_and_drops_the_rest() {
        let dirty = "<table style=\"border-collapse:collapse\"><tr><td bgcolor=\"#eee\">cell</td></tr></table>\
                     <img src=\"cid:pic\" alt=\"x\"><script>alert(1)</script><p onclick=\"go()\">text</p>\
                     <style>p{color:red}</style><mark>hi</mark>";
        let clean = sanitize_outgoing(dirty);
        has(&clean, "<table");
        has(&clean, "border-collapse");
        has(&clean, "bgcolor=\"#eee\"");
        has(&clean, "cid:pic");
        has(&clean, "<mark>hi</mark>");
        assert!(!clean.contains("script"), "{clean}");
        assert!(!clean.contains("onclick"), "{clean}");
        assert!(!clean.contains("color:red"), "{clean}");
    }

    #[test]
    fn a_task_list_survives_the_sanitizer() {
        let clean = sanitize_outgoing(&to_html("- [x] done"));
        has(&clean, "type=\"checkbox\"");
        has(&clean, "checked");
    }

    /// The whole outgoing path, as the composer runs it: render, then
    /// sanitize. What a recipient would notice going missing has to
    /// survive both halves.
    #[test]
    fn the_send_path_keeps_what_the_recipient_would_notice() {
        let src = "# Update\n\n| Item | Cost |\n| --- | ---: |\n| Chair | 40 |\n\n==Read this== and see https://example.com/x\n\n- [x] ordered\n- [ ] paid\n\n```sh\necho hi\n```";
        let out = sanitize_outgoing(&to_html(src));
        has(&out, "<h1>Update</h1>");
        has(&out, "border-collapse");
        has(&out, "text-align: right");
        has(&out, "background:#fff3a3");
        has(&out, "href=\"https://example.com/x\"");
        has(&out, "type=\"checkbox\"");
        has(&out, "echo hi");
        // And the plain-text alternative is the source itself, which the
        // composer sends unchanged.
        assert!(src.contains("==Read this=="));
    }

    #[test]
    fn pretty_html_breaks_blocks_but_not_code() {
        let out = pretty_html("<p>one</p><p>two</p><pre><code>a\nb</code></pre>");
        assert!(out.lines().count() >= 3, "{out}");
        has(&out, "<pre><code>a\nb</code></pre>");
    }
}
