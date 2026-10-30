//! SSDP (UPnP discovery): answers `M-SEARCH` requests and announces the media server
//! with `NOTIFY` on the multicast group 239.255.255.250:1900.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::Notify;

pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
pub const SSDP_PORT: u16 = 1900;
/// How long announcements are valid (`CACHE-CONTROL: max-age`).
pub const MAX_AGE_SECS: u64 = 1800;
/// Re-announce well before the announcements expire.
const NOTIFY_INTERVAL: Duration = Duration::from_secs(600);

pub const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaServer:1";
pub const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
pub const CONNECTION_MANAGER: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";

/// A parsed `M-SEARCH` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    /// Search target (`ssdp:all`, `upnp:rootdevice`, a device/service type or a uuid).
    pub st: String,
    /// Maximum wait in seconds the searcher allows (1–5).
    pub mx: u64,
}

/// Parses an SSDP datagram; `None` for anything but a well-formed `M-SEARCH` with
/// `MAN: "ssdp:discover"`.
pub fn parse_search(datagram: &[u8]) -> Option<Search> {
    let text = std::str::from_utf8(datagram).ok()?;
    let mut lines = text.split("\r\n").flat_map(|l| l.split('\n'));
    let request_line = lines.next()?.trim();
    if !request_line.eq_ignore_ascii_case("M-SEARCH * HTTP/1.1") {
        return None;
    }
    let (mut st, mut mx, mut man) = (None, None, None);
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_uppercase().as_str() {
            "ST" => st = Some(value.to_owned()),
            "MX" => mx = value.parse::<u64>().ok(),
            "MAN" => man = Some(value.trim_matches('"').to_owned()),
            _ => {}
        }
    }
    if man.as_deref() != Some("ssdp:discover") {
        return None;
    }
    Some(Search {
        st: st.filter(|s| !s.is_empty())?,
        mx: mx.unwrap_or(1).clamp(1, 5),
    })
}

/// The device as SSDP describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// `uuid:…` without the prefix.
    pub uuid: String,
    /// Port of the description and control HTTP server.
    pub http_port: u16,
    pub server: String,
}

impl Device {
    /// (NT/ST, USN) pairs of everything this device offers.
    pub fn targets(&self) -> Vec<(String, String)> {
        let uuid = format!("uuid:{}", self.uuid);
        let mut out = vec![
            (
                "upnp:rootdevice".to_owned(),
                format!("{uuid}::upnp:rootdevice"),
            ),
            (uuid.clone(), uuid.clone()),
        ];
        for target in [DEVICE_TYPE, CONTENT_DIRECTORY, CONNECTION_MANAGER] {
            out.push((target.to_owned(), format!("{uuid}::{target}")));
        }
        out
    }

    /// The targets a search asks for.
    pub fn matching(&self, st: &str) -> Vec<(String, String)> {
        self.targets()
            .into_iter()
            .filter(|(target, _)| st == "ssdp:all" || target.eq_ignore_ascii_case(st))
            .collect()
    }

    pub fn location(&self, ip: IpAddr) -> String {
        format!(
            "http://{}/description.xml",
            SocketAddr::new(ip, self.http_port)
        )
    }

    /// Unicast answer to a search.
    pub fn response(&self, st: &str, usn: &str, ip: IpAddr) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age={MAX_AGE_SECS}\r\nEXT:\r\nLOCATION: {}\r\nSERVER: {}\r\nST: {st}\r\nUSN: {usn}\r\nContent-Length: 0\r\n\r\n",
            self.location(ip),
            self.server
        )
    }

    /// Multicast announcement (`ssdp:alive`) or farewell (`ssdp:byebye`).
    pub fn notify(&self, nt: &str, usn: &str, ip: IpAddr, alive: bool) -> String {
        if alive {
            format!(
                "NOTIFY * HTTP/1.1\r\nHOST: {MULTICAST_ADDR}:{SSDP_PORT}\r\nCACHE-CONTROL: max-age={MAX_AGE_SECS}\r\nLOCATION: {}\r\nNT: {nt}\r\nNTS: ssdp:alive\r\nSERVER: {}\r\nUSN: {usn}\r\n\r\n",
                self.location(ip),
                self.server
            )
        } else {
            format!(
                "NOTIFY * HTTP/1.1\r\nHOST: {MULTICAST_ADDR}:{SSDP_PORT}\r\nNT: {nt}\r\nNTS: ssdp:byebye\r\nUSN: {usn}\r\n\r\n"
            )
        }
    }
}

