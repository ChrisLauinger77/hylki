//! Reader-first ordering: the loads the reader waits on may overtake list
//! syncs and refresh ticks, never a mutation, and may interrupt an in-flight
//! network read when nothing but reads is queued.

use tokio::sync::mpsc;

use crate::worker::MailRequest;

/// A request that puts mail in front of the reader: what the user is
/// waiting on right now.
fn is_reader_load(req: &MailRequest) -> bool {
    matches!(
        req,
        MailRequest::LoadBody { .. }
            | MailRequest::LoadBodies { .. }
            | MailRequest::LoadSource { .. }
            | MailRequest::LoadAttachments { download: true, .. }
            | MailRequest::ExportRaw {
                for_reader: true,
                ..
            }
    )
}

/// A folder list fetch: work the reader's loads may go ahead of.
fn is_list_load(req: &MailRequest) -> bool {
    matches!(
        req,
        MailRequest::LoadMessages { .. } | MailRequest::SyncFolder { .. }
    )
}

/// Refresh ticks now only schedule idle work, so they are no longer a
/// barrier between a reader and a folder sync already in the backlog.
fn can_yield_to_reader(req: &MailRequest) -> bool {
    is_list_load(req) || matches!(req, MailRequest::RefreshUnread)
}

/// Take everything else already queued behind `first` and move the reader's
/// loads ahead of the list syncs they sit behind, keeping every other order.
///
/// The worker serves one request at a time, so a message opened right after
/// a view that loads its folder (a notification click into Inboxes, which
/// asks every account for its inbox, then opens the mail) waited for the
/// whole list fetch — and the IDLE hand-off before it — before its body was
/// even asked for. The body is what the user is looking at; the list can
/// follow. A reader load overtakes lists and deferred refresh ticks, never a
/// move, flag change or reconnect, so what it reads is what the
/// requests before it left.
pub(super) fn reorder_reader_loads(
    first: MailRequest,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    backlog: &mut std::collections::VecDeque<MailRequest>,
) -> MailRequest {
    let mut queue: Vec<MailRequest> = Vec::with_capacity(backlog.len() + 2);
    queue.push(first);
    queue.extend(backlog.drain(..));
    while let Ok(req) = rx.try_recv() {
        queue.push(req);
    }
    if queue.len() > 1 {
        let mut ordered: Vec<MailRequest> = Vec::with_capacity(queue.len());
        for req in queue {
            if is_reader_load(&req) {
                // Slot in ahead of list fetches and deferred refresh ticks.
                let mut at = ordered.len();
                while at > 0 && can_yield_to_reader(&ordered[at - 1]) {
                    at -= 1;
                }
                ordered.insert(at, req);
            } else {
                ordered.push(req);
            }
        }
        queue = ordered;
    }
    let mut it = queue.into_iter();
    let first = it.next().expect("queue holds at least the request taken");
    backlog.extend(it);
    first
}

/// Let a reader interrupt network reads, without overtaking queued mutations.
/// Callers must own the session inside `work`, so cancellation drops a partially
/// consumed IMAP response, and restore the interrupted job for a later retry.
/// The reader then reconnects rather than reusing that response stream.
pub(super) async fn interrupt_read_for_reader<F: std::future::Future>(
    work: F,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    backlog: &mut std::collections::VecDeque<MailRequest>,
) -> Option<F::Output> {
    tokio::pin!(work);
    let mut closed = false;
    loop {
        tokio::select! {
            result = &mut work => return Some(result),
            req = rx.recv(), if !closed => {
                let Some(req) = req else { closed = true; continue; };
                let interrupt = is_reader_load(&req) && backlog.iter().all(|r| {
                    is_reader_load(r) || can_yield_to_reader(r)
                });
                backlog.push_back(req);
                if interrupt {
                    return None;
                }
            }
        }
    }
}
