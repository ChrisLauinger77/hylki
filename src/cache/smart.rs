//! Smart folders and tag views: scans across an account's cached mail.

use rusqlite::params;

use super::{split_keywords, split_labels, Cache, KEYWORDS_COL, LABEL_SEP};
use crate::models::Message;

/// Most messages one tag view lists per account; the list pages within it.
const TAG_VIEW_LIMIT: i64 = 5000;

impl Cache {
    /// The account's cached messages a smart folder (#417) keeps, newest
    /// first with the folder each sits in: those in `include` (every folder
    /// when `None`) and not in `exclude`, past what `pre` rules out, for
    /// which `keep` says yes. Rows are read one at a time and only the kept
    /// ones become messages, so a folder over a large index stays cheap to
    /// hold; reading stops at `limit` kept.
    pub fn smart_scan(
        &self,
        account_id: u32,
        include: Option<&[String]>,
        exclude: &[String],
        pre: crate::config::SmartPrefilter,
        limit: usize,
        mut keep: impl FnMut(&Message) -> bool,
    ) -> Vec<(String, Message)> {
        use rusqlite::types::Value;
        let mut args: Vec<Value> = vec![Value::Integer(account_id.into())];
        let list = |paths: &[String], args: &mut Vec<Value>| -> String {
            let marks: Vec<String> = paths
                .iter()
                .map(|p| {
                    args.push(Value::Text(p.clone()));
                    format!("?{}", args.len())
                })
                .collect();
            marks.join(",")
        };
        let mut sql = format!(
            "SELECT folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, \
                    has_attachment, recipients, cc, message_id, references_, preview, reply_to, {KEYWORDS_COL}, importance, due, gmail_labels, answered, forwarded, bcc \
             FROM messages WHERE account_id = ?1"
        );
        if let Some(paths) = include {
            if paths.is_empty() {
                return Vec::new();
            }
            sql.push_str(&format!(" AND folder_path IN ({})", list(paths, &mut args)));
        }
        if !exclude.is_empty() {
            sql.push_str(&format!(
                " AND folder_path NOT IN ({})",
                list(exclude, &mut args)
            ));
        }
        for (col, want) in [
            ("starred", pre.starred),
            ("unread", pre.unread),
            ("has_attachment", pre.attachment),
        ] {
            if let Some(want) = want {
                sql.push_str(&format!(" AND {col} = {}", i32::from(want)));
            }
        }
        sql.push_str(" ORDER BY ts DESC");
        let mut run = || -> rusqlite::Result<Vec<(String, Message)>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(rusqlite::params_from_iter(args.iter()))?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let uid: u32 = row.get(1)?;
                let mut m = Message {
                    id: uid,
                    account_id,
                    folder_id: 0, // filled in by the caller, which knows the ids
                    uid,
                    from_name: row.get(2)?,
                    from_addr: row.get(3)?,
                    reply_to: row.get(15)?,
                    to: row.get(10)?,
                    cc: row.get(11)?,
                    bcc: row.get(22)?,
                    subject: row.get(4)?,
                    preview: row.get(14)?,
                    body: String::new(),
                    date: row.get(5)?,
                    timestamp: row.get(6)?,
                    unread: row.get(7)?,
                    starred: row.get(8)?,
                    answered: row.get(20)?,
                    forwarded: row.get(21)?,
                    keywords: split_keywords(row.get(16)?),
                    has_attachment: row.get(9)?,
                    message_id: row.get(12)?,
                    references: row.get(13)?,
                    importance: crate::models::Importance::from_i64(row.get(17)?),
                    due: row.get(18)?,
                    labels: split_labels(row.get(19)?),
                };
                m.scrub_nuls();
                if keep(&m) {
                    out.push((row.get::<_, String>(0)?, m));
                    if out.len() >= limit {
                        break;
                    }
                }
            }
            Ok(out)
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache smart_scan failed: {e}");
            Vec::new()
        })
    }

    /// Every cached message of the account carrying `keyword` — on the server
    /// or locally — newest first, with the folder each sits in. Backs the
    /// sidebar's tag views (#71); the caller maps paths to folder ids and
    /// drops the copies Gmail keeps per label. `name` is the tag's name: on
    /// Gmail a label named like the tag (or like its keyword) counts as the
    /// tag too (#374).
    pub fn messages_with_keyword(
        &self,
        account_id: u32,
        keyword: &str,
        name: &str,
    ) -> Vec<(String, Message)> {
        let needle = format!(" {} ", keyword.to_ascii_lowercase());
        let label = |s: &str| {
            format!(
                "{sep}{}{sep}",
                crate::mutf7::encode(s).to_lowercase(),
                sep = LABEL_SEP
            )
        };
        let sql = format!(
            "SELECT folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, \
                    has_attachment, recipients, cc, message_id, references_, preview, reply_to, {KEYWORDS_COL}, importance, due, gmail_labels, answered, forwarded, bcc \
             FROM messages \
             WHERE account_id = ?1 AND (instr(' ' || lower({KEYWORDS_COL}) || ' ', ?2) > 0 \
                   OR (gmail_labels <> '' AND (instr(?4 || lower(gmail_labels) || ?4, ?5) > 0 \
                                               OR instr(?4 || lower(gmail_labels) || ?4, ?6) > 0))) \
             ORDER BY ts DESC LIMIT ?3"
        );
        let run = || -> rusqlite::Result<Vec<(String, Message)>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![
                    account_id,
                    needle,
                    TAG_VIEW_LIMIT,
                    LABEL_SEP.to_string(),
                    label(keyword),
                    label(name)
                ],
                |row| {
                    let uid: u32 = row.get(1)?;
                    let mut m = Message {
                        id: uid,
                        account_id,
                        folder_id: 0, // filled in by the caller, which knows the ids
                        uid,
                        from_name: row.get(2)?,
                        from_addr: row.get(3)?,
                        reply_to: row.get(15)?,
                        to: row.get(10)?,
                        cc: row.get(11)?,
                        bcc: row.get(22)?,
                        subject: row.get(4)?,
                        preview: row.get(14)?,
                        body: String::new(),
                        date: row.get(5)?,
                        timestamp: row.get(6)?,
                        unread: row.get(7)?,
                        starred: row.get(8)?,
                        answered: row.get(20)?,
                        forwarded: row.get(21)?,
                        keywords: split_keywords(row.get(16)?),
                        has_attachment: row.get(9)?,
                        message_id: row.get(12)?,
                        references: row.get(13)?,
                        importance: crate::models::Importance::from_i64(row.get(17)?),
                        due: row.get(18)?,
                        labels: split_labels(row.get(19)?),
                    };
                    m.scrub_nuls();
                    Ok((row.get::<_, String>(0)?, m))
                },
            )?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache messages_with_keyword failed: {e}");
            Vec::new()
        })
    }
}
