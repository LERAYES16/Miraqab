//! A minimal X11 client speaking the raw wire protocol over the local socket.
//!
//! Why: listing windows, reading their size and finding monitors must not
//! depend on `xwininfo`/`xdotool`/`wmctrl` being installed, nor on linking C
//! libraries. Only the handful of requests Miraqab needs are implemented:
//! InternAtom, GetProperty, GetGeometry, TranslateCoordinates, GetWindowAttributes,
//! QueryTree, GetAtomName, QueryExtension and the RandR 1.5 monitor list.
//!
//! Pixels are NOT read here. Frames are captured by FFmpeg's `x11grab`.

use crate::geometry::{Rect, Size};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XErr {
    /// The window/drawable no longer exists.
    BadWindow,
    Other(String),
}

impl std::fmt::Display for XErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XErr::BadWindow => write!(f, "BadWindow"),
            XErr::Other(s) => write!(f, "{}", s),
        }
    }
}

impl From<std::io::Error> for XErr {
    fn from(e: std::io::Error) -> XErr {
        XErr::Other(e.to_string())
    }
}

pub type Res<T> = Result<T, XErr>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WinInfo {
    pub id: u32,
    pub title: String,
    pub class: String,
    pub rect: Rect,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorRaw {
    pub name: String,
    pub primary: bool,
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WinState {
    Gone,
    /// Unmapped or iconified: the window exists but there is nothing to capture right now.
    Hidden,
    Visible(Rect),
}

// ---- pure helpers (unit-tested) ------------------------------------------

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_i16(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// `:0`, `:0.1`, `unix:0` -> (host, display, screen). Remote hosts are returned as-is.
pub fn parse_display(d: &str) -> Option<(String, u32, u32)> {
    let (host, rest) = d.rsplit_once(':')?;
    let (num, scr) = rest.split_once('.').unwrap_or((rest, "0"));
    Some((host.to_string(), num.parse().ok()?, scr.parse().ok()?))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthEntry {
    pub family: u16,
    pub addr: Vec<u8>,
    pub number: String,
    pub name: String,
    pub data: Vec<u8>,
}

/// Parse the `.Xauthority` file format (big-endian counted strings).
pub fn parse_xauthority(b: &[u8]) -> Vec<AuthEntry> {
    fn take<'a>(b: &'a [u8], i: &mut usize) -> Option<&'a [u8]> {
        let n = u16::from_be_bytes([*b.get(*i)?, *b.get(*i + 1)?]) as usize;
        let s = b.get(*i + 2..*i + 2 + n)?;
        *i += 2 + n;
        Some(s)
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 <= b.len() {
        let family = u16::from_be_bytes([b[i], b[i + 1]]);
        i += 2;
        let (Some(addr), Some(num), Some(name), Some(data)) =
            (take(b, &mut i), take(b, &mut i), take(b, &mut i), take(b, &mut i))
        else {
            break;
        };
        out.push(AuthEntry {
            family,
            addr: addr.to_vec(),
            number: String::from_utf8_lossy(num).into_owned(),
            name: String::from_utf8_lossy(name).into_owned(),
            data: data.to_vec(),
        });
    }
    out
}

/// Choose the MIT-MAGIC-COOKIE-1 entry for this host and display number.
pub fn pick_cookie(entries: &[AuthEntry], hostname: &str, display: u32) -> Option<(String, Vec<u8>)> {
    const FAMILY_LOCAL: u16 = 256;
    const FAMILY_WILD: u16 = 65535;
    let num = display.to_string();
    entries
        .iter()
        .filter(|e| e.name == "MIT-MAGIC-COOKIE-1" && (e.number.is_empty() || e.number == num))
        .find(|e| e.family == FAMILY_WILD || (e.family == FAMILY_LOCAL && e.addr == hostname.as_bytes()))
        .map(|e| (e.name.clone(), e.data.clone()))
}

/// Encode a request: opcode, data byte, 16-bit length in 4-byte units, body, zero padding.
pub fn encode_request(opcode: u8, data: u8, body: &[u8]) -> Vec<u8> {
    let padded = (4 + body.len() + 3) & !3;
    let mut v = Vec::with_capacity(padded);
    v.push(opcode);
    v.push(data);
    v.extend_from_slice(&((padded / 4) as u16).to_le_bytes());
    v.extend_from_slice(body);
    v.resize(padded, 0);
    v
}

/// Decode the RandR 1.5 `GetMonitors` reply into (name atom, primary, rect) triples.
pub fn decode_monitors(r: &[u8]) -> Vec<(u32, bool, Rect)> {
    let mut out = Vec::new();
    if r.len() < 32 {
        return out;
    }
    let n = rd_u32(r, 12) as usize;
    let mut o = 32;
    for _ in 0..n {
        if o + 24 > r.len() {
            break;
        }
        let atom = rd_u32(r, o);
        let primary = r[o + 4] != 0;
        let nout = rd_u16(r, o + 6) as usize;
        let rect = Rect {
            x: rd_i16(r, o + 8) as i32,
            y: rd_i16(r, o + 10) as i32,
            w: rd_u16(r, o + 12) as u32,
            h: rd_u16(r, o + 14) as u32,
        };
        out.push((atom, primary, rect));
        o += 24 + 4 * nout;
    }
    out
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

// ---- connection -----------------------------------------------------------

pub struct X11 {
    stream: UnixStream,
    seq: u16,
    pub root: u32,
    pub screen: Size,
    atoms: std::collections::HashMap<String, u32>,
}

impl X11 {
    /// Connect to `$DISPLAY` (local displays only) using `$XAUTHORITY` / `~/.Xauthority`.
    pub fn connect() -> Res<X11> {
        let d = std::env::var("DISPLAY").map_err(|_| XErr::Other("DISPLAY is not set".into()))?;
        X11::connect_to(&d)
    }

    /// Connect to an explicit display string such as `:0` (no environment access,
    /// so it is safe to use from parallel tests).
    pub fn connect_to(d: &str) -> Res<X11> {
        let (host, num, _screen) = parse_display(d).ok_or_else(|| XErr::Other(format!("cannot parse DISPLAY={}", d)))?;
        if !host.is_empty() && host != "unix" {
            return Err(XErr::Other(format!("remote X displays are not supported ({})", d)));
        }
        let path = format!("/tmp/.X11-unix/X{}", num);
        let stream = match UnixStream::connect(&path) {
            Ok(s) => s,
            Err(e1) => {
                use std::os::linux::net::SocketAddrExt;
                use std::os::unix::net::SocketAddr;
                let abs = SocketAddr::from_abstract_name(path.as_bytes()).map_err(|e| XErr::Other(e.to_string()))?;
                UnixStream::connect_addr(&abs).map_err(|e2| XErr::Other(format!("cannot connect to {}: {} / {}", path, e1, e2)))?
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        let mut x = X11 { stream, seq: 0, root: 0, screen: Size::default(), atoms: Default::default() };
        let (aname, adata) = Self::find_cookie(num).unwrap_or_default();
        x.handshake(&aname, &adata)?;
        Ok(x)
    }

    fn find_cookie(display: u32) -> Option<(String, Vec<u8>)> {
        let path = std::env::var_os("XAUTHORITY")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".Xauthority")))?;
        let bytes = std::fs::read(path).ok()?;
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
        pick_cookie(&parse_xauthority(&bytes), host.trim(), display)
    }

    fn handshake(&mut self, aname: &str, adata: &[u8]) -> Res<()> {
        let pad = |n: usize| (n + 3) & !3;
        let mut req = vec![b'l', 0, 11, 0, 0, 0];
        req.extend_from_slice(&(aname.len() as u16).to_le_bytes());
        req.extend_from_slice(&(adata.len() as u16).to_le_bytes());
        req.extend_from_slice(&[0, 0]);
        req.extend_from_slice(aname.as_bytes());
        req.resize(12 + pad(aname.len()), 0);
        req.extend_from_slice(adata);
        let want = 12 + pad(aname.len()) + pad(adata.len());
        req.resize(want, 0);
        self.stream.write_all(&req)?;

        let mut head = [0u8; 8];
        self.stream.read_exact(&mut head)?;
        let mut body = vec![0u8; rd_u16(&head, 6) as usize * 4];
        self.stream.read_exact(&mut body)?;
        if head[0] != 1 {
            let n = head[1] as usize;
            let why = String::from_utf8_lossy(body.get(..n).unwrap_or(&[])).into_owned();
            return Err(XErr::Other(format!("X server refused the connection: {}", why.trim())));
        }
        // `body` starts at offset 8 of the full reply; offsets below are relative to it.
        let vendor_len = rd_u16(&body, 16) as usize;
        let nroots = body[20];
        let nformats = body[21] as usize;
        let scr = 32 + pad(vendor_len) + nformats * 8;
        if nroots == 0 || body.len() < scr + 24 {
            return Err(XErr::Other("malformed X server handshake".into()));
        }
        self.root = rd_u32(&body, scr);
        self.screen = Size::new(rd_u16(&body, scr + 20) as u32, rd_u16(&body, scr + 22) as u32);
        Ok(())
    }

    fn send(&mut self, opcode: u8, data: u8, body: &[u8]) -> Res<()> {
        self.stream.write_all(&encode_request(opcode, data, body))?;
        self.seq = self.seq.wrapping_add(1);
        Ok(())
    }

    fn reply(&mut self) -> Res<Vec<u8>> {
        loop {
            let mut h = [0u8; 32];
            self.stream.read_exact(&mut h)?;
            match h[0] {
                0 => {
                    if rd_u16(&h, 2) == self.seq {
                        // 3 = BadWindow, 9 = BadDrawable
                        return Err(if h[1] == 3 || h[1] == 9 { XErr::BadWindow } else { XErr::Other(format!("X error {}", h[1])) });
                    }
                }
                1 => {
                    let extra = rd_u32(&h, 4) as usize * 4;
                    let mut v = h.to_vec();
                    if extra > 0 {
                        v.resize(32 + extra, 0);
                        self.stream.read_exact(&mut v[32..])?;
                    }
                    if rd_u16(&h, 2) == self.seq {
                        return Ok(v);
                    }
                }
                _ => {} // events are not selected; ignore anything unexpected
            }
        }
    }

    fn call(&mut self, opcode: u8, data: u8, body: &[u8]) -> Res<Vec<u8>> {
        self.send(opcode, data, body)?;
        self.reply()
    }

    // ---- requests ----------------------------------------------------------

    pub fn intern_atom(&mut self, name: &str) -> Res<u32> {
        if let Some(a) = self.atoms.get(name) {
            return Ok(*a);
        }
        let mut body = (name.len() as u16).to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(name.as_bytes());
        let r = self.call(16, 0, &body)?;
        let a = rd_u32(&r, 8);
        self.atoms.insert(name.to_string(), a);
        Ok(a)
    }

    pub fn atom_name(&mut self, atom: u32) -> Res<String> {
        let r = self.call(17, 0, &atom.to_le_bytes())?;
        let n = rd_u16(&r, 8) as usize;
        Ok(String::from_utf8_lossy(r.get(32..32 + n).unwrap_or(&[])).into_owned())
    }

    /// Returns (type atom, format, value bytes) or `None` if the property is not set.
    pub fn get_property(&mut self, win: u32, prop: u32, ptype: u32, max_words: u32) -> Res<Option<(u32, u8, Vec<u8>)>> {
        let mut b = Vec::new();
        for v in [win, prop, ptype, 0, max_words] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let r = self.call(20, 0, &b)?;
        let ty = rd_u32(&r, 8);
        if ty == 0 {
            return Ok(None);
        }
        let fmt = r[1];
        let n = rd_u32(&r, 16) as usize * (fmt as usize / 8);
        Ok(Some((ty, fmt, r.get(32..32 + n).unwrap_or(&[]).to_vec())))
    }

    /// Position is relative to the parent window (usually the window-manager frame).
    pub fn get_geometry(&mut self, d: u32) -> Res<Rect> {
        let r = self.call(14, 0, &d.to_le_bytes())?;
        Ok(Rect { x: rd_i16(&r, 12) as i32, y: rd_i16(&r, 14) as i32, w: rd_u16(&r, 16) as u32, h: rd_u16(&r, 18) as u32 })
    }

    pub fn translate(&mut self, src: u32, dst: u32, x: i16, y: i16) -> Res<(i32, i32)> {
        let mut b = Vec::new();
        b.extend_from_slice(&src.to_le_bytes());
        b.extend_from_slice(&dst.to_le_bytes());
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&y.to_le_bytes());
        let r = self.call(40, 0, &b)?;
        Ok((rd_i16(&r, 12) as i32, rd_i16(&r, 14) as i32))
    }

    /// 0 = unmapped, 1 = unviewable, 2 = viewable.
    pub fn map_state(&mut self, win: u32) -> Res<u8> {
        let r = self.call(3, 0, &win.to_le_bytes())?;
        Ok(r[26])
    }

    pub fn query_tree(&mut self, win: u32) -> Res<Vec<u32>> {
        let r = self.call(15, 0, &win.to_le_bytes())?;
        let n = rd_u16(&r, 16) as usize;
        Ok((0..n).filter_map(|i| r.get(32 + i * 4..36 + i * 4).map(|s| rd_u32(s, 0))).collect())
    }

    pub fn query_extension(&mut self, name: &str) -> Res<Option<u8>> {
        let mut body = (name.len() as u16).to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(name.as_bytes());
        let r = self.call(98, 0, &body)?;
        Ok(if r[8] != 0 { Some(r[9]) } else { None })
    }

    // ---- higher level ------------------------------------------------------

    pub fn window_title(&mut self, win: u32) -> Res<String> {
        let net = self.intern_atom("_NET_WM_NAME")?;
        let utf8 = self.intern_atom("UTF8_STRING")?;
        if let Some((_, _, v)) = self.get_property(win, net, utf8, 1024)? {
            if !v.is_empty() {
                return Ok(String::from_utf8_lossy(&v).into_owned());
            }
        }
        // WM_NAME (atom 39), type STRING (atom 31): Latin-1
        Ok(match self.get_property(win, 39, 31, 1024)? {
            Some((_, _, v)) => latin1(&v),
            None => String::new(),
        })
    }

    fn window_class(&mut self, win: u32) -> String {
        match self.get_property(win, 67, 31, 256) {
            Ok(Some((_, _, v))) => {
                let parts: Vec<&[u8]> = v.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
                parts.last().map(|p| latin1(p)).unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    fn is_hidden_by_wm(&mut self, win: u32) -> bool {
        let (Ok(state), Ok(hidden)) = (self.intern_atom("_NET_WM_STATE"), self.intern_atom("_NET_WM_STATE_HIDDEN")) else {
            return false;
        };
        match self.get_property(win, state, 4, 64) {
            Ok(Some((_, 32, v))) => v.chunks_exact(4).any(|c| rd_u32(c, 0) == hidden),
            _ => false,
        }
    }

    /// Absolute rectangle of the client area of `win` in root coordinates.
    pub fn absolute_rect(&mut self, win: u32) -> Res<Rect> {
        let g = self.get_geometry(win)?;
        let (x, y) = self.translate(win, self.root, 0, 0)?;
        Ok(Rect { x, y, w: g.w, h: g.h })
    }

    pub fn window_state(&mut self, win: u32) -> WinState {
        match self.absolute_rect(win) {
            Err(XErr::BadWindow) => WinState::Gone,
            Err(_) => WinState::Gone,
            Ok(r) => {
                let mapped = matches!(self.map_state(win), Ok(2));
                if !mapped || self.is_hidden_by_wm(win) || r.w < 2 || r.h < 2 {
                    WinState::Hidden
                } else {
                    WinState::Visible(r)
                }
            }
        }
    }

    pub fn root_size(&mut self) -> Res<Size> {
        let g = self.get_geometry(self.root)?;
        Ok(g.size())
    }

    /// Top-level application windows. Uses `_NET_CLIENT_LIST` (EWMH) and falls back
    /// to the root's children when the window manager does not provide it.
    pub fn list_windows(&mut self) -> Res<Vec<WinInfo>> {
        let list_atom = self.intern_atom("_NET_CLIENT_LIST")?;
        let root = self.root;
        let mut ids: Vec<u32> = match self.get_property(root, list_atom, 33, 4096)? {
            Some((_, 32, v)) => v.chunks_exact(4).map(|c| rd_u32(c, 0)).collect(),
            _ => Vec::new(),
        };
        let ewmh = !ids.is_empty();
        if !ewmh {
            ids = self.query_tree(root)?;
        }
        let mut out = Vec::new();
        for id in ids {
            let Ok(title) = self.window_title(id) else { continue };
            if title.is_empty() {
                continue;
            }
            let Ok(rect) = self.absolute_rect(id) else { continue };
            let mapped = matches!(self.map_state(id), Ok(2));
            let hidden = self.is_hidden_by_wm(id) || !mapped;
            if !ewmh && (hidden || rect.w < 2 || rect.h < 2) {
                continue; // without EWMH only list windows that are really on screen
            }
            let class = self.window_class(id);
            out.push(WinInfo { id, title, class, rect, hidden });
        }
        Ok(out)
    }

    /// Monitors via RandR 1.5; falls back to the whole screen as one monitor.
    pub fn monitors(&mut self) -> Res<Vec<MonitorRaw>> {
        let screen = self.screen;
        let whole = move || vec![MonitorRaw { name: "screen".into(), primary: true, rect: Rect { x: 0, y: 0, w: screen.w, h: screen.h } }];
        let Some(op) = self.query_extension("RANDR")? else { return Ok(whole()) };
        let mut ver = 1u32.to_le_bytes().to_vec();
        ver.extend_from_slice(&5u32.to_le_bytes());
        if self.call(op, 0, &ver).is_err() {
            return Ok(whole());
        }
        let mut body = self.root.to_le_bytes().to_vec();
        body.extend_from_slice(&[1, 0, 0, 0]); // get_active = true
        let Ok(r) = self.call(op, 42, &body) else { return Ok(whole()) };
        let mut out = Vec::new();
        for (atom, primary, rect) in decode_monitors(&r) {
            let name = self.atom_name(atom).unwrap_or_else(|_| format!("monitor{}", out.len() + 1));
            out.push(MonitorRaw { name, primary, rect });
        }
        Ok(if out.is_empty() { whole() } else { out })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_parsing() {
        assert_eq!(parse_display(":0"), Some((String::new(), 0, 0)));
        assert_eq!(parse_display(":1.2"), Some((String::new(), 1, 2)));
        assert_eq!(parse_display("unix:3"), Some(("unix".into(), 3, 0)));
        assert_eq!(parse_display("host:10.0").unwrap().0, "host");
        assert_eq!(parse_display("nonsense"), None);
    }

    fn auth_entry(family: u16, addr: &str, num: &str, name: &str, data: &[u8]) -> Vec<u8> {
        let mut v = family.to_be_bytes().to_vec();
        for s in [addr.as_bytes(), num.as_bytes(), name.as_bytes(), data] {
            v.extend_from_slice(&(s.len() as u16).to_be_bytes());
            v.extend_from_slice(s);
        }
        v
    }

    #[test]
    fn xauthority_parse_and_pick() {
        let mut file = auth_entry(256, "otherhost", "0", "MIT-MAGIC-COOKIE-1", &[9; 16]);
        file.extend(auth_entry(256, "myhost", "1", "MIT-MAGIC-COOKIE-1", &[2; 16]));
        file.extend(auth_entry(256, "myhost", "0", "MIT-MAGIC-COOKIE-1", &[7; 16]));
        let e = parse_xauthority(&file);
        assert_eq!(e.len(), 3);
        assert_eq!(pick_cookie(&e, "myhost", 0).unwrap().1, vec![7; 16]);
        assert_eq!(pick_cookie(&e, "myhost", 1).unwrap().1, vec![2; 16]);
        assert!(pick_cookie(&e, "myhost", 5).is_none());
        // wildcard family matches any host
        let w = parse_xauthority(&auth_entry(65535, "", "", "MIT-MAGIC-COOKIE-1", &[1; 16]));
        assert!(pick_cookie(&w, "whatever", 3).is_some());
        // truncated input does not panic
        assert!(parse_xauthority(&file[..file.len() - 5]).len() == 2);
    }

    #[test]
    fn request_padding_and_length() {
        let r = encode_request(16, 0, &[1, 2, 3]);
        assert_eq!(r.len(), 8);
        assert_eq!(u16::from_le_bytes([r[2], r[3]]), 2);
        assert_eq!(encode_request(14, 0, &[0; 4]).len(), 8);
        assert_eq!(encode_request(43, 0, &[]).len(), 4);
    }

    #[test]
    fn monitors_decode() {
        let mut r = vec![0u8; 32];
        r[0] = 1;
        r[12..16].copy_from_slice(&2u32.to_le_bytes());
        let mon = |atom: u32, primary: u8, nout: u16, x: i16, y: i16, w: u16, h: u16| {
            let mut m = Vec::new();
            m.extend_from_slice(&atom.to_le_bytes());
            m.extend_from_slice(&[primary, 1]);
            m.extend_from_slice(&nout.to_le_bytes());
            for v in [x, y] {
                m.extend_from_slice(&v.to_le_bytes());
            }
            for v in [w, h] {
                m.extend_from_slice(&v.to_le_bytes());
            }
            m.extend_from_slice(&[0; 8]);
            m.extend(std::iter::repeat(0u8).take(4 * nout as usize));
            m
        };
        r.extend(mon(100, 1, 1, 0, 0, 1920, 1080));
        r.extend(mon(101, 0, 2, 1920, -100, 1280, 1024));
        let d = decode_monitors(&r);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], (100, true, Rect { x: 0, y: 0, w: 1920, h: 1080 }));
        assert_eq!(d[1].2, Rect { x: 1920, y: -100, w: 1280, h: 1024 });
        assert!(decode_monitors(&r[..20]).is_empty());
    }
}