/// The local address that reaches `peer` (the interface a reply leaves through).
pub fn local_ip_towards(peer: SocketAddr) -> Option<IpAddr> {
    let probe = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    probe.connect(peer).ok()?;
    let ip = probe.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// The LAN address announcements carry (the default route's interface).
pub fn lan_ip() -> IpAddr {
    local_ip_towards(SocketAddr::V4(SocketAddrV4::new(MULTICAST_ADDR, SSDP_PORT)))
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// Binds the shared SSDP port (other UPnP software on the host keeps working) and
/// joins the multicast group.
pub fn bind_multicast(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    socket.join_multicast_v4(&MULTICAST_ADDR, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(4)?;
    UdpSocket::from_std(socket.into())
}

/// Answers searches on `socket` until `stop` fires. With `announce_to` it also
/// announces the device there (at start, every [`NOTIFY_INTERVAL`]) and says goodbye
/// when stopping.
pub async fn serve(
    socket: UdpSocket,
    device: Device,
    announce_to: Option<SocketAddr>,
    stop: Arc<Notify>,
) {
    let mut buf = vec![0u8; 2048];
    let mut notify = tokio::time::interval(NOTIFY_INTERVAL);
    let announce = |alive: bool| {
        let device = device.clone();
        let socket = &socket;
        async move {
            let Some(target) = announce_to else { return };
            let ip = lan_ip();
            for (nt, usn) in device.targets() {
                let message = device.notify(&nt, &usn, ip, alive);
                if let Err(err) = socket.send_to(message.as_bytes(), target).await {
                    tracing::debug!(%err, "SSDP announcement failed");
                }
            }
        }
    };
    loop {
        tokio::select! {
            _ = notify.tick() => announce(true).await,
            received = socket.recv_from(&mut buf) => {
                let Ok((len, peer)) = received else { continue };
                let Some(search) = parse_search(&buf[..len]) else { continue };
                let matches = device.matching(&search.st);
                if matches.is_empty() {
                    continue;
                }
                let ip = local_ip_towards(peer).unwrap_or_else(lan_ip);
                // Spread answers a little over the allowed wait (at most 0.5 s here).
                let jitter = Duration::from_millis(u64::from(flixnet_db::now_ms().unsigned_abs() as u32 % 500) % (search.mx * 1000));
                tokio::time::sleep(jitter).await;
                for (st, usn) in matches {
                    let response = device.response(&st, &usn, ip);
                    if let Err(err) = socket.send_to(response.as_bytes(), peer).await {
                        tracing::debug!(%err, %peer, "SSDP answer failed");
                    }
                }
            }
            () = stop.notified() => {
                announce(false).await;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Device {
        Device {
            uuid: "4d696e69-444c-164e-9d41-b827eb000001".into(),
            http_port: 8099,
            server: "Linux/6 UPnP/1.0 Flixnet/0.1".into(),
        }
    }

    #[test]
    fn searches_are_parsed() {
        let request = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 3\r\nST: urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n";
        assert_eq!(
            parse_search(request),
            Some(Search {
                st: DEVICE_TYPE.into(),
                mx: 3
            })
        );
        // Lenient about case, line endings and a missing/oversized MX.
        let request = b"m-search * http/1.1\nhost:239.255.255.250:1900\nman:\"ssdp:discover\"\nst:ssdp:all\nmx:120\n\n";
        assert_eq!(
            parse_search(request),
            Some(Search {
                st: "ssdp:all".into(),
                mx: 5
            })
        );
        // Not searches.
        assert_eq!(
            parse_search(b"NOTIFY * HTTP/1.1\r\nNT: upnp:rootdevice\r\n\r\n"),
            None
        );
        assert_eq!(
            parse_search(b"M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\n\r\n"),
            None,
            "no MAN"
        );
        assert_eq!(
            parse_search(b"M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:discover\"\r\n\r\n"),
            None,
            "no ST"
        );
        assert_eq!(parse_search(&[0xff, 0xfe]), None);
    }

    #[test]
    fn searches_match_the_offered_targets() {
        let d = device();
        assert_eq!(d.matching("ssdp:all").len(), 5);
        assert_eq!(
            d.matching("upnp:rootdevice"),
            [(
                "upnp:rootdevice".to_owned(),
                "uuid:4d696e69-444c-164e-9d41-b827eb000001::upnp:rootdevice".to_owned()
            )]
        );
        assert_eq!(d.matching(CONTENT_DIRECTORY).len(), 1);
        assert_eq!(
            d.matching("uuid:4d696e69-444c-164e-9d41-b827eb000001")
                .len(),
            1
        );
        assert!(
            d.matching("urn:schemas-upnp-org:device:MediaRenderer:1")
                .is_empty()
        );
    }

    #[test]
    fn answers_and_announcements() {
        let d = device();
        let ip: IpAddr = "192.168.188.129".parse().unwrap();
        let response = d.response(DEVICE_TYPE, "uuid:x::y", ip);
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("\r\nLOCATION: http://192.168.188.129:8099/description.xml\r\n"));
        assert!(response.contains("\r\nST: urn:schemas-upnp-org:device:MediaServer:1\r\n"));
        assert!(response.contains("\r\nEXT:\r\n"));
        assert!(response.ends_with("\r\n\r\n"));
        let alive = d.notify("upnp:rootdevice", "uuid:x::upnp:rootdevice", ip, true);
        assert!(alive.starts_with("NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n"));
        assert!(alive.contains("NTS: ssdp:alive"));
        let bye = d.notify("upnp:rootdevice", "uuid:x::upnp:rootdevice", ip, false);
        assert!(bye.contains("NTS: ssdp:byebye") && !bye.contains("LOCATION"));
    }

    /// The responder over a real socket (unicast on loopback, so it works anywhere).
    #[tokio::test]
    async fn m_search_gets_an_answer() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = server.local_addr().unwrap();
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(serve(server, device(), None, stop.clone()));

        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let search = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n";
        client.send_to(search.as_bytes(), address).await.unwrap();
        let mut buf = [0u8; 2048];
        let (len, _) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
            .await
            .expect("an answer within 5 s")
            .unwrap();
        let answer = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
        assert!(
            answer.contains("LOCATION: http://127.0.0.1:8099/description.xml"),
            "{answer}"
        );
        assert!(answer.contains("USN: uuid:4d696e69-444c-164e-9d41-b827eb000001::urn:schemas-upnp-org:device:MediaServer:1"));

        // Searches for something else stay unanswered.
        let other = search.replace("MediaServer", "MediaRenderer");
        client.send_to(other.as_bytes(), address).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(800), client.recv_from(&mut buf))
                .await
                .is_err()
        );
        stop.notify_one();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
    }

    /// Over multicast, where the network allows it (CI runners often do not): skipped
    /// with a note when joining or looping back multicast does not work.
    #[tokio::test]
    async fn m_search_over_multicast_when_available() {
        // A private port, so real UPnP devices on the network are not involved.
        let port = 41_900 + (std::process::id() % 1000) as u16;
        let Ok(server) = bind_multicast(port) else {
            eprintln!("multicast unavailable: skipped");
            return;
        };
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(serve(server, device(), None, stop.clone()));
        let client = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let _ = client.set_multicast_loop_v4(true);
        let search = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: upnp:rootdevice\r\n\r\n";
        if client
            .send_to(search.as_bytes(), (MULTICAST_ADDR, port))
            .await
            .is_err()
        {
            eprintln!("multicast send failed: skipped");
            stop.notify_one();
            return;
        }
        let mut buf = [0u8; 2048];
        match tokio::time::timeout(Duration::from_secs(3), client.recv_from(&mut buf)).await {
            Ok(Ok((len, _))) => {
                let answer = std::str::from_utf8(&buf[..len]).unwrap();
                assert!(answer.contains("ST: upnp:rootdevice"), "{answer}");
            }
            _ => eprintln!("no multicast loopback on this machine: skipped"),
        }
        stop.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }
}
