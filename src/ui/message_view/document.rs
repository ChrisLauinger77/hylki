//! The reader's wrapper document: one sandboxed frame per message, under
//! the card chrome a conversation shows. A static builder (no `&self`) so the
//! tests can exercise it without a display; the live theme reaches it through
//! the `LIVE_*` thread-locals in `frame.rs`.

use super::{
    att_row_html, attr_escape, card_action_button, escape_text, has_remote_resources,
    inline_icon_svg, invite_row_html, message_frame, paints_own_background, recipient_count,
    recipients_html, tag_chips_html, template_row_html, unsub_row_html, MessageView, CHROME,
    FOLD_BAR_FACE, GROUND, LIVE_ATTS, LIVE_FOLDBAR, LIVE_FOLDED, LIVE_GROUNDS, LIVE_INVITE,
    LIVE_TAGS, LIVE_TEMPLATES, LIVE_TRANSLATE, LIVE_TRANSLATE_ON, LIVE_UNSUB, PAGE, SIZE_SCRIPT,
};
use crate::i18n::{i18n, ni18n_f};
use crate::models::Message;
use relm4::prelude::*;

/// What every card of one document shares.
#[derive(Clone, Copy)]
struct Cards<'a> {
    thread: &'a [Message],
    folder_labels: &'a std::collections::HashMap<(u32, u32), String>,
    heights: &'a std::collections::HashMap<(u32, u32), u32>,
    selected: &'a [(u32, u32)],
    accent: &'a str,
    restrict: bool,
    dark: bool,
    always_show_recipients: bool,
    style: &'a crate::config::ReaderStyle,
    sender_style: &'a std::collections::HashSet<(u32, u32)>,
    reader: bool,
    zoom: u32,
    /// Card selection only means something between cards.
    mark_selection: bool,
}

impl MessageView {
    /// The wrapper document: one sandboxed iframe per message (so each email's
    /// CSS is fully isolated and its scripts can't run), with per-message
    /// headers in conversation mode. A small script sizes each iframe to its
    /// content.
    ///
    /// Takes the thread rather than `&self` so it can be exercised without a GTK
    /// display: what it emits is a security boundary, and one that needs
    /// regression cover.
    pub(super) fn conversation_document(
        thread: &[Message],
        folder_labels: &std::collections::HashMap<(u32, u32), String>,
        _no_autoread: &std::collections::HashSet<(u32, u32)>,
        heights: &std::collections::HashMap<(u32, u32), u32>,
        selected: &[(u32, u32)],
        accent: &str,
        restrict: bool,
        dark: bool,
        always_show_recipients: bool,
        single_message_card: bool,
        style: &crate::config::ReaderStyle,
        sender_style: &std::collections::HashSet<(u32, u32)>,
        reader: bool,
        zoom: u32,
    ) -> String {
        // Every message renders with the conversation chrome — a thread of one
        // gets the same in-document header. But only a real conversation is
        // *carded*: inset rounded cards on the deeper page. A lone message
        // goes full-bleed — no gutter, no radius, its ground filling the whole
        // view — so it reads as a message rather than a card in a margin.
        let conversation = !thread.is_empty();
        // A lone message cards up too when the preference asks (#57);
        // otherwise it keeps the full-bleed treatment below.
        let carded = thread.len() > 1 || (conversation && single_message_card);
        // Card selection only means something between cards: a lone message is
        // already the selection, so it never wears the accent outline.
        let mark_selection = thread.len() > 1;
        let cards = Cards {
            thread,
            folder_labels,
            heights,
            selected,
            accent,
            restrict,
            dark,
            always_show_recipients,
            style,
            sender_style,
            reader,
            zoom,
            mark_selection,
        };
        let mut sections = String::new();
        for m in thread {
            let body = card_body(&cards, m);
            if conversation {
                let ava_html = card_avatar(m, dark);
                sections.push_str(&card_html(&cards, m, &ava_html, body));
            } else {
                sections.push_str(&body);
            }
        }
        let scheme = if dark { "dark" } else { "light" };
        // The ⋯ actions toggle: plain white in dark mode — the inherited text
        // color reads too pale there.
        let toggle_color = if dark { "#ffffff" } else { "inherit" };
        // Paint the wrapper and the (still-loading) iframes in the theme color so
        // there's no white flash before each message's content renders. The live
        // theme's grounds when the reader set them (issue #62); the stock GNOME
        // values otherwise (tests).
        let (bg, deep, chrome) = LIVE_GROUNDS
            .with(|g| g.borrow().clone())
            .unwrap_or_else(|| {
                let (g, p, c) = if dark {
                    (GROUND.1, PAGE.1, CHROME.1)
                } else {
                    (GROUND.0, PAGE.0, CHROME.0)
                };
                (g.to_string(), p.to_string(), c.to_string())
            });
        // Each message card only reads as a card against a slightly deeper
        // ground than its own; a full-bleed single message sits on its own
        // ground — the chrome ground when it paints no background (see
        // `plain_css` below) — so the whole view is one color.
        let single_ground = if !carded
            && thread.len() == 1
            && (reader || !paints_own_background(&thread[0].body))
        {
            chrome.clone()
        } else {
            bg.clone()
        };
        let page = if carded { deep } else { single_ground };
        let body_class = if carded { " class=\"hylki-conv\"" } else { "" };
        // Defence in depth for the wrapper: the only script allowed to run is the
        // one carrying this render's nonce, which is ours. Anything a message
        // manages to smuggle into this document — an injected `<script>`, an
        // `onerror=` handler — is refused by the engine even if the escaping
        // above is ever wrong again.
        //
        // Deliberately *only* `script-src`/`object-src`/`base-uri`: no
        // `default-src`. A wrapper policy is inherited by the `srcdoc` frames, so
        // restricting images or styles here would silently re-block the remote
        // content the user has chosen to load. Each frame carries its own
        // `default-src 'none'` policy for that.
        let nonce = crate::rng::nonce(24).ok();
        let csp = match &nonce {
            Some(n) => format!(
                "<meta http-equiv=\"Content-Security-Policy\" content=\"\
                 script-src 'nonce-{n}'; object-src 'none'; base-uri 'none'\">"
            ),
            // No entropy, no nonce we can trust — allow no script at all. The
            // iframes render at their default height instead of resizing, which
            // is a visual degradation, not a security one.
            None => "<meta http-equiv=\"Content-Security-Policy\" content=\"\
                     script-src 'none'; object-src 'none'; base-uri 'none'\">"
                .to_string(),
        };
        let sizer = match &nonce {
            Some(n) => {
                // The fold's tooltips, in the user's language.
                let qt =
                    serde_json::to_string(&[i18n("Show quoted text"), i18n("Hide quoted text")])
                        .unwrap_or_default()
                        .replace("</", "<\\/");
                format!("<script nonce=\"{n}\">var vireoQT={qt};{SIZE_SCRIPT}</script>")
            }
            None => String::new(),
        };
        // A single message that paints no background of its own (plain mail,
        // and much styled mail too — Apple Mail styles fonts and wrapping but
        // sets no background) takes the chrome ground for its *whole* frame,
        // matching the GTK subject block and the header above it: one
        // unbroken pane from subject to the end of the message. A message
        // that declares a background keeps the plain ground so its design
        // renders as intended.
        let single_plain =
            !carded && thread.len() == 1 && (reader || !paints_own_background(&thread[0].body));
        let plain_css = if single_plain {
            format!(
                "body:not(.hylki-conv) .hylki-msg,\
                 body:not(.hylki-conv) iframe.hylki-frame{{background:{chrome};}}"
            )
        } else {
            String::new()
        };
        // The headers around the messages are the window's own chrome, so
        // they follow its direction; the messages inside their frames take
        // their own (#366).
        let dir = if crate::i18n::is_rtl() {
            " dir=\"rtl\""
        } else {
            ""
        };
        format!(
            "<!doctype html><html{dir}><head><meta charset=\"utf-8\">{csp}\
             <meta name=\"color-scheme\" content=\"{scheme}\">\
             <style>{css}</style>{sizer}\
             </head><body{body_class}>{sections}</body></html>",
            css = wrapper_css(
                scheme,
                &page,
                &bg,
                &chrome,
                accent,
                toggle_color,
                &plain_css
            ),
        )
    }
}

