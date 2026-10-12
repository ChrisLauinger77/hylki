//! Exchange Web Services (#391): on-premise Exchange, signed in to with
//! NTLM or Basic over HTTPS, its requests and replies typed by the `ews`
//! crate.

#![allow(dead_code)]

mod autodiscover;
mod client;
mod http;
mod ntlm;
mod ops;
mod push;
mod run;

pub(super) use run::{run_ews, test_ews};
