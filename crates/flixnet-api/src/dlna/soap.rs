//! SOAP for UPnP control requests: the action and its arguments in, envelopes out.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::Event;

use super::didl::escape;

/// A UPnP action call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// e.g. `Browse`.
    pub name: String,
    /// Argument name → text.
    pub args: HashMap<String, String>,
}

impl Action {
    pub fn arg(&self, name: &str) -> Option<&str> {
        self.args.get(name).map(String::as_str)
    }
}

/// `amp`, `lt`, … and `#38` / `#x26` → the character.
fn entity(name: &str) -> Option<String> {
    Some(match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        _ => {
            let code = name.strip_prefix('#')?;
            let value = match code.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => code.parse().ok()?,
            };
            char::from_u32(value)?.to_string()
        }
    })
}

fn local_name(raw: &str) -> String {
    raw.rsplit(':').next().unwrap_or_default().to_owned()
}

/// Parses `<s:Envelope><s:Body><u:Action>…args…</u:Action></s:Body></s:Envelope>`.
pub fn parse_action(body: &str) -> Option<Action> {
    let mut reader = Reader::from_str(body);
    // Depth below Body: 1 = the action element, 2 = its arguments.
    let mut in_body = false;
    let mut depth = 0usize;
    let mut action: Option<String> = None;
    let mut current_arg: Option<String> = None;
    let mut args = HashMap::new();
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => {
                let name = local_name(e.name().as_ref());
                if !in_body {
                    in_body = name == "Body";
                    continue;
                }
                depth += 1;
                match depth {
                    1 => action = Some(name),
                    2 => {
                        current_arg = Some(name.clone());
                        args.entry(name).or_insert_with(String::new);
                    }
                    _ => {}
                }
            }
            Event::Empty(e) if in_body => {
                let name = local_name(e.name().as_ref());
                match depth {
                    0 => action = Some(name),
                    1 => {
                        args.insert(name, String::new());
                    }
                    _ => {}
                }
            }
            Event::Text(t) if depth == 2 => {
                if let Some(arg) = &current_arg {
                    args.entry(arg.clone())
                        .or_default()
                        .push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) if depth == 2 => {
                if let (Some(arg), Some(resolved)) = (&current_arg, entity(&r.xml10_content())) {
                    args.entry(arg.clone()).or_default().push_str(&resolved);
                }
            }
            Event::CData(t) if depth == 2 => {
                if let Some(arg) = &current_arg {
                    args.entry(arg.clone())
                        .or_default()
                        .push_str(&t.xml10_content());
                }
            }
            Event::End(e) => {
                let name = local_name(e.name().as_ref());
                if !in_body {
                    continue;
                }
                if depth == 0 {
                    if name == "Body" {
                        break;
                    }
                    continue;
                }
                if depth == 2 {
                    current_arg = None;
                }
                depth -= 1;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Some(Action {
        name: action?,
        args,
    })
}

/// A successful answer: `<u:{action}Response>` with the given output arguments
/// (escaped here).
pub fn response(service: &str, action: &str, outputs: &[(&str, String)]) -> String {
    let mut args = String::new();
    for (name, value) in outputs {
        args.push_str(&format!("<{name}>{}</{name}>", escape(value)));
    }
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action}Response xmlns:u="{service}">{args}</u:{action}Response></s:Body></s:Envelope>"#
    )
}

/// A UPnP error (e.g. 401 Invalid Action, 402 Invalid Args, 701 No such object).
pub fn fault(code: u16, description: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>{code}</errorCode><errorDescription>{}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"#,
        escape(description)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_requests_are_parsed() {
        let body = r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
      <ObjectID>lib:1</ObjectID>
      <BrowseFlag>BrowseDirectChildren</BrowseFlag>
      <Filter>*</Filter>
      <StartingIndex>0</StartingIndex>
      <RequestedCount>50</RequestedCount>
      <SortCriteria></SortCriteria>
      <Escaped>a &amp; b &#x3C;</Escaped>
    </u:Browse>
  </s:Body>
</s:Envelope>"#;
        let action = parse_action(body).unwrap();
        assert_eq!(action.name, "Browse");
        assert_eq!(action.arg("ObjectID"), Some("lib:1"));
        assert_eq!(action.arg("BrowseFlag"), Some("BrowseDirectChildren"));
        assert_eq!(action.arg("RequestedCount"), Some("50"));
        assert_eq!(action.arg("SortCriteria"), Some(""));
        assert_eq!(action.arg("Escaped"), Some("a & b <"));

        let empty = r#"<s:Envelope xmlns:s="x"><s:Body><u:GetSystemUpdateID xmlns:u="y"/></s:Body></s:Envelope>"#;
        assert_eq!(parse_action(empty).unwrap().name, "GetSystemUpdateID");
        assert!(parse_action("<kein xml").is_none());
        assert!(parse_action("<a><b/></a>").is_none(), "no Body");
    }

    #[test]
    fn envelopes() {
        let ok = response(
            "urn:schemas-upnp-org:service:ContentDirectory:1",
            "Browse",
            &[
                ("Result", "<DIDL-Lite/>".into()),
                ("NumberReturned", "0".into()),
            ],
        );
        assert!(ok.contains(r#"<u:BrowseResponse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><Result>&lt;DIDL-Lite/&gt;</Result><NumberReturned>0</NumberReturned></u:BrowseResponse>"#));
        let parsed = parse_action(&ok).unwrap();
        assert_eq!(parsed.name, "BrowseResponse");
        assert_eq!(parsed.arg("Result"), Some("<DIDL-Lite/>"));
        let err = fault(701, "No such object");
        assert!(err.contains("<errorCode>701</errorCode>"));
    }
}