/// One card's message: its frame, or a placeholder while the body loads.
fn card_body(cards: &Cards<'_>, m: &Message) -> String {
    let Cards {
        restrict,
        dark,
        heights,
        style,
        sender_style,
        accent,
        reader,
        zoom,
        ..
    } = *cards;
    // A card showing its translation (#327): the message with its
    // text replaced, or for plain text a Reader View fragment.
    let (translated, translated_reader) = LIVE_TRANSLATE
        .with(|t| {
            t.borrow()
                .get(&(m.account_id, m.id))
                .and_then(|(_, shown)| shown.clone())
        })
        .map_or((None, false), |(html, reader)| (Some(html), reader));
    let body = if m.body.trim().is_empty() && translated.is_none() {
        "<div class=\"hylki-loading\">Loading…</div>".to_string()
    } else {
        message_frame(
            translated.as_deref().unwrap_or(&m.body),
            restrict,
            dark,
            (m.account_id, m.id),
            heights.get(&(m.account_id, m.id)).copied(),
            // The card's toggle wins over the preference.
            if sender_style.contains(&(m.account_id, m.id)) {
                &crate::config::ReaderStyle::NONE
            } else {
                style
            },
            accent,
            reader || translated_reader,
            zoom,
        )
    };
    body
}

/// The face on a card's header: a mailbox of your own wears the one it was
/// given (#189), anyone else tinted initials.
fn card_avatar(m: &Message, dark: bool) -> String {
    // One of your own mailboxes wrote this card (#189):
    // show what its account chose — the sidebar's picture,
    // or its emoji on the account color — so your own
    // replies in a conversation wear the face you gave
    // that mailbox rather than plain initials.
    let own = crate::avatar::own_face(&m.from_addr).and_then(|face| {
        // The account's own Gravatar leads when it asked
        // for one and the address has one; the picture and
        // the emoji are what it falls back to.
        let gravatar = face
            .gravatar
            .then(|| crate::avatar::own_gravatar_data_uri(&m.from_addr, 26))
            .flatten();
        if let Some(uri) = gravatar {
            return Some(format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">"));
        }
        match (&face.picture, &face.emoji) {
            (Some(path), _) => crate::ui::initials::picture_data_uri(path, 26)
                .map(|uri| format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">")),
            (None, Some(emoji)) => {
                let png = gtk::gdk::RGBA::parse(&face.color)
                    .ok()
                    .and_then(|bg| crate::ui::initials::png_data_uri(emoji, bg, 26));
                Some(match png {
                    Some(uri) => {
                        format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">")
                    }
                    None => format!(
                        "<span class=\"hylki-ava\" style=\"background:{bg}\">{glyph}</span>",
                        bg = attr_escape(&face.color),
                        glyph = escape_text(emoji),
                    ),
                })
            }
            (None, None) => None,
        }
    });
    own.unwrap_or_else(|| {
        let initial = m
            .from_name
            .trim()
            .chars()
            .next()
            .or_else(|| m.from_addr.trim().chars().next())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        let hue = m
            .from_addr
            .to_ascii_lowercase()
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
            % 360;
        let l = if dark { 38 } else { 45 };
        // Drawn ink-centred by ui::initials and embedded as
        // a PNG (the same tint); the markup span stands in
        // only before a window exists to render with.
        let bg = crate::ui::initials::hsl(f64::from(hue), 0.52, f64::from(l) / 100.0);
        match crate::ui::initials::png_data_uri(&initial, bg, 26) {
            Some(uri) => format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">"),
            None => format!(
                "<span class=\"hylki-ava\" style=\"background:hsl({hue},52%,{l}%)\">{}</span>",
                escape_text(&initial),
            ),
        }
    })
}

