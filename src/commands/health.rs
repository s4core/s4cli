//! `ping` and `ready` endpoint checks.

use std::time::Instant;

use super::{Context, target_arg};
use crate::output::JsonObject;
use crate::s3::Request;

pub fn ping(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, "usage: s4 ping <alias>")?;
    let start = Instant::now();
    client.send(&Request::new("GET", ""))?;
    let ms = start.elapsed().as_millis();
    ctx.report(
        JsonObject::new()
            .str("alias", &target.alias)
            .str("status", "ok")
            .raw("latency_ms", ms),
        format!("{} is alive ({ms} ms)", target.alias),
    );
    Ok(())
}

pub fn ready(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, "usage: s4 ready <alias>")?;
    let body = client.send(&Request::new("GET", ""))?;
    if !looks_ready_xml(&body) {
        return Err("ready check got unexpected response body".to_string());
    }
    ctx.report(
        JsonObject::new()
            .str("alias", &target.alias)
            .raw("ready", true),
        format!("{} is ready", target.alias),
    );
    Ok(())
}

fn looks_ready_xml(body: &str) -> bool {
    body.contains("<ListAllMyBucketsResult") || body.contains("<Error")
}

#[cfg(test)]
mod tests {
    use super::looks_ready_xml;

    #[test]
    fn looks_ready_xml_accepts_known_payloads() {
        assert!(looks_ready_xml(
            "<ListAllMyBucketsResult></ListAllMyBucketsResult>"
        ));
        assert!(looks_ready_xml("<Error><Code>AccessDenied</Code></Error>"));
        assert!(!looks_ready_xml("not-xml"));
    }
}
