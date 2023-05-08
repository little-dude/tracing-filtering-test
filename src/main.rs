#![allow(warnings)]
#[macro_use]
extern crate tracing;

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;

use tracing::field::Field;
use tracing::field::ValueSet;
use tracing::field::Visit;
use tracing::span::Attributes;
use tracing::subscriber::Interest;
use tracing::Id;
use tracing::Metadata;
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::reload;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

mod router;

fn main() {
    // Construct a reloadable layer that filters span based on field
    // values. The handle will be passed to the `handle_tcp_client`,
    // so that the fields to filter on can be read from a TCP
    // connection
    let (field_filter, handle) = reload::Layer::new(DynamicFieldFilter::default());

    let fmt_subcriber = tracing_subscriber::fmt()
        .compact()
        .with_line_number(true)
        .with_ansi(false)
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    // Compose the fmt subscriber with out custom layer
    let subcriber = fmt_subcriber.with(field_filter);

    // Install the subscriber
    subcriber.init();

    // Start listening for incoming TCP connections. Clients should be
    // able to specify fields they want to filter on.
    thread::spawn(move || {
        let listener = TcpListener::bind("127.0.0.1:8888").unwrap();
        for stream in listener.incoming() {
            handle_tcp_client(stream.unwrap(), handle.clone());
        }
    });

    // Start our fake router so that we start logging stuff
    let (tx, rx) = mpsc::channel();
    let bgp = router::Bgp::new(rx);
    let rib = router::Rib::new(tx);
    thread::spawn(move || bgp.run());
    rib.run();
}

struct MatchStrVisitor<'a> {
    field: &'a str,
    value: &'a str,
    matched: bool,
}

impl Visit for MatchStrVisitor<'_> {
    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == self.field && value == self.value {
            self.matched = true;
        }
    }
}

/// Return `true` if the value set contains the given field with the
/// given value.
fn value_in_valueset(valueset: &ValueSet<'_>, field: &str, value: &str) -> bool {
    let mut visitor = MatchStrVisitor {
        field,
        value,
        matched: false,
    };
    valueset.record(&mut visitor);
    visitor.matched
}

/// A layer that checks filters spans based their fields values
#[derive(Debug, Default)]
struct DynamicFieldFilter {
    filters: HashMap<String, String>,
}

/// A span extension that indicates that the span is disabled
struct SpanExtDisable;

impl<S> Layer<S> for DynamicFieldFilter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn enabled(&self, _metadata: &Metadata<'_>, ctx: Context<'_, S>) -> bool {
        panic!("enabled");
        if let Some(span_ref) = ctx.lookup_current() {
            span_ref.extensions().get::<SpanExtDisable>().is_none()
        } else {
            true
        }
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        panic!("on_new_span");
        // Lookup up the parents spans, see if an ancestor has the
        // extension already. If so, add the extension for this span
        // too.
        let span_ref = ctx.span(id).unwrap();
        if let Some(parent_span) = span_ref.parent() {
            if parent_span.extensions().get::<SpanExtDisable>().is_some() {
                span_ref.extensions_mut().insert(SpanExtDisable);
                return;
            }
        }

        // If the parent wasn't disabled or if there was no parent,
        // check the fields
        for (filtered_field, filtered_value) in self.filters.iter() {
            if value_in_valueset(attrs.values(), filtered_field, filtered_value) {
                span_ref.extensions_mut().insert(SpanExtDisable);
                return;
            }
        }
    }
}

fn handle_tcp_client<S>(mut stream: TcpStream, layer_handle: Handle<DynamicFieldFilter, S>) {
    loop {
        let mut read_buf = [0_u8; 1024];
        match stream.read(&mut read_buf[..]) {
            Ok(n) => {
                let s = String::from_utf8_lossy(&read_buf[..n]);
                let mut words = s.split_whitespace();
                match words.next() {
                    Some("CLEAR") => {
                        layer_handle.modify(|layer| layer.filters.clear()).unwrap();
                    }
                    // Filter on vrf_id=id
                    Some("VRF") => {
                        if let Some(id) = words.next() {
                            layer_handle
                                .modify(|layer| {
                                    error!("setting filter for vrf_id = {id}");
                                    layer.filters.insert("vrf_id".to_string(), id.to_string());
                                })
                                .unwrap();
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => {
                warn!("TCP connection closed ({e})");
                return;
            }
        }
    }
}