/// One conversation card: header, banners, the message and its attachments.
fn card_html(cards: &Cards<'_>, m: &Message, ava_html: &str, body: String) -> String {
    let Cards {
        thread,
        mark_selection,
        selected,
        always_show_recipients,
        folder_labels,
        ..
    } = *cards;
    format!(
        "<section class=\"hylki-msg{sel}{unread_cls}{folded_cls}\" data-key=\"{aid}:{id}\">{fbar}\
               <header class=\"hylki-msg-hdr\" data-key=\"{aid}:{id}\" \
                 title=\"{hdr_title}\">\
                 <div class=\"hylki-hdr-line\">\
                   {ava}{dot}<span class=\"hylki-from\">{from}</span>{verify}{addr}\
                   <span class=\"hylki-tags\" data-key=\"{aid}:{id}\">{tags}</span>\
                   <span class=\"hylki-hdr-meta\">{folder}{rcpt_toggle}\
                     <span class=\"hylki-date\">{date}</span></span>\
                   {acts_toggle}{acts}\
                 </div>{rcpt}\
               </header>{tpl}{invite}{unsub}{tr}{body}{atts}</section>",
        aid = m.account_id,
        id = m.id,
        // The message's own attachments beneath its body (#213).
        // The row is always emitted, so files arriving after
        // the paint have somewhere to be patched into.
        atts = LIVE_ATTS.with(|a| {
            let key = (m.account_id, m.id);
            att_row_html(key, a.borrow().get(&key).map(|v| v.as_slice()))
        }),
        // A template's banner (#360), first: what the message
        // is for matters more than anything it says.
        tpl = if LIVE_TEMPLATES.with(|t| t.borrow().contains(&(m.account_id, m.id))) {
            template_row_html((m.account_id, m.id))
        } else {
            String::new()
        },
        // The invitation banner (#223) at the top of the card,
        // above the Unsubscribe one and on the same terms: the
        // container is always there, empty (hidden) for a
        // message about no meeting, so an invitation read after
        // the paint is patched in without a re-render.
        invite = LIVE_INVITE.with(|i| {
            invite_row_html(
                (m.account_id, m.id),
                i.borrow()
                    .get(&(m.account_id, m.id))
                    .map(String::as_str)
                    .unwrap_or(""),
            )
        }),
        // The Unsubscribe banner between header and body: the
        // container is always there, empty (hidden) for a
        // message from no list, so a verdict arriving after the
        // paint is patched in without a re-render.
        // The translation banner (#327), on the same terms.
        tr = LIVE_TRANSLATE.with(|t| {
            format!(
                "<div class=\"hylki-tr\" data-key=\"{}:{}\">{}</div>",
                m.account_id,
                m.id,
                t.borrow()
                    .get(&(m.account_id, m.id))
                    .map(|(b, _)| b.as_str())
                    .unwrap_or("")
            )
        }),
        unsub = LIVE_UNSUB.with(|u| {
            unsub_row_html(
                (m.account_id, m.id),
                u.borrow()
                    .get(&(m.account_id, m.id))
                    .map(String::as_str)
                    .unwrap_or(""),
            )
        }),
        hdr_title = gtk::glib::markup_escape_text(&i18n("Double-click to open in a new window")),
        tags = LIVE_TAGS.with(|t| tag_chips_html(&t.borrow(), &m.keywords, &m.labels)),
        // The ⋯ toggle that expands/collapses the action row when
        // the hidden-until-hover preference is on; CSS keeps it
        // display:none otherwise, so it costs nothing when off.
        acts_toggle = if !thread.is_empty() {
            format!(
                "<button type=\"button\" class=\"hylki-acts-toggle\" \
                     title=\"{acts_title}\" data-key=\"{aid}:{id}\">{svg}</button>",
                aid = m.account_id,
                id = m.id,
                acts_title = gtk::glib::markup_escape_text(&i18n("Actions")),
                // The same ⋯ the list's actions palette toggle uses.
                svg = inline_icon_svg("view-more-horizontal-symbolic"),
            )
        } else {
            String::new()
        },
        // Per-card actions, on every message — single messages
        // included: the reader toolbar carries no per-message
        // icons any more, so the card's own row (and the overflow
        // menu) is where a message is acted on.
        acts = if !thread.is_empty() {
            card_actions(cards, m)
        } else {
            String::new()
        },
        sel = if mark_selection && selected.contains(&(m.account_id, m.id)) {
            " selected"
        } else {
            ""
        },
        unread_cls = if m.unread { " unread" } else { "" },
        // Folded (#326): the header and a line of preview.
        folded_cls = if LIVE_FOLDED.with(|f| f.borrow().contains(&(m.account_id, m.id))) {
            " hylki-folded"
        } else {
            ""
        },
        // What a folded card shows instead of all of it (#326).
        fbar = LIVE_FOLDBAR.with(|b| {
            b.borrow()
                .get(&(m.account_id, m.id))
                .map(|bar| bar.replace(FOLD_BAR_FACE, ava_html))
                .unwrap_or_default()
        }),
        // `escape_text`, not `attr_escape`: these land in element
        // text content, where `<` and `>` are structural. A `From:`
        // display name is attacker-controlled (and RFC 2047-decoded,
        // so any byte sequence can be delivered), and this document
        // is the trusted wrapper — not a sandboxed message frame.
        from = escape_text(&m.from_name),
        // The sender-authentication seal (#88): hidden until a
        // verdict arrives (SenderCheckFor patches it live), then
        // tinted by trust — Bazaar's fixed blue for a pass.
        verify = format!(
            "<button type=\"button\" class=\"hylki-verify\" data-key=\"{aid}:{id}\" \
                 title=\"\">{svg}</button>\
                 <button type=\"button\" class=\"hylki-pgp\" data-key=\"{aid}:{id}\" \
                 title=\"\">{lock}{sig}<span class=\"hylki-pgp-text\"></span></button>\
                 <span class=\"hylki-sens\" data-key=\"{aid}:{id}\" title=\"\">{sens}\
                 <span class=\"hylki-sens-text\"></span></span>",
            aid = m.account_id,
            id = m.id,
            svg = inline_icon_svg("verified-checkmark-symbolic"),
            // The OpenPGP chip (#133): a lock for an encrypted
            // message, a shield for a signed one, then the
            // verdict in words (#300); hidden until the verdict
            // is patched in.
            lock = inline_icon_svg("channel-secure-symbolic"),
            sig = inline_icon_svg("security-high-symbolic"),
            // The sensitivity label (#411), hidden until a
            // label is patched in.
            sens = inline_icon_svg("tag-outline-symbolic"),
        ),
        // An initials circle, tinted per sender address, so who
        // wrote each card — and which cards are your own replies —
        // reads at a glance (#22). Nothing is linked: a picture
        // arrives as an embedded PNG, never a path the document
        // could reach for, and the initial is escaped like every
        // other header field.
        ava = ava_html,
        addr = if m.from_addr.is_empty() {
            String::new()
        } else {
            // `data-addr` feeds the hover reveal's ::after overlay;
            // `data-mail` (the bare address) drives click-to-compose
            // and the copy menu. Fully entity-escaped (&<>"), so an
            // attacker-controlled address can't smuggle markup
            // through the attributes.
            format!(
                "<span class=\"hylki-addr hylki-mail\" data-mail=\"{0}\" \
                     data-addr=\"&lt;{0}&gt;\">&lt;{0}&gt;</span>",
                escape_text(&m.from_addr).replace('"', "&quot;")
            )
        },
        date = escape_text(&m.datetime_full()),
        // Everyone the message went to, tucked behind a small chip so
        // a card's header stays one line tall until asked. Escaped
        // like the sender: recipient headers are attacker-controlled.
        rcpt_toggle = match recipient_count(m) {
            0 => String::new(),
            // With the recipients line always visible, a
            // single-recipient chip would only repeat it (#40).
            1 if always_show_recipients => String::new(),
            n => format!(
                "<button type=\"button\" class=\"hylki-rcpt-toggle{open}\" \
                     title=\"{title}\">{label}</button>",
                label = gtk::glib::markup_escape_text(&ni18n_f(
                    "{n} recipient",
                    "{n} recipients",
                    n as u32,
                    &[("n", &n.to_string())],
                )),
                open = if always_show_recipients { " open" } else { "" },
                title = if always_show_recipients {
                    i18n("Hide recipients")
                } else {
                    i18n("Show recipients")
                },
            ),
        },
        rcpt = recipients_html(m, always_show_recipients),
        // Where this message was read from, when that isn't the
        // folder on screen — the reply you sent, pulled in from Sent.
        folder = match folder_labels.get(&(m.account_id, m.id)) {
            Some(label) => format!("<span class=\"hylki-folder\">{}</span>", escape_text(label)),
            None => String::new(),
        },
        body = body,
        // Unread messages in a conversation are marked; the mark
        // clears when the user clicks the card to read it —
        // scrolling past no longer counts as reading.
        dot = if m.unread {
            format!(
                "<span class=\"hylki-dot\" data-key=\"{}:{}\"></span>",
                m.account_id, m.id
            )
        } else {
            String::new()
        },
    )
}

