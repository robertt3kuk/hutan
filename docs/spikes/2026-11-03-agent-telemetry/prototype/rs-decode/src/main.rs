use opentelemetry_proto::tonic::collector::{logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest, trace::v1::ExportTraceServiceRequest};
use prost::Message;
use std::time::Instant;
fn main() {
    for arg in std::env::args().skip(1) {
        let (kind, p) = arg.split_once(':').unwrap();
        let raw = std::fs::read(p).unwrap();
        let t = Instant::now();
        let n = 200;
        let mut items = 0usize;
        for _ in 0..n {
            items = match kind {
                "traces" => ExportTraceServiceRequest::decode(raw.as_slice()).unwrap().resource_spans.iter().flat_map(|r| &r.scope_spans).map(|s| s.spans.len()).sum(),
                "logs" => ExportLogsServiceRequest::decode(raw.as_slice()).unwrap().resource_logs.iter().flat_map(|r| &r.scope_logs).map(|s| s.log_records.len()).sum(),
                _ => ExportMetricsServiceRequest::decode(raw.as_slice()).unwrap().resource_metrics.iter().flat_map(|r| &r.scope_metrics).map(|s| s.metrics.len()).sum(),
            };
        }
        let us = t.elapsed().as_micros() as f64 / n as f64;
        println!("{p}: {kind} {} bytes, {items} items, {us:.0} us per decode", raw.len());
    }
}