/// A card's action buttons, in the reader toolbar's order.
fn card_actions(cards: &Cards<'_>, m: &Message) -> String {
    let Cards {
        style,
        reader,
        sender_style,
        restrict,
        ..
    } = *cards;
    let key = (m.account_id, m.id);
    format!(
        "<span class=\"hylki-acts\">{}{}{}{}{}{}{}{}{}{}{}{}{}</span>",
        // Same order as the reader toolbar and the list's
        // actions palette, View Source closing the line.
        card_action_button(
            key,
            "reply",
            "mail-reply-sender-symbolic",
            &i18n("Reply to this message")
        ),
        card_action_button(
            key,
            "replyall",
            "mail-reply-all-symbolic",
            &i18n("Reply to everyone on this message")
        ),
        card_action_button(
            key,
            "forward",
            "mail-forward-symbolic",
            &i18n("Forward this message")
        ),
        // Action-showing icon (read envelope = "mark as
        // read"), like the menus and toolbar. Both icons
        // are baked in; the section's `unread` class picks
        // one, so marking read/unread flips in place.
        format!(
            "<button type=\"button\" class=\"hylki-act\" data-act=\"toggleread\" \
                 data-key=\"{aid}:{id}\" title=\"{title}\">\
                 <span class=\"tr-when-unread\">{read_svg}</span>\
                 <span class=\"tr-when-read\">{unread_svg}</span></button>",
            aid = key.0,
            id = key.1,
            title = gtk::glib::markup_escape_text(&i18n("Mark as read or unread")),
            read_svg = inline_icon_svg("hylki-mail-read-symbolic"),
            unread_svg = inline_icon_svg("mail-unread-symbolic"),
        ),
        // The star keeps one glyph; the starred state is
        // color alone (`.on`, toggled optimistically on
        // click too).
        format!(
            "<button type=\"button\" class=\"hylki-act{on}\" data-act=\"star\" \
                 data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
            on = if m.starred { " on" } else { "" },
            title = gtk::glib::markup_escape_text(&i18n("Star this message")),
            aid = key.0,
            id = key.1,
            svg = inline_icon_svg("hylki-non-starred-symbolic"),
        ),
        card_action_button(
            key,
            "moveto",
            "folder-symbolic",
            &i18n("Move this message to a folder")
        ),
        card_action_button(
            key,
            "archive",
            "mail-archive-symbolic",
            &i18n("Archive this message")
        ),
        card_action_button(
            key,
            "delete",
            "user-trash-symbolic",
            &i18n("Delete this message")
        ),
        card_action_button(
            key,
            "spam",
            "mail-mark-junk-symbolic",
            &i18n("Mark as Spam")
        ),
        card_action_button(
            key,
            "contact",
            "contact-new-symbolic",
            &i18n("Add sender to Contacts")
        ),
        format!(
            "{}{}",
            card_action_button(key, "viewsource", "code-symbolic", &i18n("View source")),
            if LIVE_TRANSLATE_ON.with(|on| on.get()) {
                card_action_button(
                    key,
                    "translate",
                    "translate-symbolic",
                    &i18n("Translate this message"),
                )
            } else {
                String::new()
            },
        ),
        // The escape from the reader's own fonts and
        // colors (#56): only offered while an override
        // is on, lit while this card shows the sender's.
        if style.active() && !reader {
            let on = sender_style.contains(&key);
            format!(
                "<button type=\"button\" class=\"hylki-act{on_cls}\" data-act=\"senderfmt\" \
                     data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
                on_cls = if on { " on" } else { "" },
                aid = key.0,
                id = key.1,
                title = gtk::glib::markup_escape_text(&if on {
                    i18n("Back to my fonts and colors")
                } else {
                    i18n("Show the sender's fonts and colors")
                }),
                svg = inline_icon_svg("format-text-rich-symbolic"),
            )
        } else {
            String::new()
        },
        // Remote content, per message. The banner offers
        // the same thing, but it can be switched off
        // (Settings → Privacy) and then there is nothing to
        // click — and it only ever lets content in, where
        // this puts the block back too. Offered only on a
        // card that actually has something to load.
        if has_remote_resources(&m.body) {
            format!(
                "<button type=\"button\" class=\"hylki-act{on_cls}\" data-act=\"remote\" \
                     data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
                on_cls = if restrict { "" } else { " on" },
                aid = key.0,
                id = key.1,
                title = gtk::glib::markup_escape_text(&if restrict {
                    i18n("Show remote content")
                } else {
                    i18n("Block remote content")
                }),
                svg = inline_icon_svg("image-x-generic-symbolic"),
            )
        } else {
            String::new()
        },
    )
}

/// The wrapper's stylesheet: the cards, their headers and chips, in the
/// reader's grounds and accent.
fn wrapper_css(
    scheme: &str,
    page: &str,
    bg: &str,
    chrome: &str,
    accent: &str,
    toggle_color: &str,
    plain_css: &str,
) -> String {
    format!(
        "\
         :root{{color-scheme:{scheme};}}\
         body{{margin:0;padding:0;background:{page};font:14px/1.55 system-ui,sans-serif;}}\
         body.hylki-conv{{padding:14px;}}\
         iframe.hylki-frame{{width:100%;border:0;display:block;background:{bg};\
         visibility:hidden;}}\
         iframe.hylki-frame.hylki-live{{visibility:visible;}}\
         .hylki-pan{{overflow-x:auto;position:relative;}}\
         iframe.hylki-frame.anim{{transition:height 240ms cubic-bezier(0.4,0,0.2,1);}}\
         @media (prefers-reduced-motion:reduce){{iframe.hylki-frame.anim{{transition:none;}}}}\
         .hylki-msg{{background:{bg};\
         border-radius:12px;overflow:hidden;margin:0 0 14px;}}\
         .hylki-msg:last-child{{margin-bottom:0;}}\
         .hylki-msg{{user-select:none;}}\
         body:not(.hylki-conv) .hylki-msg{{border-radius:0;margin:0;}}\
         .hylki-msg.selected{{box-shadow:0 0 0 2px {accent};}}\
         .hylki-fbar{{display:none;}}\
         .hylki-msg.hylki-folded>.hylki-fbar{{display:flex;align-items:flex-start;gap:12px;\
         padding:14px 18px;cursor:pointer;user-select:none;}}\
         .hylki-fbar-main{{flex:1 1 auto;min-width:0;display:flex;flex-direction:column;gap:2px;}}\
         .hylki-fbar-top{{display:flex;align-items:center;gap:12px;min-height:26px;}}\
         .hylki-fbar>.hylki-ava{{align-self:flex-start;}}\
         .hylki-fbar-prev{{display:-webkit-box;-webkit-box-orient:vertical;overflow:hidden;\
         opacity:0.6;font-size:0.92em;line-height:1.35;overflow-wrap:anywhere;}}\
         .hylki-fbar-from{{flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;\
         white-space:nowrap;opacity:0.85;}}\
         .hylki-msg.unread .hylki-fbar-from{{font-weight:700;opacity:1;}}\
         .hylki-fbar-ico{{flex:none;display:inline-flex;opacity:0.55;line-height:0;}}\
         .hylki-fbar-ico svg{{width:16px;height:16px;}}\
         .hylki-fbar-none{{width:16px;height:16px;}}\
         .hylki-fbar-ico svg,.hylki-fbar-ico svg *{{fill:currentColor;}}\
         .hylki-fbar-date{{flex:none;min-width:6.5em;text-align:end;opacity:0.7;\
         font-size:0.92em;white-space:nowrap;}}\
         .hylki-msg.hylki-folded>:not(.hylki-fbar){{display:none;}}\
         body.hylki-folds .hylki-msg-hdr{{cursor:pointer;}}\
         @media print{{.hylki-msg.hylki-folded>:not(.hylki-fbar){{display:revert !important;}}\
         .hylki-msg.hylki-folded>.hylki-fbar{{display:none !important;}}}}\
         .hylki-msg-hdr{{cursor:pointer;}}\
         .hylki-msg-hdr{{padding:12px 16px;cursor:default;user-select:none;\
         position:sticky;top:0;z-index:1;background-color:{bg};}}\
         /* 8px between items on a line, but only 2px between wrapped\
         lines — the flex gap otherwise pads rows apart too. */\
         .hylki-hdr-line{{display:flex;column-gap:8px;row-gap:2px;\
         align-items:baseline;flex-wrap:nowrap;\
         min-width:0;position:relative;}}\
         .hylki-hdr-meta{{display:flex;gap:8px;align-items:baseline;flex:none;\
         margin-inline-start:auto;}}\
         /* Narrow pane: the meta group (folder chip, recipients chip, date)\
         drops to its own line as one unit — no mid-text wrapping — while\
         the sender and the action palette hold the first line. */\
         @media (max-width:620px){{\
         /* Tighten the right-hand insets (card gutter + header padding)\
         so the palette toggle sits nearer the pane's edge — but the\
         combined inset never drops below 20px. */\
         body.hylki-conv{{padding:10px;}}\
         body.hylki-conv .hylki-msg-hdr{{padding:12px 10px;}}\
         .hylki-hdr-line{{flex-wrap:wrap;}}\
         .hylki-hdr-meta{{order:10;flex-basis:100%;margin-inline-start:34px;\
         justify-content:flex-start;}}\
         /* Below the wrapped meta line the recipients need a touch\
         more air than the wide layout's 2px. */\
         .hylki-rcpt{{margin-top:6px;}}\
         /* With the meta group gone from the first line, the palette\
         pins itself to the corner (the \u{22ef} is already absolute). */\
         body:not([data-hylki-acts=\"toggle\"]) .hylki-acts{{margin-inline-start:auto;}}\
         }}\
         body:not(.hylki-conv) .hylki-msg-hdr{{background-color:{chrome};}}\
         {plain_css}\
         .hylki-ava{{width:26px;height:26px;border-radius:50%;flex:none;align-self:center;\
         display:flex;align-items:center;justify-content:center;color:#fff;\
         font-size:0.8em;font-weight:700;}}\
         /* Zero flex-basis: the name and address only absorb free space\
         (up to their natural width), so their length can never force\
         the palette onto another line. Under pressure the address\
         gives way first (tiny grow share), fading out at its right\
         edge; the name ellipsizes only after that. */\
         .hylki-from{{font-weight:700;flex:99 1 0;max-width:max-content;min-width:0;\
         white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}}\
         .hylki-tags{{display:inline-flex;gap:4px;align-items:center;flex:0 0 auto;}}\
         .hylki-tags:empty{{display:none;}}\
         /* A card's own attachments (#213): chips beneath the body. */\
         .hylki-atts{{display:flex;flex-wrap:wrap;gap:6px;padding:2px 14px 12px;}}\
         .hylki-atts:empty{{display:none;}}\
         /* The Unsubscribe banner between a list message's header and\
         body: a line of text and one button. */\
         .hylki-tr{{display:flex;align-items:center;gap:10px;padding:8px 14px 9px;\
         font-size:0.9em;border-bottom:1px solid rgba(128,128,128,0.25);}}\
         .hylki-tr:empty{{display:none;}}\
         .hylki-tr-text{{flex:1 1 auto;min-width:0;opacity:0.8;}}\
         .hylki-tr-btn{{flex:none;border:none;border-radius:6px;\
         padding:3px 12px;background:color-mix(in srgb,currentColor 10%,transparent);\
         color:inherit;font:inherit;font-size:0.95em;font-weight:600;cursor:pointer;}}\
         .hylki-tr-btn:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
         .hylki-tr-btn:active{{background:color-mix(in srgb,currentColor 30%,transparent);}}\
         .hylki-unsub{{display:flex;align-items:center;gap:10px;padding:8px 14px 9px;\
         font-size:0.9em;border-bottom:1px solid rgba(128,128,128,0.25);}}\
         .hylki-unsub:empty{{display:none;}}\
         .hylki-unsub-text{{flex:1 1 auto;min-width:0;opacity:0.8;}}\
         .hylki-unsub-btn{{flex:none;border:none;border-radius:6px;\
         padding:3px 12px;background:color-mix(in srgb,currentColor 10%,transparent);\
         color:inherit;font:inherit;font-size:0.95em;font-weight:600;cursor:pointer;}}\
         .hylki-unsub-btn:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
         .hylki-unsub-btn:active{{background:color-mix(in srgb,currentColor 30%,transparent);}}\
         /* The invitation banner (#223): the meeting at the top of\
         the card, and the answer buttons under it. */\
         .hylki-inv{{display:flex;gap:10px;padding:10px 14px 11px;font-size:0.9em;\
         border-bottom:1px solid rgba(128,128,128,0.25);}}\
         .hylki-inv:empty{{display:none;}}\
         .hylki-inv-icon{{flex:none;padding-top:1px;opacity:0.7;}}\
         .hylki-inv-icon svg{{width:20px;height:20px;display:block;}}\
         .hylki-inv-icon svg,.hylki-inv-icon svg *{{fill:currentColor;}}\
         .hylki-inv-body{{flex:1 1 auto;min-width:0;}}\
         .hylki-inv-head{{display:flex;align-items:baseline;gap:8px;flex-wrap:wrap;}}\
         .hylki-inv-title{{font-weight:700;font-size:1.05em;}}\
         .hylki-inv-title.off{{text-decoration:line-through;opacity:0.7;}}\
         .hylki-inv-kind{{font-size:0.85em;text-transform:uppercase;\
         letter-spacing:0.04em;opacity:0.6;}}\
         .hylki-inv-row{{opacity:0.8;margin-top:2px;}}\
         .hylki-inv-note{{margin-top:4px;opacity:0.8;font-style:italic;}}\
         .hylki-inv-acts{{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px;}}\
         .hylki-inv-btn{{border:1px solid rgba(128,128,128,0.4);border-radius:6px;\
         padding:3px 12px;background:rgba(128,128,128,0.1);color:inherit;font:inherit;\
         font-size:0.95em;font-weight:600;cursor:pointer;}}\
         .hylki-inv-btn:hover{{background:rgba(128,128,128,0.22);}}\
         /* The answer already given is marked, so a meeting opened\
         again shows where it stands at a glance. */\
         .hylki-inv-btn.on{{border-color:{accent};color:{accent};\
         background:color-mix(in srgb, {accent} 16%, transparent);}}\
         .hylki-attw{{display:inline-flex;align-items:stretch;max-width:100%;\
         border:1px solid rgba(128,128,128,0.35);border-radius:8px;overflow:hidden;\
         background:rgba(128,128,128,0.08);}}\
         .hylki-attc{{display:inline-flex;align-items:center;gap:6px;padding:4px 8px;\
         border:none;background:none;color:inherit;font:inherit;font-size:0.85em;\
         cursor:pointer;min-width:0;max-width:320px;}}\
         .hylki-attc:hover{{background:rgba(128,128,128,0.18);}}\
         .hylki-attc svg{{width:16px;height:16px;flex:none;display:block;}}\
         .hylki-attc svg,.hylki-attc svg *{{fill:currentColor;}}\
         .hylki-attc.ftype-pdf svg{{color:#e01b24;}}\
         .hylki-attc.ftype-doc svg{{color:#3584e4;}}\
         .hylki-attc.ftype-sheet svg{{color:#2ec27e;}}\
         .hylki-attc.ftype-slides svg{{color:#ff7800;}}\
         .hylki-attc.ftype-archive svg{{color:#e5a50a;}}\
         .hylki-attc.ftype-audio svg{{color:#9141ac;}}\
         .hylki-attc.ftype-video svg{{color:#e6689e;}}\
         .hylki-attc.ftype-calendar svg{{color:#009e8f;}}\
         .hylki-attc.ftype-image svg{{color:#1c9bd8;}}\
         .hylki-attc.ftype-generic svg{{color:#8c8c8c;}}\
         .hylki-attn{{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0;}}\
         .hylki-attz{{opacity:0.55;white-space:nowrap;flex:none;}}\
         .hylki-attsave{{display:inline-flex;align-items:center;padding:0 7px;border:none;\
         border-inline-start:1px solid rgba(128,128,128,0.35);background:none;color:inherit;\
         cursor:pointer;opacity:0.6;}}\
         .hylki-attsave:hover{{opacity:1;background:rgba(128,128,128,0.18);}}\
         .hylki-attsave svg{{width:14px;height:14px;display:block;}}\
         .hylki-attsave svg,.hylki-attsave svg *{{fill:currentColor;}}\
         .hylki-atts.flash .hylki-attw{{animation:hylki-att-flash 1.2s ease-out;}}\
         @keyframes hylki-att-flash{{0%,40%{{box-shadow:0 0 0 3px rgba(53,132,228,0.7);}}\
         100%{{box-shadow:0 0 0 3px rgba(53,132,228,0);}}}}\
         .hylki-tag{{font-size:0.72em;font-weight:700;line-height:1.4;padding:1px 7px;\
         border-radius:9999px;white-space:nowrap;}}\
         .hylki-label{{max-width:16em;\
         overflow:hidden;text-overflow:ellipsis;}}\
         .hylki-addr{{opacity:0.55;font-size:0.9em;flex:1 1 0;max-width:max-content;\
         min-width:0;white-space:nowrap;overflow:hidden;text-overflow:clip;\
         position:relative;}}\
         /* The fade exists only while something actually collides with\
         the address (`clipped` is set by measurement, below). */\
         .hylki-addr.clipped{{\
         -webkit-mask-image:linear-gradient(to right,#000 calc(100% - 18px),transparent);\
         mask-image:linear-gradient(to right,#000 calc(100% - 18px),transparent);}}\
         /* Hovering a clipped address reveals it in full: an overlay\
         pinned to the span's spot, floating over whatever crowded it\
         out, gone when the cursor leaves. */\
         .hylki-addr.clipped:hover{{-webkit-mask-image:none;mask-image:none;\
         z-index:6;}}\
         /* Addresses act as links: click composes to them, right-click\
         offers Copy / New Message. */\
         /* Sender-authentication seal (#88): invisible until a\
         verdict arrives; a pass wears Bazaar's fixed blue (never the\
         system accent), problems the warning/error reds.\
         Sized in em and anchored to the text baseline, then eased\
         down to the name's optical centre with an em transform\
         (margins can't move it: WebKit synthesizes the missing\
         baseline before they apply) — a fixed centred pixel box sat\
         visibly low whenever a GNOME text scaling factor shrank the\
         type around it. */\
         .hylki-verify{{display:none;background:none;border:none;\
         padding:0 2px;margin-inline-start:2px;cursor:pointer;line-height:0;\
         align-self:baseline;transform:translateY(0.18em);flex:none;}}\
         .hylki-verify.on{{display:inline-flex;}}\
         .hylki-verify svg{{width:0.95em;height:0.95em;display:block;}}\
         .hylki-verify svg,.hylki-verify svg *{{fill:currentColor;}}\
         .hylki-verify.trust-pass{{color:#3584e4;}}\
         .hylki-verify.trust-unverified{{color:currentColor;opacity:0.4;}}\
         .hylki-verify.trust-suspicious{{color:#cd9309;}}\
         .hylki-verify.trust-fail{{color:#c01c28;}}\
         /* The OpenPGP chip (#133), a labelled pill since #300: the\
         verdict reads in words on a wash of its color. Text of its\
         own, so it sits on the name's baseline without the seal's\
         nudge. */\
         .hylki-pgp{{display:none;font:inherit;font-size:0.78em;font-weight:600;\
         border:none;border-radius:999px;padding-block:0.1em;padding-inline:0.45em 0.6em;\
         margin-inline-start:4px;cursor:pointer;align-items:center;gap:0.3em;\
         align-self:baseline;white-space:nowrap;flex:none;color:inherit;\
         background:rgba(128,128,128,0.16);}}\
         .hylki-pgp.on{{display:inline-flex;}}\
         .hylki-pgp svg{{width:1.1em;height:1.1em;display:none;flex:none;}}\
         .hylki-pgp svg,.hylki-pgp svg *{{fill:currentColor;}}\
         .hylki-pgp.enc svg:first-child{{display:block;}}\
         .hylki-pgp.sig svg:nth-child(2){{display:block;}}\
         .hylki-pgp.pgp-good{{color:#26a269;background:rgba(38,162,105,0.16);}}\
         .hylki-pgp.pgp-warn{{color:#cd9309;background:rgba(205,147,9,0.16);}}\
         .hylki-pgp.pgp-bad{{color:#c01c28;background:rgba(192,28,40,0.14);}}\
         .hylki-pgp:hover{{filter:brightness(1.1);}}\
         /* The sensitivity label (#411): the OpenPGP pill's shape in a\
         neutral wash, since a label is a statement, not a verdict.\
         Zero flex-basis and a small share, like the address: the\
         name fills first and a long label ellipsizes before it. */\
         .hylki-sens{{display:none;font-size:0.78em;font-weight:600;\
         border-radius:999px;padding-block:0.1em;padding-inline:0.45em 0.6em;\
         margin-inline-start:4px;align-items:center;gap:0.3em;\
         align-self:baseline;white-space:nowrap;flex:1 1 0;min-width:3.5em;max-width:max-content;\
         background:rgba(128,128,128,0.16);cursor:default;}}\
         .hylki-sens.on{{display:inline-flex;}}\
         .hylki-sens svg{{width:1em;height:1em;flex:none;}}\
         .hylki-sens svg,.hylki-sens svg *{{fill:var(--hylki-sens,currentColor);}}\
         .hylki-sens-text{{overflow:hidden;text-overflow:ellipsis;}}\
         /* The icons alone (Settings, #300): the chip as it was before\
         it had words, sitting on the name's line like the seal. */\
         body[data-hylki-pgpicons] .hylki-pgp{{background:none;padding:0 2px;\
         margin-inline-start:2px;gap:1px;font-size:inherit;line-height:0;\
         transform:translateY(0.18em);}}\
         body[data-hylki-pgpicons] .hylki-pgp svg{{width:0.95em;height:0.95em;}}\
         body[data-hylki-pgpicons] .hylki-pgp-text{{display:none;}}\
         .hylki-mail{{cursor:pointer;}}\
         .hylki-mail:hover{{text-decoration:underline;}}\
         /* Styled after the app's own context menus (context_menu.rs +\
         .context-menu-list in styles.css): same padding, min width,\
         row shape and weight, so both menu families read as one. */\
         .hylki-mailmenu-scrim{{position:fixed;left:0;top:0;right:0;bottom:0;\
         z-index:98;}}\
         .hylki-mailmenu{{position:fixed;z-index:99;background:{bg};\
         border:1px solid rgba(128,128,128,0.2);border-radius:12px;\
         box-shadow:0 6px 24px rgba(0,0,0,0.22);padding:6px;\
         min-width:190px;font-size:0.95em;}}\
         .hylki-mailmenu button{{display:block;width:100%;text-align:start;\
         background:none;border:none;padding:8px 10px;border-radius:6px;\
         color:inherit;cursor:pointer;font:inherit;font-weight:normal;}}\
         .hylki-mailmenu button:hover{{background:rgba(128,128,128,0.14);}}\
         .hylki-addr.clipped:hover::after{{content:attr(data-addr);\
         position:absolute;inset-inline-start:-6px;top:50%;transform:translateY(-50%);\
         background:{bg};border-radius:6px;padding:1px 6px;\
         white-space:nowrap;text-decoration:underline;}}\
         body:not(.hylki-conv) .hylki-addr.clipped:hover::after{{\
         background:{chrome};}}\
         .hylki-date{{opacity:0.55;font-size:0.85em;flex:none;white-space:nowrap;}}\
         .hylki-dot{{width:8px;height:8px;border-radius:50%;background:#3584e4;\
         flex:none;align-self:center;}}\
         .hylki-end{{height:1px;}}\
         .hylki-quote{{display:block;margin:0 16px 12px;padding:0 7px;\
         font:inherit;font-size:0.7em;line-height:1.45;letter-spacing:0.06em;\
         color:inherit;opacity:0.6;background:rgba(128,128,128,0.16);\
         border:0;border-radius:999px;cursor:pointer;}}\
         .hylki-quote:hover{{opacity:0.95;background:rgba(128,128,128,0.28);}}\
         .hylki-quote.open{{opacity:0.95;position:absolute;inset-inline-start:0;z-index:2;}}\
         .hylki-acts{{display:flex;gap:2px;flex:none;align-self:center;margin-inline-start:4px;}}\
         /* Read-toggle: the icon showing is the ACTION (read envelope\
         means mark-as-read); the section's unread class decides. */\
         .hylki-act .tr-when-unread,.hylki-act .tr-when-read{{display:none;line-height:0;}}\
         .hylki-msg.unread .hylki-act[data-act=\"toggleread\"] .tr-when-unread{{display:inline-flex;}}\
         .hylki-msg:not(.unread) .hylki-act[data-act=\"toggleread\"] .tr-when-read{{display:inline-flex;}}\
         .hylki-act{{color:inherit;background:none;border:none;border-radius:6px;\
         padding:5px 8px;cursor:pointer;opacity:0.7;\
         display:inline-flex;align-items:center;justify-content:center;\
         vertical-align:middle;line-height:0;\
         transition:opacity 120ms ease,background 120ms ease;}}\
         .hylki-act:hover{{opacity:1;background:rgba(128,128,128,0.18);}}\
         .hylki-act:active{{background:rgba(128,128,128,0.3);}}\
         .hylki-act svg{{width:14px;height:14px;display:block;}}\
         .hylki-act svg,.hylki-act svg *{{fill:currentColor;}}\
         /* A set star: the one state that carries color. */\
         .hylki-act.on{{color:#e5a50a;opacity:1;}}\
         .hylki-acts-toggle{{display:none;}}\
         /* Behind-the-\u{22ef} mode: the palette overlays the header line as an\
         absolute strip anchored at the toggle, expanding leftward over\
         the text beneath (never pushing it), its left edge fading in\
         from transparent. The \u{22ef} itself never moves. */\
         body[data-hylki-acts=\"toggle\"] .hylki-acts{{position:absolute;\
         inset-inline-end:34px;top:13px;transform:translateY(-50%) scaleX(0.6);\
         transform-origin:right center;margin:0;\
         padding-block:2px;padding-inline:30px 6px;border-radius:8px;\
         background:linear-gradient(to right,transparent,{bg} 26px);\
         opacity:0;pointer-events:none;\
         transition:opacity 140ms ease,transform 180ms ease;}}\
         body[data-hylki-acts=\"toggle\"] .hylki-acts.open{{opacity:1;\
         pointer-events:auto;transform:translateY(-50%) scaleX(1);}}\
         body:not(.hylki-conv)[data-hylki-acts=\"toggle\"] .hylki-acts{{\
         background:linear-gradient(to right,transparent,{chrome} 26px);}}\
         /* Always visible (not just on card hover), and OUT OF FLOW:\
         absolutely pinned to the header's right edge, so no amount of\
         header content can push or wrap it. The line reserves the\
         button's footprint as fixed right padding instead. */\
         body[data-hylki-acts=\"toggle\"] .hylki-hdr-line{{padding-inline-end:38px;}}\
         /* Anchored to the FIRST row's centre (the 26px avatar/sender\
         line), not the line box's — when the meta row wraps beneath,\
         the box grows downward but the button must not move. */\
         body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle{{display:block;\
         position:absolute;inset-inline-end:0;top:13px;transform:translateY(-50%);\
         color:{toggle_color};background:none;border:none;border-radius:6px;\
         padding:4px 8px;margin:0;cursor:pointer;\
         opacity:0.65;transition:opacity 120ms ease,background 120ms ease;}}\
         /* `svg *` too: the embedded symbolic paths carry their own fill\
         attribute, which a rule on the svg element alone can't beat. */\
         .hylki-acts-toggle svg{{width:14px;height:14px;display:block;}}\
         .hylki-acts-toggle svg,.hylki-acts-toggle svg *{{fill:currentColor;}}\
         body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle:hover{{opacity:1;\
         background:rgba(128,128,128,0.18);}}\
         body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle.open{{opacity:1;}}\
         body[data-hylki-acts=\"hover\"] .hylki-acts{{opacity:0;pointer-events:none;\
         transition:opacity 300ms ease var(--acts-delay,0ms);}}\
         body[data-hylki-acts=\"hover\"] .hylki-msg:hover .hylki-acts{{opacity:1;\
         pointer-events:auto;transition:opacity 300ms ease 0ms;}}\
         .hylki-folder{{padding:0.05em 0.45em;border-radius:0.7em;\
         font-size:0.78em;opacity:0.75;border:1px solid currentColor;\
         white-space:nowrap;flex:none;}}\
         .hylki-rcpt-toggle{{font:inherit;font-size:0.78em;color:inherit;\
         background:color-mix(in srgb,currentColor 10%,transparent);\
         border:none;border-radius:6px;padding:0.15em 0.6em;cursor:pointer;\
         white-space:nowrap;flex:none;transition:background 120ms ease;}}\
         .hylki-rcpt-toggle:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
         .hylki-rcpt-toggle.open{{background:color-mix(in srgb,currentColor 20%,transparent);}}\
         /* Indented past the avatar (26px + 8px gap), so the recipients\
         align with the sender's name — as does the wrapped meta line. */\
         .hylki-rcpt{{font-size:0.85em;opacity:0.75;\
         user-select:text;overflow-wrap:anywhere;margin:2px 0 0;margin-inline-start:34px;}}\
         .hylki-rcpt div{{margin-top:2px;}}\
         .hylki-loading{{opacity:0.5;padding:16px;}}\
         /* The Copied pill after Ctrl+C: a toast the page draws itself,\
         since the reader has no toast overlay of its own. */\
         .hylki-copied{{position:fixed;left:50%;bottom:22px;transform:translateX(-50%) translateY(8px);\
         padding:8px 18px;border-radius:999px;background:rgba(40,40,40,0.92);color:#fff;\
         font:inherit;font-size:0.9em;box-shadow:0 2px 10px rgba(0,0,0,0.25);\
         opacity:0;pointer-events:none;transition:opacity 180ms ease,transform 180ms ease;z-index:50;}}\
         .hylki-copied.on{{opacity:1;transform:translateX(-50%) translateY(0);}}\
         /* Right to left (#366): what logical properties cannot say. */\
         html[dir=rtl] .hylki-addr.clipped{{\
         -webkit-mask-image:linear-gradient(to left,#000 calc(100% - 18px),transparent);\
         mask-image:linear-gradient(to left,#000 calc(100% - 18px),transparent);}}\
         html[dir=rtl] .hylki-addr.clipped:hover{{-webkit-mask-image:none;mask-image:none;}}\
         html[dir=rtl] body[data-hylki-acts=\"toggle\"] .hylki-acts{{\
         transform-origin:left center;\
         background:linear-gradient(to left,transparent,{bg} 26px);}}\
         html[dir=rtl] body:not(.hylki-conv)[data-hylki-acts=\"toggle\"] .hylki-acts{{\
         background:linear-gradient(to left,transparent,{chrome} 26px);}}\
         /* Each chip's words in their own direction, as a GTK label\
         has them: an English 1 recipient in a right-to-left line\
         otherwise reads recipient 1. */\
         html[dir=rtl] :is(.hylki-rcpt-toggle,.hylki-date,.hylki-fbar-date,\
         .hylki-folder,.hylki-rcpt div){{unicode-bidi:plaintext;}}\
         html[dir=rtl] .hylki-fbar-date{{text-align:left;}}\
         "
    )
}
